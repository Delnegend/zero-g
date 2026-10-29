use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tracing::error;

use super::{LogStore, ObservabilityResponse};

/// Optional inclusive time bounds for dashboard metrics.
#[derive(Deserialize)]
pub(in crate::server) struct DashboardQuery {
    /// Lower timestamp bound in Unix seconds.
    since: Option<i64>,
    /// Upper timestamp bound in Unix seconds.
    until: Option<i64>,
}

/// Serve dashboard metrics and recent request logs.
#[utoipa::path(get, path = "/api/observability", operation_id = "getObservability",
    params(("since" = Option<i64>, Query, description = "Inclusive lower Unix timestamp"),
        ("until" = Option<i64>, Query, description = "Inclusive upper Unix timestamp")),
    responses((status = 200, body = ObservabilityResponse),
        (status = 400, description = "Invalid query"),
        (status = 500, description = "Log query failed")))]
pub(in crate::server) async fn dashboard(
    State(store): State<LogStore>,
    Query(query): Query<DashboardQuery>,
) -> Response {
    match store.dashboard(query.since, query.until).await {
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
            json!([{"start": -86400, "requests": 1}])
        );
        assert!(bounded.get("daily").is_none());
        assert_eq!(bounded["logs"].as_array().unwrap().len(), 1);
        assert_eq!(bounded["logs"][0]["request_body"], "boundary");

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
                {"start": -172_800, "requests": 1},
                {"start": -86400, "requests": 1}
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

    /// Apply time bounds to summaries, buckets, and recent logs.
    #[tokio::test]
    async fn range_filters_all_aggregates_and_latest_logs() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let mut store = LogStore::new(database);
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
                {"start": -31_536_000, "requests": 202},
                {"start": 0, "requests": 1}
            ])
        );
        let hourly =
            serde_json::to_value(store.dashboard(Some(-86400), Some(0)).await.unwrap()).unwrap();
        assert_eq!(hourly["interval"], "hour");
        assert_eq!(hourly["summary"]["requests"], 203);
        assert_eq!(hourly["logs"].as_array().unwrap().len(), 200);
        assert_eq!(
            hourly["buckets"],
            json!([
                {"start": -86400, "requests": 1},
                {"start": -3600, "requests": 201},
                {"start": 0, "requests": 1}
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
}
