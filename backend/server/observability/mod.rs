use std::time::{SystemTime, UNIX_EPOCH};

/// Request capture middleware.
mod capture;
/// Observability dashboard endpoint.
mod dashboard;
/// Filters and filter choices shared by the dashboard and log list.
mod filter;
/// Request log endpoints.
mod logs;
/// Retention pruning and retained filter snapshots.
mod retention;

pub(super) use capture::capture;
pub(super) use dashboard::dashboard;
use dashboard::{
    __path_dashboard, Breakdown, BreakdownEntry, DashboardResponse, Interval, ObservabilityBucket,
    ObservabilitySummary, ProviderBucket,
};
use filter::{FilterOptions, IdentityOption};
use logs::{__path_log_detail, __path_logs, LogEntry, LogSummary, LogsResponse};
pub(super) use logs::{log_detail, logs};
pub(crate) use retention::prune_expired;

/// Request observability dashboard operation.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(dashboard, logs, log_detail),
    components(schemas(
        DashboardResponse,
        LogsResponse,
        ObservabilitySummary,
        ObservabilityBucket,
        ProviderBucket,
        FilterOptions,
        IdentityOption,
        Breakdown,
        BreakdownEntry,
        Interval,
        LogEntry,
        LogSummary
    ))
)]
pub(crate) struct Api;

/// Return the current Unix timestamp in seconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Request log persistence and retention tests, and shared fixtures.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
