use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::database::{Database, transaction_with_retry};
use crate::storage::{LogFilterOption, RequestLog};
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

/// Request capture middleware.
mod capture;
/// Observability dashboard endpoint.
mod dashboard;

pub(super) use capture::capture;
pub(super) use dashboard::dashboard;
use dashboard::{__path_dashboard, DashboardQuery};

/// Persistent request logs and their retention policy.
#[derive(Clone)]
pub(crate) struct LogStore {
    /// Database holding request records.
    database: Database,
    /// Number of days of records to retain.
    retention_days: Arc<RwLock<i64>>,
}
impl LogStore {
    /// Create a log store with the default 30-day retention.
    pub(crate) fn new(database: Database) -> Self {
        Self {
            database,
            retention_days: Arc::new(RwLock::new(30)),
        }
    }

    /// Apply startup retention and immediately prune expired records.
    pub(crate) async fn set_retention(&self, retention_days: i64) -> Result<(), String> {
        self.update_retention(retention_days, false).await
    }

    /// Save and apply retention atomically with pruning.
    pub(crate) async fn save_retention(&self, retention_days: i64) -> Result<(), String> {
        self.update_retention(retention_days, true).await
    }

    /// Serialize policy changes with insertions so old cutoffs cannot prune new data.
    async fn update_retention(&self, retention_days: i64, save: bool) -> Result<(), String> {
        let mut active = self.retention_days.write().await;
        let cutoff = now().saturating_sub(retention_days.saturating_mul(86400));
        transaction_with_retry!(self.database, |mut tx| {
            if save {
                crate::storage::Setting::upsert_by_name("log_retention_days")
                    .value(retention_days.to_string())
                    .exec(&mut tx)
                    .await?;
            }
            RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
                .delete()
                .exec(&mut tx)
                .await?;
            LogFilterOption::filter(LogFilterOption::fields().last_seen().lt(cutoff))
                .delete()
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        *active = retention_days;
        drop(active);
        Ok(())
    }

    /// Store a request record and prune expired records transactionally.
    async fn insert(&self, log: LogEntry) -> Result<(), String> {
        let active = self.retention_days.read().await;
        let cutoff = now().saturating_sub(active.saturating_mul(86400));
        let result = transaction_with_retry!(self.database, |mut tx| {
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
            update_filter_options(&mut tx, &log).await?;
            RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
                .delete()
                .exec(&mut tx)
                .await?;
            LogFilterOption::filter(LogFilterOption::fields().last_seen().lt(cutoff))
                .delete()
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .map_err(|e| e.to_string());
        drop(active);
        result
    }

    /// Aggregate request statistics and recent logs for the requested range.
    #[allow(clippy::too_many_lines)]
    async fn dashboard(&self, query: &DashboardQuery) -> Result<ObservabilityResponse, String> {
        let since = query.since;
        let until = query.until;
        let upper = until.or_else(|| since.map(|_| now()));
        let active = self.retention_days.read().await;
        let retention_days = *active;
        let interval = interval(since, upper)?;
        let (options, rows, logs) = transaction_with_retry!(self.database, |mut tx| {
            let mut rows_query = RequestLog::all();
            let mut options_query = RequestLog::all();
            let mut logs_query = RequestLog::all();
            if let Some(since) = since {
                rows_query = rows_query.filter(RequestLog::fields().created_at().ge(since));
                options_query = options_query.filter(RequestLog::fields().created_at().ge(since));
                logs_query = logs_query.filter(RequestLog::fields().created_at().ge(since));
            }
            if let Some(upper) = upper {
                rows_query = rows_query.filter(RequestLog::fields().created_at().le(upper));
                options_query = options_query.filter(RequestLog::fields().created_at().le(upper));
                logs_query = logs_query.filter(RequestLog::fields().created_at().le(upper));
            }
            let options = if query.logs_only.unwrap_or(false) && since.is_none() && upper.is_none()
            {
                // The unbounded logs page reads retention-maintained dimensions.
                OptionRows::Distinct(
                    LogFilterOption::all()
                        .select((
                            LogFilterOption::fields().kind(),
                            LogFilterOption::fields().value(),
                            LogFilterOption::fields().name(),
                        ))
                        .exec(&mut tx)
                        .await?,
                )
            } else {
                OptionRows::Dashboard(
                    options_query
                        .select((
                            RequestLog::fields().provider(),
                            RequestLog::fields().model(),
                            RequestLog::fields().path(),
                            RequestLog::fields().virtual_key_id(),
                            RequestLog::fields().virtual_key_name(),
                            RequestLog::fields().credential_id(),
                            RequestLog::fields().credential_name(),
                        ))
                        .exec(&mut tx)
                        .await?,
                )
            };
            if !query.providers.is_empty() {
                rows_query = rows_query.filter(
                    RequestLog::fields()
                        .provider()
                        .in_list(query.providers.clone()),
                );
                logs_query = logs_query.filter(
                    RequestLog::fields()
                        .provider()
                        .in_list(query.providers.clone()),
                );
            }
            if !query.models.is_empty() {
                rows_query =
                    rows_query.filter(RequestLog::fields().model().in_list(query.models.clone()));
                logs_query =
                    logs_query.filter(RequestLog::fields().model().in_list(query.models.clone()));
            }
            if !query.paths.is_empty() {
                rows_query =
                    rows_query.filter(RequestLog::fields().path().in_list(query.paths.clone()));
                logs_query =
                    logs_query.filter(RequestLog::fields().path().in_list(query.paths.clone()));
            }
            if !query.virtual_key_ids.is_empty() {
                let ids: Vec<_> = query.virtual_key_ids.iter().cloned().map(Some).collect();
                rows_query =
                    rows_query.filter(RequestLog::fields().virtual_key_id().in_list(ids.clone()));
                logs_query = logs_query.filter(RequestLog::fields().virtual_key_id().in_list(ids));
            }
            if !query.credential_ids.is_empty() {
                let ids: Vec<_> = query.credential_ids.iter().cloned().map(Some).collect();
                rows_query =
                    rows_query.filter(RequestLog::fields().credential_id().in_list(ids.clone()));
                logs_query = logs_query.filter(RequestLog::fields().credential_id().in_list(ids));
            }
            let success = query.statuses.iter().any(|s| s == "success");
            let error = query.statuses.iter().any(|s| s == "error");
            if success || error {
                let lower = if success { 200 } else { 400 };
                rows_query = rows_query.filter(RequestLog::fields().status().ge(lower));
                logs_query = logs_query.filter(RequestLog::fields().status().ge(lower));
                if !error {
                    rows_query = rows_query.filter(RequestLog::fields().status().lt(400));
                    logs_query = logs_query.filter(RequestLog::fields().status().lt(400));
                }
            }
            let rows = if query.logs_only.unwrap_or(false) {
                Vec::new()
            } else {
                rows_query
                    .select((
                        RequestLog::fields().status(),
                        RequestLog::fields().latency_ms(),
                        RequestLog::fields().input_tokens(),
                        RequestLog::fields().output_tokens(),
                        RequestLog::fields().created_at(),
                        RequestLog::fields().provider(),
                        RequestLog::fields().model(),
                    ))
                    .exec(&mut tx)
                    .await?
            };
            let logs = if query.aggregates_only.unwrap_or(false) {
                Vec::new()
            } else {
                let logs_query = if since.is_some() || until.is_some() {
                    logs_query.order_by((
                        RequestLog::fields().created_at().desc(),
                        RequestLog::fields().id().desc(),
                    ))
                } else {
                    logs_query.order_by(RequestLog::fields().id().desc())
                };
                logs_query.limit(200).exec(&mut tx).await?
            };
            tx.commit().await?;
            Ok((options, rows, logs))
        })
        .map_err(|e| e.to_string())?;
        drop(active);
        let (summary, buckets, breakdown) = aggregate_rows(&rows, interval)?;
        let filter_options = match options {
            OptionRows::Dashboard(rows) => collect_filter_options(rows),
            OptionRows::Distinct(rows) => collect_distinct_options(rows)?,
        };
        let logs = logs.into_iter().map(LogEntry::from).collect();
        Ok(ObservabilityResponse {
            summary,
            logs,
            interval,
            buckets,
            filter_options,
            breakdown,
            retention_cutoff: now().saturating_sub(retention_days.saturating_mul(86400)),
        })
    }
}

/// Track the newest retained request for every option and identity-name snapshot.
async fn update_filter_options(
    tx: &mut toasty::Transaction<'_>,
    log: &LogEntry,
) -> toasty::Result<()> {
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
            .exec(tx)
            .await?;
        let last_seen = existing.map_or(log.created_at, |row| row.last_seen.max(log.created_at));
        LogFilterOption::upsert_by_id(id.as_str())
            .kind(kind)
            .value(value)
            .name(name)
            .last_seen(last_seen)
            .exec(tx)
            .await?;
    }
    Ok(())
}

impl From<RequestLog> for LogEntry {
    fn from(r: RequestLog) -> Self {
        Self {
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
            virtual_key_id: r.virtual_key_id,
            virtual_key_name: r.virtual_key_name,
            credential_id: r.credential_id,
            credential_name: r.credential_name,
        }
    }
}

/// Selected columns used for aggregation, excluding captured bodies.
type MetricsRow = (i64, i64, Option<i64>, Option<i64>, i64, String, String);

/// Increment a named count without allocating for names already present.
fn increment_count(counts: &mut BTreeMap<String, usize>, name: &str) {
    if let Some(count) = counts.get_mut(name) {
        *count = count.saturating_add(1);
    } else {
        counts.insert(name.to_owned(), 1);
    }
}

/// Summarize all matching rows into time buckets and dimension counts.
fn aggregate_rows(
    rows: &[MetricsRow],
    interval: Interval,
) -> Result<(ObservabilitySummary, Vec<ObservabilityBucket>, Breakdown), String> {
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
    let mut counts = BTreeMap::<i64, ObservabilityBucket>::new();
    let mut providers = BTreeMap::<String, usize>::new();
    let mut models = BTreeMap::<String, usize>::new();
    for row in rows {
        let start = bucket_start(row.4, interval)?;
        let bucket = counts.entry(start).or_insert_with(|| ObservabilityBucket {
            start,
            requests: 0,
            successes: 0,
            errors: 0,
            input_tokens: None,
            output_tokens: None,
            avg_latency_ms: 0.0,
            by_model: BTreeMap::new(),
        });
        bucket.requests = bucket.requests.saturating_add(1);
        bucket.successes = bucket
            .successes
            .saturating_add(usize::from((200..400).contains(&row.0)));
        bucket.errors = bucket.errors.saturating_add(usize::from(row.0 >= 400));
        increment_count(&mut bucket.by_model, &row.6);
        if let Some(tokens) = row.2 {
            bucket.input_tokens = Some(bucket.input_tokens.unwrap_or(0).saturating_add(tokens));
        }
        if let Some(tokens) = row.3 {
            bucket.output_tokens = Some(bucket.output_tokens.unwrap_or(0).saturating_add(tokens));
        }
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        {
            bucket.avg_latency_ms += row.1 as f64;
        }
        increment_count(&mut providers, &row.5);
        increment_count(&mut models, &row.6);
    }
    let buckets = counts
        .into_values()
        .map(|mut bucket| {
            #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
            {
                bucket.avg_latency_ms /= bucket.requests as f64;
            }
            bucket
        })
        .collect();
    let breakdown = Breakdown {
        providers: providers
            .into_iter()
            .map(|(name, requests)| BreakdownEntry { name, requests })
            .collect(),
        models: models
            .into_iter()
            .map(|(name, requests)| BreakdownEntry { name, requests })
            .collect(),
    };
    Ok((summary, buckets, breakdown))
}

/// Selected time-range dimensions and identity snapshots.
type FilterOptionRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Dashboard rows or retained dimension snapshots for the logs view.
enum OptionRows {
    /// Individual rows used by the unchanged dashboard query.
    Dashboard(Vec<FilterOptionRow>),
    /// Retained dimension and identity-name snapshots.
    Distinct(Vec<(i64, String, String)>),
}

/// Convert retained dimension snapshots into dashboard-compatible options.
fn collect_distinct_options(rows: Vec<(i64, String, String)>) -> Result<FilterOptions, String> {
    let mut options = Vec::with_capacity(rows.len());
    for (kind, value, name) in rows {
        let empty = String::new();
        let row = match kind {
            0 => (value, empty.clone(), empty, None, None, None, None),
            1 => (empty.clone(), value, empty, None, None, None, None),
            2 => (empty.clone(), empty, value, None, None, None, None),
            3 => (
                empty.clone(),
                empty.clone(),
                empty,
                Some(value),
                Some(name),
                None,
                None,
            ),
            4 => (
                empty.clone(),
                empty.clone(),
                empty,
                None,
                None,
                Some(value),
                Some(name),
            ),
            _ => return Err("Invalid retained filter option".into()),
        };
        options.push(row);
    }
    Ok(collect_filter_options(options))
}

/// Collect sorted nonempty dimension options from the date range.
fn collect_filter_options(options: Vec<FilterOptionRow>) -> FilterOptions {
    let mut option_providers = BTreeSet::new();
    let mut option_models = BTreeSet::new();
    let mut option_paths = BTreeSet::new();
    let mut virtual_keys = BTreeMap::new();
    let mut credentials = BTreeMap::new();
    for (provider, model, path, virtual_key_id, virtual_key_name, credential_id, credential_name) in
        options
    {
        // A stable lexical choice makes snapshots deterministic even after renames.
        if let (Some(id), Some(name)) = (virtual_key_id, virtual_key_name) {
            virtual_keys
                .entry(id)
                .and_modify(|old: &mut String| {
                    if name < *old {
                        old.clone_from(&name);
                    }
                })
                .or_insert(name);
        }
        if let (Some(id), Some(name)) = (credential_id, credential_name) {
            credentials
                .entry(id)
                .and_modify(|old: &mut String| {
                    if name < *old {
                        old.clone_from(&name);
                    }
                })
                .or_insert(name);
        }
        if !provider.is_empty() {
            option_providers.insert(provider);
        }
        if !model.is_empty() {
            option_models.insert(model);
        }
        if !path.is_empty() {
            option_paths.insert(path);
        }
    }
    FilterOptions {
        providers: option_providers.into_iter().collect(),
        models: option_models.into_iter().collect(),
        paths: option_paths.into_iter().collect(),
        virtual_keys: virtual_keys
            .into_iter()
            .map(|(id, name)| IdentityOption { id, name })
            .collect(),
        credentials: credentials
            .into_iter()
            .map(|(id, name)| IdentityOption { id, name })
            .collect(),
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
    /// Authenticated virtual key ID and name snapshot, if known.
    #[schema(required = true)]
    virtual_key_id: Option<String>,
    /// Authenticated virtual key name snapshot.
    #[schema(required = true)]
    virtual_key_name: Option<String>,
    /// Successfully selected provider credential ID, if any.
    #[schema(required = true)]
    credential_id: Option<String>,
    /// Successfully selected provider credential name snapshot.
    #[schema(required = true)]
    credential_name: Option<String>,
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
    /// Responses with status 200 through 399.
    successes: usize,
    /// Responses with status at least 400.
    errors: usize,
    /// Sum of reported input tokens.
    #[schema(required = true)]
    input_tokens: Option<i64>,
    /// Sum of reported output tokens.
    #[schema(required = true)]
    output_tokens: Option<i64>,
    /// Mean latency in milliseconds.
    avg_latency_ms: f64,
    /// Request counts by model in this bucket.
    by_model: BTreeMap<String, usize>,
}

/// Available dimensions before applying dimension filters.
#[derive(Serialize, utoipa::ToSchema)]
struct FilterOptions {
    /// Available providers.
    providers: Vec<String>,
    /// Available models.
    models: Vec<String>,
    /// Available paths.
    paths: Vec<String>,
    /// Virtual keys observed within the time range.
    virtual_keys: Vec<IdentityOption>,
    /// Provider credentials observed within the time range.
    credentials: Vec<IdentityOption>,
}

/// A stable identity and its name snapshot.
#[derive(Serialize, utoipa::ToSchema)]
struct IdentityOption {
    /// Stable identifier.
    id: String,
    /// Name observed in a request.
    name: String,
}

/// Request counts grouped by provider and model.
#[derive(Serialize, utoipa::ToSchema)]
struct Breakdown {
    /// Counts by provider.
    providers: Vec<BreakdownEntry>,
    /// Counts by model.
    models: Vec<BreakdownEntry>,
}

/// One dimension value and its request count.
#[derive(Serialize, utoipa::ToSchema)]
struct BreakdownEntry {
    /// Dimension value.
    name: String,
    /// Number of requests for the value.
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
    /// Sorted dimension values in the selected date range.
    filter_options: FilterOptions,
    /// Counts by provider and model after filtering.
    breakdown: Breakdown,
    /// Earliest Unix second retained by the active log-store policy.
    retention_cutoff: i64,
}

/// Request observability dashboard operation.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(dashboard),
    components(schemas(
        ObservabilityResponse,
        ObservabilitySummary,
        ObservabilityBucket,
        FilterOptions,
        IdentityOption,
        Breakdown,
        BreakdownEntry,
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
            serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap())
                .unwrap();
        assert_eq!(result["summary"]["requests"], 2);
        assert_eq!(result["logs"][1]["request_body"], "old request");
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
            serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap())
                .unwrap();
        assert_eq!(after["filter_options"], dashboard["filter_options"]);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Persist request bodies and prune expired logs.
    #[tokio::test]
    async fn stores_and_expires_private_request_logs() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
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
            serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap())
                .unwrap();
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
            virtual_key_id: None,
            virtual_key_name: None,
            credential_id: None,
            credential_name: None,
            request_body: body.into(),
            response_body: String::new(),
        };
        let (first, second) =
            tokio::join!(store.insert(entry("first")), store.insert(entry("second")));
        first.unwrap();
        second.unwrap();
        let result =
            serde_json::to_value(store.dashboard(&DashboardQuery::default()).await.unwrap())
                .unwrap();
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
