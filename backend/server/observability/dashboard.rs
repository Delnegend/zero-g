use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tracing::error;

use super::{LogStore, ObservabilityResponse};

/// Optional inclusive bounds and exact-match dashboard filters.
#[derive(Default, Deserialize)]
pub(in crate::server) struct DashboardQuery {
    /// Inclusive lower timestamp.
    pub(super) since: Option<i64>,
    /// Inclusive upper timestamp.
    pub(super) until: Option<i64>,
    /// Skip dashboard aggregates for the recent-logs view.
    pub(super) logs_only: Option<bool>,
    /// Skip fetching recent request logs for aggregate-only dashboard views.
    pub(super) aggregates_only: Option<bool>,
    /// Exact providers encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) providers: Vec<String>,
    /// Exact models encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) models: Vec<String>,
    /// Exact request paths encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) paths: Vec<String>,
    /// Response classes encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) statuses: Vec<String>,
    /// Exact virtual key IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) virtual_key_ids: Vec<String>,
    /// Exact provider credential IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "deserialize_array")]
    pub(super) credential_ids: Vec<String>,
}

/// Decode a JSON array from one URL query value.
fn deserialize_array<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let value = String::deserialize(deserializer)?;
    serde_json::from_str(&value).map_err(serde::de::Error::custom)
}

/// Serve dashboard metrics and recent request logs.
#[utoipa::path(get, path = "/api/observability", operation_id = "getObservability",
    params(("since" = Option<i64>, Query, description = "Inclusive lower Unix timestamp"),
        ("until" = Option<i64>, Query, description = "Inclusive upper Unix timestamp"),
        ("providers" = Option<String>, Query, description = "JSON array of exact providers; empty means all"),
        ("models" = Option<String>, Query, description = "JSON array of exact models; empty means all"),
        ("paths" = Option<String>, Query, description = "JSON array of exact request paths; empty means all"),
        ("statuses" = Option<String>, Query, description = "JSON array of success (200–399) and/or error (400 and above); empty means all"),
        ("virtual_key_ids" = Option<String>, Query, description = "JSON array of exact virtual key IDs; empty means all"),
        ("credential_ids" = Option<String>, Query, description = "JSON array of exact provider credential IDs; empty means all"),
        ("logs_only" = Option<bool>, Query, description = "Return recent logs and filter options without calculating dashboard aggregates; aggregate fields are empty"),
        ("aggregates_only" = Option<bool>, Query, description = "Return aggregates and filter options without fetching request logs; logs is empty")),
    responses((status = 200, body = ObservabilityResponse),
        (status = 400, description = "Invalid query"),
        (status = 500, description = "Log query failed")))]
