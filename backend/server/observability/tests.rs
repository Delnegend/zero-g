use serde_json::Value;

use super::filter::Filter;
use super::logs::{LogEntry, Page, detail};
use super::{dashboard, logs, now, prune_expired};
use crate::database::{Database, transaction_with_retry};
use crate::storage::{LogFilterOption, RequestLog};

/// Dashboard response for a filter, as JSON.
async fn dashboard_json(database: &Database, filter: &Filter) -> Value {
    serde_json::to_value(dashboard::query(database, filter).await.unwrap()).unwrap()
}

/// First page of the unfiltered log list, as JSON.
async fn logs_json(database: &Database) -> Value {
    serde_json::to_value(
        logs::query(database, &Filter::default(), &Page::default())
            .await
            .unwrap(),
    )
    .unwrap()
}

/// A bounded range that forces filter options to be read from the logs themselves.
fn scanned() -> Filter {
    Filter {
        since: Some(0),
        ..Filter::default()
    }
}

/// Seed a log and its retained filter snapshots without invoking request capture.
pub(super) async fn fixture(database: &Database, log: LogEntry) -> Result<(), String> {
    transaction_with_retry!(database, |mut tx| {
        toasty::create!(RequestLog {
            created_at: log.created_at,
            provider: log.provider.clone(),
            model: log.model.clone(),
            path: log.path.clone(),
            status: log.status,
            latency_ms: log.latency_ms,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            request_body: log.request_body.clone(),
            response_body: log.response_body.clone(),
            virtual_key_id: log.virtual_key_id.clone(),
            virtual_key_name: log.virtual_key_name.clone(),
            credential_id: log.credential_id.clone(),
            credential_name: log.credential_name.clone(),
        })
        .exec(&mut tx)
        .await?;
        let options = [
            (!log.provider.is_empty()).then_some((0_i64, log.provider.as_str(), "")),
            (!log.model.is_empty()).then_some((1, log.model.as_str(), "")),
            (!log.path.is_empty()).then_some((2, log.path.as_str(), "")),
            log.virtual_key_id
                .as_deref()
                .zip(log.virtual_key_name.as_deref())
                .map(|(id, name)| (3, id, name)),
            log.credential_id
                .as_deref()
                .zip(log.credential_name.as_deref())
                .map(|(id, name)| (4, id, name)),
        ];
        for (kind, value, name) in options.into_iter().flatten() {
            let id = format!("{kind}:{}:{value}{name}", value.len());
            let existing = LogFilterOption::filter_by_id(id.as_str())
                .first()
                .exec(&mut tx)
                .await?;
            let last_seen =
                existing.map_or(log.created_at, |row| row.last_seen.max(log.created_at));
            LogFilterOption::upsert_by_id(id.as_str())
                .kind(kind)
                .value(value)
                .name(name)
                .last_seen(last_seen)
                .exec(&mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    })
    .map_err(|err| err.to_string())
}

/// Create rows spanning the time-range test boundaries.
pub(in crate::server::observability) async fn range_fixture() -> (std::path::PathBuf, Database) {
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let entry = |created_at, status, body: String| LogEntry {
        id: 0,
        created_at,
        provider: "codex".into(),
        model: "model".into(),
        path: "/v1".into(),
        status,
        latency_ms: 10,
        input_tokens: Some(2),
        output_tokens: Some(3),
        virtual_key_id: None,
        virtual_key_name: None,
        credential_id: None,
        credential_name: None,
        request_body: body,
        response_body: "response".into(),
    };
    fixture(&database, entry(-86401, 500, "before".into()))
        .await
        .unwrap();
    fixture(&database, entry(-86400, 500, "boundary".into()))
        .await
        .unwrap();
    fixture(&database, entry(0, 200, "midnight".into()))
        .await
        .unwrap();
    for i in 0..201 {
        fixture(&database, entry(-1, 200, i.to_string()))
            .await
            .unwrap();
    }
    fixture(
        &database,
        entry(now().saturating_add(86400), 500, "future".into()),
    )
    .await
    .unwrap();
    (path, database)
}

/// Create dimension rows including more than one page of recent logs.
pub(in crate::server::observability) async fn dimension_fixture() -> (std::path::PathBuf, Database)
{
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let entry = |created_at,
                 provider: &str,
                 model: &str,
                 path: &str,
                 status,
                 input_tokens,
                 output_tokens| LogEntry {
        id: 0,
        created_at,
        provider: provider.into(),
        model: model.into(),
        path: path.into(),
        status,
        latency_ms: 20,
        input_tokens,
        output_tokens,
        virtual_key_id: None,
        virtual_key_name: None,
        credential_id: None,
        credential_name: None,
        request_body: "private".into(),
        response_body: "response".into(),
    };
    fixture(&database, entry(100, "alpha", "m1", "/a", 399, None, None))
        .await
        .unwrap();
    fixture(
        &database,
        entry(101, "alpha", "m1", "/a", 400, Some(0), None),
    )
    .await
    .unwrap();
    fixture(
        &database,
        entry(102, "beta", "m2", "/b", 199, None, Some(7)),
    )
    .await
    .unwrap();
    fixture(
        &database,
        entry(99, "outside", "outside", "/outside", 500, None, None),
    )
    .await
    .unwrap();
    for _ in 0..201 {
        fixture(
            &database,
            entry(103, "beta", "m2", "/b", 200, Some(1), Some(2)),
        )
        .await
        .unwrap();
    }
    fixture(
        &database,
        entry(104, "提供,者", "模型,一", "/路径,a", 201, None, None),
    )
    .await
    .unwrap();
    (path, database)
}

/// Existing request identities and dimension options survive migration.
#[allow(clippy::too_many_lines)]
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
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let result = dashboard_json(&database, &Filter::default()).await;
    assert_eq!(result["summary"]["requests"], 2);
    let logs = logs_json(&database).await;
    assert!(logs["logs"][1].get("request_body").is_none());
    let stored = detail(&database, logs["logs"][1]["id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.request_body, "old request");
    for field in [
        "virtual_key_id",
        "virtual_key_name",
        "credential_id",
        "credential_name",
    ] {
        assert!(logs["logs"][1][field].is_null());
    }
    // Retained snapshots, read for the unbounded range, match options scanned from the rows.
    assert_eq!(
        logs["filter_options"],
        dashboard_json(&database, &scanned()).await["filter_options"]
    );
    assert_eq!(
        logs["filter_options"]["virtual_keys"],
        serde_json::json!([{"id": "old-key", "name": "Old key"}])
    );
    drop(database);
    std::fs::remove_file(path).unwrap();
}

/// Dashboard reports the current persisted retention policy.
#[tokio::test]
async fn dashboard_retention_uses_persisted_policy() {
    let path =
        std::env::temp_dir().join(format!("zero-g-retention-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    transaction_with_retry!(database, |mut tx| {
        crate::storage::Setting::upsert_by_name("log_retention_days")
            .value("30".to_string())
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
    .unwrap();
    let before = now();
    let result = dashboard::query(&database, &Filter::default())
        .await
        .unwrap();
    let after = now();
    assert!(result.retention_cutoff >= before.saturating_sub(30 * 86400));
    assert!(result.retention_cutoff <= after.saturating_sub(30 * 86400));
    drop(database);
    std::fs::remove_file(path).unwrap();
}

/// Expiration removes only snapshots whose last matching request was pruned.
#[tokio::test]
async fn retained_filter_options_follow_renames_and_pruning() {
    let path =
        std::env::temp_dir().join(format!("zero-g-options-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
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
    fixture(&database, entry(5, "Z name", "active"))
        .await
        .unwrap();
    fixture(&database, entry(20, "A name", "expired"))
        .await
        .unwrap();
    fixture(&database, entry(25, "Z name", "active"))
        .await
        .unwrap();
    let before = logs_json(&database).await;
    assert_eq!(
        before["filter_options"]["virtual_keys"],
        serde_json::json!([{"id":"key","name":"A name"}])
    );
    transaction_with_retry!(database, |mut tx| {
        crate::storage::Setting::upsert_by_name("log_retention_days")
            .value("10".to_string())
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
    .unwrap();
    let pending = logs_json(&database).await;
    assert_eq!(pending["logs"].as_array().unwrap().len(), 3);
    assert_eq!(pending["filter_options"], before["filter_options"]);
    prune_expired(&database).await.unwrap();
    let after = logs_json(&database).await;
    assert_eq!(after["logs"].as_array().unwrap().len(), 1);
    assert_eq!(
        after["filter_options"]["providers"],
        serde_json::json!(["active"])
    );
    assert_eq!(
        after["filter_options"]["virtual_keys"],
        serde_json::json!([{"id":"key","name":"Z name"}])
    );
    assert_eq!(
        after["filter_options"],
        dashboard_json(&database, &scanned()).await["filter_options"]
    );
    drop(database);
    std::fs::remove_file(path).unwrap();
}

/// Persist request bodies and prune expired logs.
#[tokio::test]
async fn stores_and_expires_private_request_logs() {
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
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
    fixture(&database, entry(now().saturating_sub(31 * 86400)))
        .await
        .unwrap();
    fixture(&database, entry(now())).await.unwrap();
    let pending = dashboard::query(&database, &Filter::default())
        .await
        .unwrap();
    assert_eq!(pending.summary.requests, 2);
    prune_expired(&database).await.unwrap();
    let result = dashboard_json(&database, &Filter::default()).await;
    assert_eq!(result["summary"]["requests"], 1);
    assert_eq!(result["summary"]["input_tokens"], 10);
    let logs = logs_json(&database).await;
    assert!(logs["logs"][0].get("request_body").is_none());
    assert!(logs["logs"][0].get("response_body").is_none());
    let stored = detail(&database, logs["logs"][0]["id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.request_body, "private prompt");
    assert_eq!(stored.response_body, "private response");
    drop(database);
    std::fs::remove_file(path).unwrap();
}
