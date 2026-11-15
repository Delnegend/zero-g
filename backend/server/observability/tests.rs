use serde_json::Value;

use super::filter::Filter;
use super::logs::{LogEntry, Page, detail};
use super::{dashboard, logs, now, prune_expired};
use crate::database::{Database, transaction_with_retry};

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

/// Filter options read by scanning the logs in a range, as JSON.
async fn scanned_json(database: &Database, since: Option<i64>, upper: Option<i64>) -> Value {
    let options = transaction_with_retry!(database, |mut tx| {
        super::filter::scanned_options(&mut tx, super::filter::Range { since, upper }).await
    })
    .unwrap();
    serde_json::to_value(options).unwrap()
}

/// Filter options read from the retained snapshots, as JSON.
async fn snapshot_json(database: &Database, since: Option<i64>) -> Value {
    let options = transaction_with_retry!(database, |mut tx| {
        super::filter::snapshot_options(&mut tx, since).await
    })
    .unwrap();
    serde_json::to_value(options).unwrap()
}

/// Seed a log and its retained filter snapshots without invoking request capture.
pub(super) async fn fixture(database: &Database, log: LogEntry) -> Result<(), String> {
    let bodies = logs::CompressedBodies::new(&log).map_err(|err| err.to_string())?;
    transaction_with_retry!(database, |mut tx| {
        logs::insert(&mut tx, &log, &bodies).await?;
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
        request_body: Some(body),
        response_body: Some("response".into()),
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
        request_body: Some("private".into()),
        response_body: Some("response".into()),
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
        request_body: Some(String::new()),
        response_body: Some(String::new()),
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
        scanned_json(&database, None, None).await
    );
    drop(database);
    std::fs::remove_file(path).unwrap();
}

/// Whenever no log is newer than the range end, the snapshots give exactly the
/// options a scan of the range gives; the dashboard falls back to the scan otherwise.
#[tokio::test]
async fn snapshot_options_match_scan_for_ranges_ending_after_the_newest_log() {
    let path =
        std::env::temp_dir().join(format!("zero-g-options-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let entry = |at: i64, provider: &str, key: Option<(&str, &str)>| LogEntry {
        id: 0,
        created_at: at,
        provider: provider.into(),
        model: format!("model-{at}"),
        path: "/v1".into(),
        status: 200,
        latency_ms: 1,
        input_tokens: None,
        output_tokens: None,
        request_body: None,
        response_body: None,
        virtual_key_id: key.map(|(id, _)| id.into()),
        virtual_key_name: key.map(|(_, name)| name.into()),
        credential_id: None,
        credential_name: None,
    };
    let base = now().saturating_sub(10_000);
    for log in [
        entry(base, "old", Some(("key", "Z name"))),
        entry(base + 100, "", Some(("key", "A name"))),
        entry(base + 200, "new", Some(("key", "Z name"))),
        entry(base + 200, "new", None),
        entry(base + 300, "newest", Some(("other", "Other"))),
    ] {
        fixture(&database, log).await.unwrap();
    }
    let newest = base + 300;
    for since in [
        None,
        Some(base - 1),
        Some(base),
        Some(base + 1),
        Some(base + 100),
        Some(base + 200),
        Some(base + 300),
        Some(base + 301),
    ] {
        for upper in [newest, newest + 1000] {
            assert_eq!(
                snapshot_json(&database, since).await,
                scanned_json(&database, since, Some(upper)).await,
                "since {since:?}, upper {upper}"
            );
        }
    }
    // A range that ends before the newest log differs, so the probe must route it to the scan.
    let range = Filter {
        since: Some(base),
        until: Some(base + 150),
        ..Filter::default()
    };
    assert_ne!(
        snapshot_json(&database, Some(base)).await,
        scanned_json(&database, Some(base), Some(base + 150)).await
    );
    assert_eq!(
        dashboard_json(&database, &range).await["filter_options"],
        scanned_json(&database, Some(base), Some(base + 150)).await
    );
    drop(database);
    std::fs::remove_file(path).unwrap();
}

/// Bodies expire after the body policy while their log rows stay until the log policy.
#[tokio::test]
async fn bodies_expire_before_their_logs() {
    let path = std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
    let database = Database::open(path.to_str().unwrap()).await.unwrap();
    let entry = |days: i64| LogEntry {
        id: 0,
        created_at: now().saturating_sub(days * 86400),
        provider: "codex".into(),
        model: "model".into(),
        path: format!("/{days}"),
        status: 200,
        latency_ms: 42,
        input_tokens: Some(10),
        output_tokens: Some(3),
        virtual_key_id: None,
        virtual_key_name: None,
        credential_id: None,
        credential_name: None,
        request_body: Some("private prompt".into()),
        response_body: Some("private response".into()),
    };
    // Default policies: bodies 7 days, logs 90 days.
    for days in [0, 8, 91] {
        fixture(&database, entry(days)).await.unwrap();
    }
    prune_expired(&database).await.unwrap();
    let result = dashboard_json(&database, &Filter::default()).await;
    assert_eq!(result["summary"]["requests"], 2);
    assert_eq!(result["summary"]["input_tokens"], 20);
    let logs = logs_json(&database).await;
    assert!(logs["logs"][0].get("request_body").is_none());
    let mut bodies = std::collections::HashMap::new();
    for log in logs["logs"].as_array().unwrap() {
        let stored = detail(&database, log["id"].as_i64().unwrap())
            .await
            .unwrap()
            .unwrap();
        bodies.insert(stored.path, (stored.request_body, stored.response_body));
    }
    assert_eq!(
        bodies["/0"],
        (
            Some("private prompt".into()),
            Some("private response".into())
        )
    );
    assert_eq!(bodies["/8"], (None, None));
    assert!(!bodies.contains_key("/91"));
    drop(database);
    std::fs::remove_file(path).unwrap();
}
