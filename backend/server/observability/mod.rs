use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::database::{Database, transaction_with_retry};
use crate::storage::RequestLog;
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use serde::Serialize;

/// Request capture middleware.
mod capture;
/// Observability dashboard endpoint.
mod dashboard;

pub(super) use capture::capture;
use dashboard::__path_dashboard;
pub(super) use dashboard::dashboard;

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
        transaction_with_retry!(self.database, |mut tx| {
            RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
                .delete()
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .map_err(|e| e.to_string())
    }

    /// Store a request record and prune expired records transactionally.
    async fn insert(&self, log: LogEntry) -> Result<(), String> {
        let cutoff = now().saturating_sub(self.retention_days.saturating_mul(86400));
        transaction_with_retry!(self.database, |mut tx| {
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
        .map_err(|e| e.to_string())
    }

    /// Aggregate request statistics and recent logs for the requested range.
    async fn dashboard(
        &self,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<ObservabilityResponse, String> {
        let upper = until.or_else(|| since.map(|_| now()));
        let interval = interval(since, upper)?;
        let (rows, logs) = transaction_with_retry!(self.database, |mut tx| {
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
        let summary = ObservabilitySummary {
            requests: count,
            successes,
            errors,
            input_tokens,
            output_tokens,
            avg_latency_ms,
        };
        let mut counts = BTreeMap::<i64, usize>::new();
        for row in &rows {
            let start = bucket_start(row.4, interval)?;
            let requests = counts.entry(start).or_default();
            *requests = requests.saturating_add(1);
        }
        let buckets: Vec<ObservabilityBucket> = counts
            .into_iter()
            .map(|(start, requests)| ObservabilityBucket { start, requests })
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
        Ok(ObservabilityResponse {
            summary,
            logs,
            interval,
            buckets,
        })
    }
}

/// Calendar granularity used for dashboard request buckets.
#[derive(Clone, Copy, Serialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
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
    #[schema(required = true)]
    input_tokens: Option<i64>,
    /// Reported output token count, when available.
    #[schema(required = true)]
    output_tokens: Option<i64>,
    /// Captured request body.
    request_body: String,
    /// Captured response body.
    response_body: String,
}

/// Aggregate statistics for the selected range.
#[derive(Serialize, utoipa::ToSchema)]
struct ObservabilitySummary {
    /// Number of requests.
    requests: usize,
    /// Number of responses with status 200 through 399.
    successes: usize,
    /// Number of responses with status at least 400.
    errors: usize,
    /// Sum of available input token counts.
    #[schema(required = true)]
    input_tokens: Option<i64>,
    /// Sum of available output token counts.
    #[schema(required = true)]
    output_tokens: Option<i64>,
    /// Mean latency in milliseconds.
    avg_latency_ms: f64,
}

/// Request count for a UTC time bucket.
#[derive(Serialize, utoipa::ToSchema)]
struct ObservabilityBucket {
    /// Bucket start in Unix seconds.
    start: i64,
    /// Number of requests in the bucket.
    requests: usize,
}

/// Dashboard metrics and recent request logs.
#[derive(Serialize, utoipa::ToSchema)]
struct ObservabilityResponse {
    /// Aggregate metrics.
    summary: ObservabilitySummary,
    /// Up to 200 recent log entries.
    logs: Vec<LogEntry>,
    /// Bucket granularity.
    interval: Interval,
    /// Request counts by time bucket.
    buckets: Vec<ObservabilityBucket>,
}

/// Request observability dashboard operation.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(dashboard),
    components(schemas(
        ObservabilityResponse,
        ObservabilitySummary,
        ObservabilityBucket,
        Interval,
        LogEntry
    ))
)]
pub(crate) struct Api;

/// Return the current Unix timestamp in seconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Request log persistence and aggregation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
        let result = serde_json::to_value(store.dashboard(None, None).await.unwrap()).unwrap();
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
        let result = serde_json::to_value(store.dashboard(None, None).await.unwrap()).unwrap();
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
}
