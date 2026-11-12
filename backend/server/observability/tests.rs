use super::*;

/// Provider totals are independent per bucket and retain token availability.
#[test]
fn provider_bucket_totals_keep_known_values() {
    let rows = vec![
        (200, 1, None, Some(0), 1, "same".into(), "same".into()),
        (200, 1, Some(3), None, 2, "same".into(), "other".into()),
        (500, 1, None, None, 3, String::new(), "same".into()),
        (200, 1, None, Some(7), 3600, "same".into(), "same".into()),
    ];
    let (_, buckets, _) = aggregate_rows(&rows, Interval::Hour).unwrap();
    let buckets = serde_json::to_value(buckets).unwrap();
    assert_eq!(
        buckets[0]["by_provider"],
        serde_json::json!({
            "": {"requests": 1, "input_tokens": null, "output_tokens": null},
            "same": {"requests": 2, "input_tokens": 3, "output_tokens": 0}
        })
    );
    assert_eq!(
        buckets[1]["by_provider"],
        serde_json::json!({"same": {"requests": 1, "input_tokens": null, "output_tokens": 7}})
    );
    let (_, empty, _) = aggregate_rows(&[], Interval::Hour).unwrap();
    assert!(empty.is_empty());
}

/// Existing request identities and dimension options survive migration.
#[tokio::test]
async fn migrates_existing_log_filter_options() {
    let path =
        std::env::temp_dir().join(format!("zero-g-old-log-test-{}.db", uuid::Uuid::new_v4()));
    let mut db = toasty::Db::builder()
        .models(toasty::models!(
            crate::storage::RequestLog,
            crate::storage::Setting,
            crate::storage::Secret,
            crate::storage::VirtualKey,
            crate::storage::ProviderRecord,
            crate::storage::ProviderCredential,
            crate::storage::LogFilterOption
        ))
        .connect(&format!("turso:{}", path.display()))
        .await
        .unwrap();
    for migration in [
        include_str!("../../../toasty/migrations/0000_initial.sql"),
        include_str!("../../../toasty/migrations/0001_add_providers.sql"),
        include_str!("../../../toasty/migrations/0002_provider_credentials.sql"),
        include_str!("../../../toasty/migrations/0003_request_log_identities.sql"),
    ] {
        for statement in migration.split("-- #[toasty::breakpoint]") {
            toasty::sql::statement(statement.trim())
                .exec(&mut db)
                .await
                .unwrap();
        }
    }
    toasty::sql::statement("CREATE TABLE __toasty_migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)")
            .exec(&mut db).await.unwrap();
    for (id, name) in [
        (6_240_284_849_605_402_046_i64, "0000_initial.sql"),
        (9_146_868_781_853_628_900_i64, "0001_add_providers.sql"),
        (
            7_352_461_067_189_203_411_i64,
            "0002_provider_credentials.sql",
        ),
        (
            9_001_098_191_863_351_684_i64,
            "0003_request_log_identities.sql",
        ),
    ] {
        toasty::sql::statement("INSERT INTO __toasty_migrations VALUES (?1, ?2, '2026-01-01')")
            .bind(id)
            .bind(name)
            .exec(&mut db)
            .await
            .unwrap();
    }
    toasty::sql::statement("INSERT INTO request_logs (created_at, provider, model, path, status, latency_ms, request_body, response_body) VALUES (?1, 'p', 'm', '/v1', 200, 1, 'old request', 'old response')")
            .bind(now()).exec(&mut db).await.unwrap();
    toasty::sql::statement("INSERT INTO request_logs (created_at, provider, model, path, status, latency_ms, request_body, response_body, virtual_key_id, virtual_key_name) VALUES (?1, 'historical', 'old-model', '/old', 200, 1, '', '', 'old-key', 'Old key')")
            .bind(now()).exec(&mut db).await.unwrap();
    drop(db);
    let store = LogStore::new(Database::open(path.to_str().unwrap()).await.unwrap());
    let result =
        serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap()).unwrap();
    assert_eq!(result["summary"]["requests"], 2);
    assert!(result["logs"][1].get("request_body").is_none());
    let detail = store
        .dashboard(&DashboardQuery {
            log_id: result["logs"][1]["id"].as_i64(),
            logs_only: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(detail.logs[0].request_body.as_deref(), Some("old request"));
    for field in [
        "virtual_key_id",
        "virtual_key_name",
        "credential_id",
        "credential_name",
    ] {
        assert!(result["logs"][1][field].is_null());
    }
    let logs = serde_json::to_value(
        store
            .dashboard(&DashboardQuery {
                logs_only: Some(true),
                ..Default::default()
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(logs["filter_options"], result["filter_options"]);
    assert_eq!(
        logs["filter_options"]["virtual_keys"],
        serde_json::json!([{"id": "old-key", "name": "Old key"}])
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// Dashboard reports the active policy, not a pending settings change.
#[tokio::test]
async fn dashboard_retention_uses_active_store_policy() {
    let path =
        std::env::temp_dir().join(format!("zero-g-retention-test-{}.db", uuid::Uuid::new_v4()));
    let store = LogStore::new(Database::open(path.to_str().unwrap()).await.unwrap());
    store.set_retention(1).await.unwrap();
    transaction_with_retry!(store.database, |mut tx| {
        crate::storage::Setting::upsert_by_name("log_retention_days")
            .value("30".to_string())
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
    .unwrap();
    let before = now();
    let result = store.dashboard(&DashboardQuery::default()).await.unwrap();
    let after = now();
    assert!(result.retention_cutoff >= before.saturating_sub(86400));
    assert!(result.retention_cutoff <= after.saturating_sub(86400));
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// Expiration removes only snapshots whose last matching request was pruned.
#[tokio::test]
async fn retained_filter_options_follow_renames_and_pruning() {
    let path =
        std::env::temp_dir().join(format!("zero-g-options-test-{}.db", uuid::Uuid::new_v4()));
    let store = LogStore::new(Database::open(path.to_str().unwrap()).await.unwrap());
    store.set_retention(30).await.unwrap();
    let entry = |days: i64, name: &str, provider: &str| LogEntry {
        id: 0,
        created_at: now().saturating_sub(days.saturating_mul(86400)),
        provider: provider.into(),
        model: "model".into(),
        path: "/v1".into(),
        status: 200,
        latency_ms: 1,
        input_tokens: None,
        output_tokens: None,
        request_body: String::new(),
        response_body: String::new(),
        virtual_key_id: Some("key".into()),
        virtual_key_name: Some(name.into()),
        credential_id: None,
        credential_name: None,
    };
    store.insert(entry(5, "Z name", "active")).await.unwrap();
    store.insert(entry(20, "A name", "expired")).await.unwrap();
    store.insert(entry(25, "Z name", "active")).await.unwrap();
    let query = DashboardQuery {
        logs_only: Some(true),
        ..Default::default()
    };
    let before = serde_json::to_value(store.dashboard(&query).await.unwrap()).unwrap();
    assert_eq!(
        before["filter_options"]["virtual_keys"],
        serde_json::json!([{"id":"key","name":"A name"}])
    );
    store.set_retention(10).await.unwrap();
    let after = serde_json::to_value(store.dashboard(&query).await.unwrap()).unwrap();
    assert_eq!(after["logs"].as_array().unwrap().len(), 1);
    assert_eq!(
        after["filter_options"]["providers"],
        serde_json::json!(["active"])
    );
    assert_eq!(
        after["filter_options"]["virtual_keys"],
        serde_json::json!([{"id":"key","name":"Z name"}])
    );
    let dashboard =
        serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap()).unwrap();
    assert_eq!(after["filter_options"], dashboard["filter_options"]);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// Persist request bodies and prune expired logs.
#[tokio::test]
async fn stores_and_expires_private_request_logs() {
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let store = LogStore::new(database);
    store.set_retention(30).await.unwrap();
    let entry = |created_at| LogEntry {
        id: 0,
        created_at,
        provider: "codex".into(),
        model: "model".into(),
        path: "/codex/v1/responses".into(),
        status: 200,
        latency_ms: 42,
        input_tokens: Some(10),
        output_tokens: Some(3),
        virtual_key_id: None,
        virtual_key_name: None,
        credential_id: None,
        credential_name: None,
        request_body: "private prompt".into(),
        response_body: "private response".into(),
    };
    store
        .insert(entry(now().saturating_sub(31 * 86400)))
        .await
        .unwrap();
    store.insert(entry(now())).await.unwrap();
    let result =
        serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap()).unwrap();
    assert_eq!(result["summary"]["requests"], 1);
    assert_eq!(result["summary"]["input_tokens"], 10);
    assert!(result["logs"][0].get("request_body").is_none());
    assert!(result["logs"][0].get("response_body").is_none());
    let detail = store
        .dashboard(&DashboardQuery {
            log_id: result["logs"][0]["id"].as_i64(),
            logs_only: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        detail.logs[0].request_body.as_deref(),
        Some("private prompt")
    );
    assert_eq!(
        detail.logs[0].response_body.as_deref(),
        Some("private response")
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// Concurrent inserts produce one record per request.
#[tokio::test]
async fn concurrent_inserts_are_not_duplicated() {
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let store = LogStore::new(database);
    let entry = |body: &str| LogEntry {
        id: 0,
        created_at: now(),
        provider: "codex".into(),
        model: "model".into(),
        path: "/codex/v1/responses".into(),
        status: 200,
        latency_ms: 42,
        input_tokens: Some(10),
        output_tokens: Some(3),
        virtual_key_id: None,
        virtual_key_name: None,
        credential_id: None,
        credential_name: None,
        request_body: body.into(),
        response_body: String::new(),
    };
    let (first, second) = tokio::join!(store.insert(entry("first")), store.insert(entry("second")));
    first.unwrap();
    second.unwrap();
    let result =
        serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap()).unwrap();
    assert_eq!(result["summary"]["requests"], 2);
    let logs = result["logs"].as_array().unwrap();
    let mut bodies = Vec::new();
    for log in logs {
        let detail = store
            .dashboard(&DashboardQuery {
                log_id: log["id"].as_i64(),
                logs_only: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        bodies.push(
            detail
                .logs
                .into_iter()
                .next()
                .unwrap()
                .request_body
                .unwrap(),
        );
    }
    bodies.sort();
    assert_eq!(bodies, ["first", "second"]);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

/// Select interval thresholds and UTC bucket boundaries.
#[test]
fn interval_thresholds_and_utc_bucket_boundaries() {
    let stamp = |year, month, day, hour, minute, second| {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
            .and_utc()
            .timestamp()
    };
    let selected =
        |start, end| serde_json::to_value(interval(Some(start), Some(end)).unwrap()).unwrap();
    let jan31 = stamp(2024, 1, 31, 12, 0, 0);
    assert_eq!(selected(jan31, jan31 + 86_400), "hour");
    assert_eq!(selected(jan31, jan31 + 86_401), "day");
    let april30 = stamp(2024, 4, 30, 12, 0, 0);
    assert_eq!(selected(jan31, april30), "day");
    assert_eq!(selected(jan31, april30 + 1), "month");
    let leap = stamp(2024, 2, 29, 0, 0, 0);
    let three_years = stamp(2027, 2, 28, 0, 0, 0);
    assert_eq!(selected(leap, three_years), "month");
    assert_eq!(selected(leap, three_years + 1), "year");
    assert_eq!(
        serde_json::to_value(interval(None, None).unwrap()).unwrap(),
        "day"
    );
    assert_eq!(
        serde_json::to_value(interval(None, Some(0)).unwrap()).unwrap(),
        "day"
    );
    assert_eq!(bucket_start(-1, Interval::Hour).unwrap(), -3600);
    assert_eq!(bucket_start(-1, Interval::Day).unwrap(), -86400);
    assert_eq!(
        bucket_start(-1, Interval::Month).unwrap(),
        stamp(1969, 12, 1, 0, 0, 0)
    );
    assert_eq!(
        bucket_start(leap, Interval::Year).unwrap(),
        stamp(2024, 1, 1, 0, 0, 0)
    );
    assert!(bucket_start(i64::MIN, Interval::Day).is_err());
    assert!(interval(Some(i64::MAX), Some(i64::MAX)).is_err());
}