pub(in crate::server) async fn dashboard(
    State(store): State<LogStore>,
    Query(query): Query<DashboardQuery>,
) -> Response {
    if query
        .statuses
        .iter()
        .any(|s| s != "success" && s != "error")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match store.dashboard(&query).await {
        Ok(data) => Json(data).into_response(),
        Err(err) => {
            error!("Log query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Dashboard endpoint tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::{LogEntry, now};
    use super::*;
    use crate::database::Database;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Request,
        routing::get,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Check dashboard responses for bounded time ranges.
    async fn check_bounded_ranges(app: &Router) {
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app
            .clone()
            .oneshot(request("/api/observability?since=-86400&until=-86400"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let bounded: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(bounded["summary"]["requests"], 1);
        assert_eq!(bounded["summary"]["errors"], 1);
        assert_eq!(bounded["summary"]["input_tokens"], 2);
        assert_eq!(bounded["interval"], "hour");
        assert_eq!(
            bounded["buckets"],
            json!([{"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}}])
        );
        assert!(bounded.get("daily").is_none());
        assert_eq!(bounded["logs"].as_array().unwrap().len(), 1);
        assert_eq!(bounded["logs"][0]["request_body"], "boundary");
        let response = app
            .clone()
            .oneshot(request(
                "/api/observability?since=-86400&until=-86400&aggregates_only=true",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let aggregate: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(aggregate["summary"], bounded["summary"]);
        assert_eq!(aggregate["buckets"], bounded["buckets"]);
        assert_eq!(aggregate["filter_options"], bounded["filter_options"]);
        assert_eq!(aggregate["logs"], json!([]));
        assert!(!String::from_utf8_lossy(&body).contains("boundary"));
        assert!(!String::from_utf8_lossy(&body).contains("response"));

        let response = app
            .clone()
            .oneshot(request(
                "/api/observability?since=-86400&until=-86400&logs_only=true",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let logs: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(logs["logs"][0]["request_body"], "boundary");
        assert_eq!(logs["logs"][0]["response_body"], "response");

        let response = app
            .clone()
            .oneshot(request("/api/observability?until=-86400"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let until_only: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(until_only["summary"]["requests"], 2);
        assert_eq!(until_only["interval"], "day");
        assert_eq!(
            until_only["buckets"],
            json!([
                {"start": -172_800, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}},
                {"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}}
            ])
        );
        assert_eq!(until_only["logs"].as_array().unwrap().len(), 2);
        assert_eq!(until_only["logs"][0]["request_body"], "boundary");
        assert_eq!(until_only["logs"][1]["request_body"], "before");

        let response = app
            .clone()
            .oneshot(request(
                "/api/observability?since=9223372036854775807&until=9223372036854775807",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = app
            .clone()
            .oneshot(request("/api/observability?since=0&until=-1"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let reversed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reversed["summary"]["requests"], 0);
        assert_eq!(reversed["logs"], json!([]));
        assert_eq!(reversed["buckets"], json!([]));
        for uri in [
            "/api/observability?since=nope",
            "/api/observability?since=9223372036854775808",
            "/api/observability?since=",
            "/api/observability?until=nope",
            "/api/observability?until=9223372036854775808",
            "/api/observability?until=",
        ] {
            assert_eq!(
                app.clone().oneshot(request(uri)).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    /// Create rows spanning the time-range test boundaries.
    async fn range_fixture() -> (std::path::PathBuf, LogStore) {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = LogStore::new(database);
        store.set_retention(i64::MAX).await.unwrap();
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
        store
            .insert(entry(-86401, 500, "before".into()))
            .await
            .unwrap();
        store
            .insert(entry(-86400, 500, "boundary".into()))
            .await
            .unwrap();
        store
            .insert(entry(0, 200, "midnight".into()))
            .await
            .unwrap();
        for i in 0..201 {
            store.insert(entry(-1, 200, i.to_string())).await.unwrap();
        }
        store
            .insert(entry(now().saturating_add(86400), 500, "future".into()))
            .await
            .unwrap();
        (path, store)
    }

    /// Create dimension rows including more than one page of recent logs.
    async fn dimension_fixture() -> (std::path::PathBuf, LogStore) {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = LogStore::new(database);
        store.set_retention(i64::MAX).await.unwrap();
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
        store
            .insert(entry(100, "alpha", "m1", "/a", 399, None, None))
            .await
            .unwrap();
        store
            .insert(entry(101, "alpha", "m1", "/a", 400, Some(0), None))
            .await
            .unwrap();
        store
            .insert(entry(102, "beta", "m2", "/b", 199, None, Some(7)))
            .await
            .unwrap();
        store
            .insert(entry(99, "outside", "outside", "/outside", 500, None, None))
            .await
            .unwrap();
        for _ in 0..201 {
            store
                .insert(entry(103, "beta", "m2", "/b", 200, Some(1), Some(2)))
                .await
                .unwrap();
        }
        store
            .insert(entry(104, "提供,者", "模型,一", "/路径,a", 201, None, None))
            .await
            .unwrap();
        (path, store)
    }

    /// The logs view keeps full-range filter choices while limiting matching results.
    #[tokio::test]
    async fn logs_only_preserves_filters_bounds_and_recent_limit() {
        let (path, store) = dimension_fixture().await;
        store
            .insert(LogEntry {
                id: 0,
                created_at: 100,
                provider: "alpha".into(),
                model: "m1".into(),
                path: "/a".into(),
                status: 400,
                latency_ms: 1,
                input_tokens: None,
                output_tokens: None,
                virtual_key_id: Some("key-a".into()),
                virtual_key_name: Some("Historical key".into()),
                credential_id: Some("cred-a".into()),
                credential_name: Some("Historical credential".into()),
                request_body: "identity match".into(),
                response_body: String::new(),
            })
            .await
            .unwrap();
        let app = Router::new()
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());
        let fetch = |uri: String| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                serde_json::from_slice::<Value>(
                    &to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap()
            }
        };
        for filters in [
            "",
            "&providers=%5B%22alpha%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%5D&statuses=%5B%22error%22%5D&virtual_key_ids=%5B%22key-a%22%5D&credential_ids=%5B%22cred-a%22%5D",
            "&statuses=%5B%22success%22%5D",
            "&statuses=%5B%22error%22%5D",
            "&statuses=%5B%22success%22%2C%22error%22%5D",
        ] {
            let dashboard = fetch(format!("/api/observability?since=100&until=103{filters}")).await;
            let logs = fetch(format!(
                "/api/observability?since=100&until=103&logs_only=true{filters}"
            ))
            .await;
            assert_eq!(logs["logs"], dashboard["logs"]);
            assert_eq!(logs["filter_options"], dashboard["filter_options"]);
            assert_eq!(logs["summary"]["requests"], 0);
            assert_eq!(logs["buckets"], json!([]));
        }
        let all = fetch("/api/observability?logs_only=true".into()).await;
        assert_eq!(all["logs"].as_array().unwrap().len(), 200);
        assert!(
            all["filter_options"]["providers"]
                .as_array()
                .unwrap()
                .contains(&json!("outside"))
        );
        assert_eq!(
            all["filter_options"]["virtual_keys"],
            json!([{"id":"key-a","name":"Historical key"}])
        );
        let identity = fetch("/api/observability?logs_only=true&providers=%5B%22alpha%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%5D&statuses=%5B%22error%22%5D&virtual_key_ids=%5B%22key-a%22%5D&credential_ids=%5B%22cred-a%22%5D".into()).await;
        assert_eq!(identity["logs"].as_array().unwrap().len(), 1);
        assert_eq!(identity["logs"][0]["request_body"], "identity match");
        let success = fetch(
            "/api/observability?since=100&until=102&logs_only=true&statuses=%5B%22success%22%5D"
                .into(),
        )
        .await;
        assert_eq!(success["logs"].as_array().unwrap().len(), 1);
        assert_eq!(success["logs"][0]["status"], 399);
        let error = fetch(
            "/api/observability?since=100&until=102&logs_only=true&statuses=%5B%22error%22%5D"
                .into(),
        )
        .await;
        assert_eq!(error["logs"].as_array().unwrap().len(), 2);
        assert!(
            error["logs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|log| log["status"] == 400)
        );
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Identity filters combine with other dimensions and include historical snapshots.
    #[tokio::test]
    async fn identity_filters_use_all_rows_and_keep_deleted_names() {
        let (path, store) = dimension_fixture().await;
        let entry =
            |key: Option<(&str, &str)>, credential: Option<(&str, &str)>, status| LogEntry {
                id: 0,
                created_at: 100,
                provider: "alpha".into(),
                model: "m1".into(),
                path: "/a".into(),
                status,
                latency_ms: 1,
                input_tokens: None,
                output_tokens: None,
                request_body: String::new(),
                response_body: String::new(),
                virtual_key_id: key.map(|v| v.0.to_owned()),
                virtual_key_name: key.map(|v| v.1.to_owned()),
                credential_id: credential.map(|v| v.0.to_owned()),
                credential_name: credential.map(|v| v.1.to_owned()),
            };
        store
            .insert(entry(
                Some(("key-a", "Old name")),
                Some(("cred-a", "Deleted credential")),
                200,
            ))
            .await
            .unwrap();
        store
            .insert(entry(Some(("key-a", "Renamed")), None, 500))
            .await
            .unwrap();
        store
            .insert(entry(
                Some(("key-b", "Other")),
                Some(("cred-b", "Other credential")),
                200,
            ))
            .await
            .unwrap();
        let app = Router::new()
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());
        let response = app.clone().oneshot(Request::builder()
            .uri("/api/observability?since=100&until=103&providers=%5B%22alpha%22%5D&statuses=%5B%22success%22%5D&virtual_key_ids=%5B%22key-a%22%5D&credential_ids=%5B%22cred-a%22%5D")
            .body(Body::empty()).unwrap()).await.unwrap();
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["logs"][0]["virtual_key_name"], "Old name");
        assert_eq!(value["logs"][0]["credential_name"], "Deleted credential");
        assert_eq!(
            value["filter_options"]["virtual_keys"],
            json!([
                {"id":"key-a","name":"Old name"}, {"id":"key-b","name":"Other"}
            ])
        );
        assert_eq!(
            value["filter_options"]["credentials"],
            json!([
                {"id":"cred-a","name":"Deleted credential"}, {"id":"cred-b","name":"Other credential"}
            ])
        );
        let response = app.clone().oneshot(Request::builder()
            .uri("/api/observability?since=100&until=103&virtual_key_ids=%5B%22key-a%22%2C%22key-b%22%5D")
            .body(Body::empty()).unwrap()).await.unwrap();
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["summary"]["requests"], 3);
        assert_eq!(value["logs"].as_array().unwrap().len(), 3);
        for uri in [
            "/api/observability?virtual_key_ids=nope",
            "/api/observability?credential_ids=%5Bnull%5D",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Apply time bounds to summaries, buckets, and recent logs.
    #[tokio::test]
    async fn range_filters_all_aggregates_and_latest_logs() {
        let (path, store) = range_fixture().await;
        let app = Router::new()
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app
            .clone()
            .oneshot(request("/api/observability?since=-86400"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["summary"]["requests"], 203);
        assert_eq!(result["summary"]["successes"], 202);
        assert_eq!(result["summary"]["errors"], 1);
        assert_eq!(result["summary"]["input_tokens"], 406);
        assert_eq!(result["summary"]["output_tokens"], 609);
        assert_eq!(result["interval"], "year");
        assert_eq!(
            result["buckets"],
            json!([
                {"start": -31_536_000, "requests": 202, "successes": 201, "errors": 1, "input_tokens": 404, "output_tokens": 606, "avg_latency_ms": 10.0, "by_model": {"model": 202}},
                {"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}}
            ])
        );
        let hourly = serde_json::to_value(
            store
                .dashboard(&DashboardQuery {
                    since: Some(-86400),
                    until: Some(0),
                    ..Default::default()
                })
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(hourly["interval"], "hour");
        assert_eq!(hourly["summary"]["requests"], 203);
        assert_eq!(hourly["logs"].as_array().unwrap().len(), 200);
        assert_eq!(
            hourly["buckets"],
            json!([
                {"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}},
                {"start": -3600, "requests": 201, "successes": 201, "errors": 0, "input_tokens": 402, "output_tokens": 603, "avg_latency_ms": 10.0, "by_model": {"model": 201}},
                {"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}}
            ])
        );
        let logs = result["logs"].as_array().unwrap();
        assert_eq!(logs.len(), 200);
        assert_eq!(logs[0]["request_body"], "midnight");
        assert_eq!(logs[199]["request_body"], "2");
        assert_eq!(logs[0]["response_body"], "response");

        let response = app
            .clone()
            .oneshot(request("/api/observability"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let all: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(all["summary"]["requests"], 205);
        assert_eq!(all["logs"][0]["request_body"], "future");
        check_bounded_ranges(&app).await;

        let response = app
            .clone()
            .oneshot(request("/api/observability?since=9223372036854775807"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Filters apply to all rows, while options remain independent of dimensions.
    #[tokio::test]
    async fn dimension_filters_and_full_dataset_metrics() {
        let (path, store) = dimension_fixture().await;
        let app = Router::new()
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request("/api/observability?since=100&until=103&providers=%5B%22alpha%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%5D&statuses=%5B%22success%22%5D")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["summary"]["input_tokens"], Value::Null);
        assert_eq!(value["summary"]["output_tokens"], Value::Null);
        assert_eq!(
            value["buckets"],
            json!([{"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": null, "output_tokens": null, "avg_latency_ms": 20.0, "by_model": {"m1": 1}}])
        );
        assert_eq!(value["logs"].as_array().unwrap().len(), 1);
        assert_eq!(value["logs"][0]["status"], 399);
        assert_eq!(
            value["filter_options"],
            json!({"providers":["alpha","beta"],"models":["m1","m2"],"paths":["/a","/b"],"virtual_keys":[],"credentials":[]})
        );
        assert_eq!(
            value["breakdown"],
            json!({"providers":[{"name":"alpha","requests":1}],"models":[{"name":"m1","requests":1}]})
        );

        let response = app
            .clone()
            .oneshot(request(
                "/api/observability?since=100&until=103&providers=%5B%22alpha%22%5D&statuses=%5B%22error%22%5D",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["summary"]["input_tokens"], 0);
        assert_eq!(value["buckets"][0]["output_tokens"], Value::Null);
        assert_eq!(value["logs"][0]["status"], 400);

        let response = app
            .clone()
            .oneshot(request("/api/observability?since=100&until=103"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 204);
        assert_eq!(value["summary"]["successes"], 202);
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["summary"]["input_tokens"], 201);
        assert_eq!(value["summary"]["output_tokens"], 409);
        assert_eq!(value["logs"].as_array().unwrap().len(), 200);
        assert_eq!(value["buckets"][0]["by_model"], json!({"m1": 2, "m2": 202}));
        assert_eq!(
            value["breakdown"]["providers"],
            json!([{"name":"alpha","requests":2},{"name":"beta","requests":202}])
        );
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Multi-select, empty, invalid, and encoded values retain their filter semantics.
    #[tokio::test]
    async fn multiple_and_encoded_dimension_filters() {
        let (path, store) = dimension_fixture().await;
        let app = Router::new()
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request(
            "/api/observability?since=100&until=103&providers=%5B%22alpha%22%2C%22beta%22%5D&models=%5B%22m1%22%2C%22m2%22%5D&paths=%5B%22%2Fa%22%2C%22%2Fb%22%5D&statuses=%5B%22success%22%2C%22error%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 203);
        assert_eq!(value["summary"]["successes"], 202);
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["logs"].as_array().unwrap().len(), 200);
        assert_eq!(value["buckets"][0]["by_model"], json!({"m1": 2, "m2": 201}));

        let response = app.clone().oneshot(request(
            "/api/observability?since=100&until=103&providers=%5B%22alpha%22%2C%22beta%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%2C%22%2Fb%22%5D&statuses=%5B%22error%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["logs"][0]["status"], 400);
        assert_eq!(
            value["filter_options"],
            json!({"providers":["alpha","beta"],"models":["m1","m2"],"paths":["/a","/b"],"virtual_keys":[],"credentials":[]})
        );

        let response = app.clone().oneshot(request(
            "/api/observability?since=100&until=103&providers=%5B%5D&models=%5B%5D&paths=%5B%5D&statuses=%5B%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 204);
        assert_eq!(value["logs"].as_array().unwrap().len(), 200);

        let response = app.clone().oneshot(request(
            "/api/observability?since=104&until=104&providers=%5B%22%E6%8F%90%E4%BE%9B%2C%E8%80%85%22%5D&models=%5B%22%E6%A8%A1%E5%9E%8B%2C%E4%B8%80%22%5D&paths=%5B%22%2F%E8%B7%AF%E5%BE%84%2Ca%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["logs"][0]["provider"], "提供,者");
        assert_eq!(value["logs"][0]["path"], "/路径,a");
        assert_eq!(value["buckets"][0]["by_model"], json!({"模型,一": 1}));
        for uri in [
            "/api/observability?statuses=%5B%22other%22%5D",
            "/api/observability?statuses=",
            "/api/observability?providers=alpha",
            "/api/observability?models=%5B1%5D",
            "/api/observability?paths=%5Bnull%5D",
            "/api/observability?statuses=%5B%22success%22%2C%22other%22%5D",
            "/api/observability?statuses=%7B%7D",
        ] {
            assert_eq!(
                app.clone().oneshot(request(uri)).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        let response = app
            .clone()
            .oneshot(request(
                "/api/observability?since=100&until=103&providers=%5B%22Alpha%22%5D",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 0);
        assert_eq!(value["logs"], json!([]));
        assert_eq!(
            value["filter_options"]["providers"],
            json!(["alpha", "beta"])
        );
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    /// Settings updates immediately affect existing capture and dashboard clones.
    #[tokio::test]
    async fn saved_retention_applies_to_existing_clones() {
        let path =
            std::env::temp_dir().join(format!("zero-g-live-retention-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let settings = crate::server::settings::SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let store = LogStore::new(database);
        let capture = store.clone();
        let app = Router::new()
            .route(
                "/api/observability",
                get(dashboard).with_state(store.clone()),
            )
            .route(
                "/api/settings",
                axum::routing::put(crate::server::settings::put_settings)
                    .with_state((settings.clone(), store.clone())),
            );
        let entry = || LogEntry {
            id: 0,
            created_at: now() - 10 * 86400,
            provider: "provider".into(),
            model: "model".into(),
            path: "/v1".into(),
            status: 200,
            latency_ms: 1,
            input_tokens: None,
            output_tokens: None,
            request_body: "request".into(),
            response_body: "response".into(),
            virtual_key_id: None,
            virtual_key_name: None,
            credential_id: None,
            credential_name: None,
        };
        capture.insert(entry()).await.unwrap();
        let put = |days| {
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"log_retention_days":{days}}}"#)))
                .unwrap()
        };
        let request = || {
            Request::builder()
                .uri("/api/observability?logs_only=true")
                .body(Body::empty())
                .unwrap()
        };
        let response = app.clone().oneshot(put(1)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let saved: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(saved["log_retention_days"], 1);
        let response = app.clone().oneshot(request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["logs"], json!([]));
        assert!(body["retention_cutoff"].as_i64().unwrap() >= now() - 86401);

        assert_eq!(
            app.clone().oneshot(put(20)).await.unwrap().status(),
            StatusCode::OK
        );
        capture.insert(entry()).await.unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["logs"][0]["request_body"], "request");
        assert_eq!(body["logs"][0]["response_body"], "response");
        toasty::sql::statement("DROP TABLE log_filter_options")
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        assert_eq!(
            app.clone().oneshot(put(1)).await.unwrap().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            settings
                .setting_get("log_retention_days")
                .await
                .unwrap()
                .as_deref(),
            Some("20")
        );
        assert_eq!(*capture.retention_days.read().await, 20);
        drop(app);
        drop(capture);
        drop(store);
        drop(settings);
        std::fs::remove_file(path).unwrap();
    }
}
