use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::database::Database;
use crate::storage::RequestLog;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::error;

/// Persistent request logs and their retention policy.
#[derive(Clone)]
pub(crate) struct LogStore {
    /// Database holding request records.
    database: Database,
    /// Number of days of records to retain.
    retention_days: i64,
}
impl LogStore {
    /// Create a log store with the default 30-day retention.
    pub(crate) const fn new(database: Database) -> Self {
        Self {
            database,
            retention_days: 30,
        }
    }

    /// Update retention and immediately prune expired records.
    pub(crate) async fn set_retention(&mut self, retention_days: i64) -> Result<(), String> {
        self.retention_days = retention_days;
        self.prune().await
    }

    /// Delete records older than the retention cutoff.
    async fn prune(&self) -> Result<(), String> {
        let cutoff = now().saturating_sub(self.retention_days.saturating_mul(86400));
        self.database
            .retry(|| async {
                let mut db = (*self.database.db).clone();
                let mut tx = db.transaction().await?;
                RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
                    .delete()
                    .exec(&mut tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Store a request record and prune expired records transactionally.
    async fn insert(&self, log: LogEntry) -> Result<(), String> {
        let cutoff = now().saturating_sub(self.retention_days.saturating_mul(86400));
        self.database
            .retry(|| async {
                let mut db = (*self.database.db).clone();
                let mut tx = db.transaction().await?;
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
                })
                .exec(&mut tx)
                .await?;
                RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
                    .delete()
                    .exec(&mut tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Aggregate request statistics and recent logs for the requested range.
    async fn dashboard(&self, since: Option<i64>, until: Option<i64>) -> Result<Value, String> {
        let upper = until.or_else(|| since.map(|_| now()));
        let interval = interval(since, upper)?;
        let (rows, logs) = self
            .database
            .retry(|| async {
                let mut db = (*self.database.db).clone();
                let mut tx = db.transaction().await?;
                let mut rows_query = RequestLog::all();
                let mut logs_query = RequestLog::all();
                if let Some(since) = since {
                    rows_query = rows_query.filter(RequestLog::fields().created_at().ge(since));
                    logs_query = logs_query.filter(RequestLog::fields().created_at().ge(since));
                }
                if let Some(upper) = upper {
                    rows_query = rows_query.filter(RequestLog::fields().created_at().le(upper));
                    logs_query = logs_query.filter(RequestLog::fields().created_at().le(upper));
                }
                let rows = rows_query
                    .select((
                        RequestLog::fields().status(),
                        RequestLog::fields().latency_ms(),
                        RequestLog::fields().input_tokens(),
                        RequestLog::fields().output_tokens(),
                        RequestLog::fields().created_at(),
                    ))
                    .exec(&mut tx)
                    .await?;
                let logs_query = if since.is_some() || until.is_some() {
                    logs_query.order_by((
                        RequestLog::fields().created_at().desc(),
                        RequestLog::fields().id().desc(),
                    ))
                } else {
                    logs_query.order_by(RequestLog::fields().id().desc())
                };
                let logs = logs_query.limit(200).exec(&mut tx).await?;
                tx.commit().await?;
                Ok((rows, logs))
            })
            .await
            .map_err(|e| e.to_string())?;
        let count = rows.len();
        let successes = rows.iter().filter(|r| (200..400).contains(&r.0)).count();
        let errors = rows.iter().filter(|r| r.0 >= 400).count();
        let input_tokens = rows.iter().filter_map(|r| r.2).reduce(i64::saturating_add);
        let output_tokens = rows.iter().filter_map(|r| r.3).reduce(i64::saturating_add);
        // A floating-point average necessarily approximates large integer values.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let avg_latency_ms = if count == 0 {
            0.0
        } else {
            rows.iter().map(|r| r.1 as f64).sum::<f64>() / count as f64
        };
        let summary = json!({"requests": count, "successes": successes, "errors": errors, "input_tokens": input_tokens, "output_tokens": output_tokens, "avg_latency_ms": avg_latency_ms});
        let mut counts = BTreeMap::<i64, usize>::new();
        for row in &rows {
            let start = bucket_start(row.4, interval)?;
            let requests = counts.entry(start).or_default();
            *requests = requests.saturating_add(1);
        }
        let buckets: Vec<Value> = counts
            .into_iter()
            .map(|(start, requests)| json!({"start": start, "requests": requests}))
            .collect();
        let logs: Vec<LogEntry> = logs
            .into_iter()
            .map(|r| LogEntry {
                id: r.id,
                created_at: r.created_at,
                provider: r.provider,
                model: r.model,
                path: r.path,
                status: r.status,
                latency_ms: r.latency_ms,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                request_body: r.request_body,
                response_body: r.response_body,
            })
            .collect();
        Ok(json!({"summary": summary, "logs": logs, "interval": interval, "buckets": buckets}))
    }
}

/// Calendar granularity used for dashboard request buckets.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Interval {
    /// Hourly buckets.
    Hour,
    /// Daily buckets.
    Day,
    /// Monthly buckets.
    Month,
    /// Yearly buckets.
    Year,
}

/// Convert Unix seconds to a supported UTC date.
fn date(timestamp: i64) -> Result<DateTime<Utc>, String> {
    DateTime::from_timestamp(timestamp, 0)
        .ok_or_else(|| format!("timestamp outside supported date range: {timestamp}"))
}

/// Choose a bucket granularity for the requested time range.
fn interval(since: Option<i64>, until: Option<i64>) -> Result<Interval, String> {
    let start = since.map(date).transpose()?;
    let end = until.map(date).transpose()?;
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(Interval::Day);
    };
    if end.signed_duration_since(start).num_seconds() <= 86_400 {
        return Ok(Interval::Hour);
    }
    if start
        .checked_add_months(Months::new(3))
        .is_none_or(|limit| end <= limit)
    {
        return Ok(Interval::Day);
    }
    if start
        .checked_add_months(Months::new(36))
        .is_none_or(|limit| end <= limit)
    {
        return Ok(Interval::Month);
    }
    Ok(Interval::Year)
}

/// Compute the UTC start of the bucket containing a Unix timestamp.
fn bucket_start(timestamp: i64, interval: Interval) -> Result<i64, String> {
    let value = date(timestamp)?;
    let day = match interval {
        Interval::Hour => return Ok(timestamp.div_euclid(3600).saturating_mul(3600)),
        Interval::Day => Some(value.date_naive()),
        Interval::Month => NaiveDate::from_ymd_opt(value.year(), value.month(), 1),
        Interval::Year => NaiveDate::from_ymd_opt(value.year(), 1, 1),
    };
    day.and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|day| day.and_utc().timestamp())
        .ok_or_else(|| format!("cannot bucket timestamp: {timestamp}"))
}

/// Serialized request log returned by the dashboard.
#[derive(Serialize)]
struct LogEntry {
    /// Database record identifier.
    id: i64,
    /// Completion time in Unix seconds.
    created_at: i64,
    /// Provider extracted from the request path.
    provider: String,
    /// Model named in the request body.
    model: String,
    /// Request URI path.
    path: String,
    /// HTTP status, or 499 for an incomplete response stream.
    status: i64,
    /// Elapsed request time in milliseconds.
    latency_ms: i64,
    /// Reported input token count, when available.
    input_tokens: Option<i64>,
    /// Reported output token count, when available.
    output_tokens: Option<i64>,
    /// Captured request body.
    request_body: String,
    /// Captured response body.
    response_body: String,
}

/// Return the current Unix timestamp in seconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Optional inclusive time bounds for dashboard metrics.
#[derive(Deserialize)]
pub(super) struct DashboardQuery {
    /// Lower timestamp bound in Unix seconds.
    since: Option<i64>,
    /// Upper timestamp bound in Unix seconds.
    until: Option<i64>,
}

/// Serve dashboard metrics and recent request logs.
pub(super) async fn dashboard(
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

/// Capture POST request and response bodies for asynchronous logging.
pub(super) async fn capture(State(store): State<LogStore>, req: Request, next: Next) -> Response {
    if req.method() != Method::POST {
        return next.run(req).await;
    }
    let start = Instant::now();
    let path = req.uri().path().to_owned();
    let provider = path.split('/').nth(2).unwrap_or_default().to_owned();
    let (parts, body) = req.into_parts();
    let Ok(body) = to_bytes(body, 2 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let request_body = String::from_utf8_lossy(&body).into_owned();
    let model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    let status = i64::from(response.status().as_u16());
    let (parts, body) = response.into_parts();
    let stream = FinalizeStream {
        inner: Box::pin(body.into_data_stream()),
        finish: Some(Box::new(move |bytes, completed| {
            let response_body = String::from_utf8_lossy(&bytes).into_owned();
            let (input_tokens, output_tokens) = usage(&response_body);
            let log = LogEntry {
                id: 0,
                created_at: now(),
                provider,
                model,
                path,
                status: if completed { status } else { 499 },
                latency_ms: i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX),
                input_tokens,
                output_tokens,
                request_body,
                response_body,
            };
            tokio::spawn(async move {
                if let Err(err) = store.insert(log).await {
                    error!("Could not store request log: {err}");
                }
            });
        })),
        captured: Vec::new(),
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Callback receiving captured bytes and whether the response completed.
type Completion = Box<dyn FnOnce(Vec<u8>, bool) + Send>;

/// Response stream that records bytes and finalizes on completion or drop.
struct FinalizeStream<S> {
    /// Wrapped response body stream.
    inner: std::pin::Pin<Box<S>>,
    /// One-shot completion callback.
    finish: Option<Completion>,
    /// Response bytes observed so far.
    captured: Vec<u8>,
}
impl<S, E> futures_util::Stream for FinalizeStream<S>
where
    S: futures_util::Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        match &result {
            std::task::Poll::Ready(Some(Ok(bytes))) => self.captured.extend_from_slice(bytes),
            std::task::Poll::Ready(None) => {
                if let Some(finish) = self.finish.take() {
                    finish(std::mem::take(&mut self.captured), true);
                }
            }
            _ => {}
        }
        result
    }
}
impl<S> Drop for FinalizeStream<S> {
    fn drop(&mut self) {
        if let Some(finish) = self.finish.take() {
            finish(std::mem::take(&mut self.captured), false);
        }
    }
}

/// Extract token counts from JSON or server-sent event response bodies.
fn usage(body: &str) -> (Option<i64>, Option<i64>) {
    let mut result = (None, None);
    for part in std::iter::once(body).chain(
        body.lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim)),
    ) {
        if let Ok(json) = serde_json::from_str::<Value>(part) {
            let value = json
                .get("usage")
                .or_else(|| json.pointer("/response/usage"));
            if let Some(value) = value {
                result.0 = value
                    .get("input_tokens")
                    .or_else(|| value.get("prompt_tokens"))
                    .and_then(Value::as_i64)
                    .or(result.0);
                result.1 = value
                    .get("output_tokens")
                    .or_else(|| value.get("completion_tokens"))
                    .and_then(Value::as_i64)
                    .or(result.1);
            }
        }
    }
    result
}

/// Request log persistence and dashboard aggregation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    /// Persist request bodies and prune expired logs.
    #[tokio::test]
    async fn stores_and_expires_private_request_logs() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let mut store = LogStore::new(database);
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
            request_body: "private prompt".into(),
            response_body: "private response".into(),
        };
        store
            .insert(entry(now().saturating_sub(31 * 86400)))
            .await
            .unwrap();
        store.insert(entry(now())).await.unwrap();
        let result = store.dashboard(None, None).await.unwrap();
        assert_eq!(result["summary"]["requests"], 1);
        assert_eq!(result["summary"]["input_tokens"], 10);
        assert_eq!(result["logs"][0]["request_body"], "private prompt");
        assert_eq!(result["logs"][0]["response_body"], "private response");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Concurrent inserts produce one record per request.
    #[tokio::test]
    async fn concurrent_inserts_are_not_duplicated() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
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
            request_body: body.into(),
            response_body: String::new(),
        };
        let (first, second) =
            tokio::join!(store.insert(entry("first")), store.insert(entry("second")));
        first.unwrap();
        second.unwrap();
        let result = store.dashboard(None, None).await.unwrap();
        assert_eq!(result["summary"]["requests"], 2);
        let logs = result["logs"].as_array().unwrap();
        assert_eq!(
            logs.iter()
                .filter(|row| row["request_body"] == "first")
                .count(),
            1
        );
        assert_eq!(
            logs.iter()
                .filter(|row| row["request_body"] == "second")
                .count(),
            1
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
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
            .insert(entry(2_000_000_000, 500, "future".into()))
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
        let hourly = store.dashboard(Some(-86400), Some(0)).await.unwrap();
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

    /// Read usage counts from JSON and streaming responses.
    #[test]
    fn extracts_usage_from_streamed_and_nonstreamed_responses() {
        assert_eq!(
            usage(r#"{"usage":{"prompt_tokens":7,"completion_tokens":4}}"#),
            (Some(7), Some(4))
        );
        assert_eq!(
            usage("data: {\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":5}}}\n\n"),
            (Some(3), Some(5))
        );
    }
}
