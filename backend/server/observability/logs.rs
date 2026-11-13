//! Paginated request log listing and single-log details.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use toasty::stmt::{Expr, OrderByExpr};
use tracing::error;

use super::filter::{Filter, FilterOptions, StatusClass, json_array, options};
use super::retention::{cutoff, read_retention, update_filter_options};
use crate::database::{Database, transaction_with_retry};
use crate::storage::{RequestLog, RequestLogBody};

/// Largest accepted page size.
const MAX_LIMIT: usize = 200;

/// Log list ordering; ties always fall back to newest ID first.
#[derive(Clone, Copy, Default, Deserialize)]
pub(super) enum LogSort {
    /// Newest first.
    #[default]
    #[serde(rename = "recent", alias = "created_at:desc")]
    Recent,
    /// Oldest first.
    #[serde(rename = "created_at:asc")]
    Oldest,
    /// Model ascending.
    #[serde(rename = "model:asc")]
    ModelAsc,
    /// Model descending.
    #[serde(rename = "model:desc")]
    ModelDesc,
    /// Status ascending.
    #[serde(rename = "status:asc")]
    StatusAsc,
    /// Status descending.
    #[serde(rename = "status:desc")]
    StatusDesc,
    /// Latency ascending.
    #[serde(rename = "latency_ms:asc")]
    LatencyAsc,
    /// Latency descending.
    #[serde(rename = "latency_ms:desc")]
    LatencyDesc,
    /// Path ascending.
    #[serde(rename = "path:asc")]
    PathAsc,
    /// Path descending.
    #[serde(rename = "path:desc")]
    PathDesc,
}

impl LogSort {
    /// Primary ordering expression.
    fn order(self) -> OrderByExpr {
        let fields = RequestLog::fields();
        match self {
            Self::Recent => fields.created_at().desc(),
            Self::Oldest => fields.created_at().asc(),
            Self::ModelAsc => fields.model().asc(),
            Self::ModelDesc => fields.model().desc(),
            Self::StatusAsc => fields.status().asc(),
            Self::StatusDesc => fields.status().desc(),
            Self::LatencyAsc => fields.latency_ms().asc(),
            Self::LatencyDesc => fields.latency_ms().desc(),
            Self::PathAsc => fields.path().asc(),
            Self::PathDesc => fields.path().desc(),
        }
    }
}

/// Page, search, and ordering applied after filtering.
pub(super) struct Page {
    /// One-based page number.
    pub(super) number: usize,
    /// Page size.
    pub(super) limit: usize,
    /// Case-insensitive metadata search.
    pub(super) search: Option<String>,
    /// Requested ordering.
    pub(super) sort: LogSort,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            number: 1,
            limit: 20,
            search: None,
            sort: LogSort::Recent,
        }
    }
}

impl Page {
    /// Rows to skip, or `None` when the page or size is out of range.
    fn offset(&self) -> Option<usize> {
        if self.number == 0 || !(1..=MAX_LIMIT).contains(&self.limit) {
            return None;
        }
        self.number.saturating_sub(1).checked_mul(self.limit)
    }
}

/// Log list query: the shared filter plus paging, rejecting unknown parameters.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct LogsQuery {
    /// Inclusive lower timestamp.
    since: Option<i64>,
    /// Inclusive upper timestamp.
    until: Option<i64>,
    /// Exact providers encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    providers: Vec<String>,
    /// Exact models encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    models: Vec<String>,
    /// Exact request paths encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    paths: Vec<String>,
    /// Response classes encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    statuses: Vec<StatusClass>,
    /// Exact virtual key IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    virtual_key_ids: Vec<String>,
    /// Exact provider credential IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    credential_ids: Vec<String>,
    /// One-based page number.
    page: Option<usize>,
    /// Page size.
    limit: Option<usize>,
    /// Case-insensitive metadata search.
    search: Option<String>,
    /// Requested ordering.
    #[serde(default)]
    sort: LogSort,
}

impl LogsQuery {
    /// Separate the shared filter from paging.
    fn split(self) -> (Filter, Page) {
        let Self {
            since,
            until,
            providers,
            models,
            paths,
            statuses,
            virtual_key_ids,
            credential_ids,
            page,
            limit,
            search,
            sort,
        } = self;
        let defaults = Page::default();
        (
            Filter {
                since,
                until,
                providers,
                models,
                paths,
                statuses,
                virtual_key_ids,
                credential_ids,
            },
            Page {
                number: page.unwrap_or(defaults.number),
                limit: limit.unwrap_or(defaults.limit),
                search,
                sort,
            },
        )
    }
}

/// Match the search text in provider, model, or path, or exactly as an ID, status, or timestamp.
fn search_condition(search: Option<&str>) -> Option<Expr<bool>> {
    let search = search.map(str::trim).filter(|s| !s.is_empty())?;
    let mut pattern = String::with_capacity(search.len().saturating_add(2));
    pattern.push('%');
    for c in search.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    let fields = RequestLog::fields();
    // SQLite LIKE folds ASCII case.
    let mut condition = fields
        .provider()
        .like_with_escape(pattern.clone(), '\\')
        .or(fields.model().like_with_escape(pattern.clone(), '\\'))
        .or(fields.path().like_with_escape(pattern, '\\'));
    if let Ok(number) = search.parse::<i64>() {
        condition = condition
            .or(fields.id().eq(number))
            .or(fields.status().eq(number))
            .or(fields.created_at().eq(number));
    }
    Some(condition)
}

/// Count matching logs and load one ordered page without bodies, in one snapshot.
pub(super) async fn query(
    database: &Database,
    filter: &Filter,
    page: &Page,
) -> Result<LogsResponse, String> {
    let range = filter.range();
    let offset = page.offset().ok_or("invalid log page")?;
    let mut condition = filter.condition(range);
    if let Some(search) = search_condition(page.search.as_deref()) {
        condition = condition.and(search);
    }
    let (retention_days, filter_options, total, rows) =
        transaction_with_retry!(database, |mut tx| {
            let retention_days = read_retention(&mut tx).await?;
            let filter_options = options(&mut tx, range).await?;
            let total = RequestLog::filter(condition.clone())
                .count()
                .exec(&mut tx)
                .await?;
            // SQLite offsets are signed; an unrepresentable offset is beyond every result.
            let rows = if i64::try_from(offset).is_ok() {
                let fields = RequestLog::fields();
                RequestLog::filter(condition.clone())
                    .order_by((page.sort.order(), fields.id().desc()))
                    .limit(page.limit)
                    .offset(offset)
                    .select((
                        (
                            fields.id(),
                            fields.created_at(),
                            fields.provider(),
                            fields.model(),
                            fields.path(),
                            fields.status(),
                            fields.latency_ms(),
                        ),
                        (
                            fields.input_tokens(),
                            fields.output_tokens(),
                            fields.virtual_key_id(),
                            fields.virtual_key_name(),
                            fields.credential_id(),
                            fields.credential_name(),
                        ),
                    ))
                    .exec(&mut tx)
                    .await?
            } else {
                Vec::new()
            };
            tx.commit().await?;
            Ok((retention_days, filter_options, total, rows))
        })
        .map_err(|e| e.to_string())?;
    let logs = rows
        .into_iter()
        .map(
            |(
                (id, created_at, provider, model, path, status, latency_ms),
                (
                    input_tokens,
                    output_tokens,
                    virtual_key_id,
                    virtual_key_name,
                    credential_id,
                    credential_name,
                ),
            )| LogSummary {
                id,
                created_at,
                provider,
                model,
                path,
                status,
                latency_ms,
                input_tokens,
                output_tokens,
                virtual_key_id,
                virtual_key_name,
                credential_id,
                credential_name,
            },
        )
        .collect();
    Ok(LogsResponse {
        logs,
        total: usize::try_from(total).map_err(|_| "invalid log count")?,
        page: page.number,
        limit: page.limit,
        filter_options,
        retention_cutoff: cutoff(retention_days),
    })
}

/// Retrieve one captured log and its bodies without loading unrelated records.
pub(super) async fn detail(database: &Database, id: i64) -> Result<Option<LogEntry>, String> {
    transaction_with_retry!(database, |mut tx| {
        let Some(record) = RequestLog::filter_by_id(id).first().exec(&mut tx).await? else {
            return Ok(None);
        };
        let body = RequestLogBody::filter_by_log_id(id)
            .first()
            .exec(&mut tx)
            .await?;
        Ok(Some(LogEntry::from_parts(record, body)))
    })
    .map_err(|e| e.to_string())
}

/// Store a captured log, its bodies, and its filter snapshots atomically.
pub(super) async fn insert(tx: &mut toasty::Transaction<'_>, log: &LogEntry) -> toasty::Result<()> {
    let record = toasty::create!(RequestLog {
        created_at: log.created_at,
        provider: log.provider.clone(),
        model: log.model.clone(),
        path: log.path.clone(),
        status: log.status,
        latency_ms: log.latency_ms,
        input_tokens: log.input_tokens,
        output_tokens: log.output_tokens,
        virtual_key_id: log.virtual_key_id.clone(),
        virtual_key_name: log.virtual_key_name.clone(),
        credential_id: log.credential_id.clone(),
        credential_name: log.credential_name.clone(),
    })
    .exec(tx)
    .await?;
    toasty::create!(RequestLogBody {
        log_id: record.id,
        created_at: log.created_at,
        request_body: log.request_body.clone(),
        response_body: log.response_body.clone(),
    })
    .exec(tx)
    .await?;
    update_filter_options(tx, log).await
}

/// Serve a bounded list of metadata and available filter dimensions.
#[utoipa::path(get, path = "/api/logs", operation_id = "listLogs",
    params(("since" = Option<i64>, Query, description = "Inclusive lower Unix timestamp"),
        ("until" = Option<i64>, Query, description = "Inclusive upper Unix timestamp"),
        ("providers" = Option<String>, Query, description = "JSON array of exact providers"),
        ("models" = Option<String>, Query, description = "JSON array of exact models"),
        ("paths" = Option<String>, Query, description = "JSON array of exact request paths"),
        ("statuses" = Option<String>, Query, description = "JSON array of success and/or error"),
        ("virtual_key_ids" = Option<String>, Query, description = "JSON array of exact virtual key IDs"),
        ("credential_ids" = Option<String>, Query, description = "JSON array of exact provider credential IDs"),
        ("page" = Option<usize>, Query, description = "One-based page number, default 1"),
        ("limit" = Option<usize>, Query, description = "Page size, default 20, maximum 200"),
        ("search" = Option<String>, Query, description = "Case-insensitive provider, model, or path search; whole numbers also match an exact ID, status, or timestamp"),
        ("sort" = Option<String>, Query, description = "Sort field and direction, or recent")),
    responses((status = 200, body = LogsResponse),
        (status = 400, description = "Invalid query"),
        (status = 500, description = "Log query failed")))]
pub(in crate::server) async fn logs(
    State(database): State<Database>,
    Query(query): Query<LogsQuery>,
) -> Response {
    let (filter, page) = query.split();
    if page.offset().is_none() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match self::query(&database, &filter, &page).await {
        Ok(data) => Json(data).into_response(),
        Err(err) => {
            error!("Log query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Serve one captured log including its bodies.
#[utoipa::path(get, path = "/api/logs/{id}", operation_id = "getLog",
    params(("id" = i64, Path, description = "Log identifier")),
    responses((status = 200, body = LogEntry),
        (status = 404, description = "Log not found"),
        (status = 500, description = "Log query failed")))]
pub(in crate::server) async fn log_detail(
    State(database): State<Database>,
    Path(id): Path<i64>,
) -> Response {
    match detail(&database, id).await {
        Ok(Some(log)) => Json(log).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            error!("Log query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Log metadata returned by list queries, without captured bodies.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct LogSummary {
    /// Database record identifier.
    pub(super) id: i64,
    /// Completion time in Unix seconds.
    created_at: i64,
    /// Provider serving the request.
    provider: String,
    /// Requested model.
    model: String,
    /// Request URI path.
    pub(super) path: String,
    /// HTTP response status.
    status: i64,
    /// Elapsed request time in milliseconds.
    latency_ms: i64,
    /// Reported input tokens, if available.
    #[schema(required = true)]
    input_tokens: Option<i64>,
    /// Reported output tokens, if available.
    #[schema(required = true)]
    output_tokens: Option<i64>,
    /// Authenticated virtual key ID, if known.
    #[schema(required = true)]
    virtual_key_id: Option<String>,
    /// Virtual key name snapshot, if known.
    #[schema(required = true)]
    virtual_key_name: Option<String>,
    /// Selected provider credential ID, if known.
    #[schema(required = true)]
    credential_id: Option<String>,
    /// Provider credential name snapshot, if known.
    #[schema(required = true)]
    credential_name: Option<String>,
}

/// One stored request log including its captured bodies.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct LogEntry {
    /// Database record identifier.
    pub(super) id: i64,
    /// Completion time in Unix seconds.
    pub(super) created_at: i64,
    /// Provider extracted from the request path.
    pub(super) provider: String,
    /// Model named in the request body.
    pub(super) model: String,
    /// Request URI path.
    pub(super) path: String,
    /// HTTP status, or 499 for an incomplete response stream.
    pub(super) status: i64,
    /// Elapsed request time in milliseconds.
    pub(super) latency_ms: i64,
    /// Reported input token count, when available.
    #[schema(required = true)]
    pub(super) input_tokens: Option<i64>,
    /// Reported output token count, when available.
    #[schema(required = true)]
    pub(super) output_tokens: Option<i64>,
    /// Captured request body.
    pub(super) request_body: String,
    /// Captured response body.
    pub(super) response_body: String,
    /// Authenticated virtual key ID and name snapshot, if known.
    #[schema(required = true)]
    pub(super) virtual_key_id: Option<String>,
    /// Authenticated virtual key name snapshot.
    #[schema(required = true)]
    pub(super) virtual_key_name: Option<String>,
    /// Successfully selected provider credential ID, if any.
    #[schema(required = true)]
    pub(super) credential_id: Option<String>,
    /// Successfully selected provider credential name snapshot.
    #[schema(required = true)]
    pub(super) credential_name: Option<String>,
}

impl LogEntry {
    /// Combine log metadata with its separately stored bodies, which are empty when absent.
    fn from_parts(r: RequestLog, body: Option<RequestLogBody>) -> Self {
        let (request_body, response_body) =
            body.map_or_else(Default::default, |b| (b.request_body, b.response_body));
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
            request_body,
            response_body,
            virtual_key_id: r.virtual_key_id,
            virtual_key_name: r.virtual_key_name,
            credential_id: r.credential_id,
            credential_name: r.credential_name,
        }
    }
}

/// Bounded metadata listing and available filter dimensions.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct LogsResponse {
    /// Up to 200 matching log summaries without bodies.
    pub(super) logs: Vec<LogSummary>,
    /// Number of matching log records before pagination.
    total: usize,
    /// One-based page number.
    page: usize,
    /// Maximum number of entries per page.
    limit: usize,
    /// Available filter dimensions.
    filter_options: FilterOptions,
    /// Earliest retained timestamp in Unix seconds.
    retention_cutoff: i64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::dashboard::dashboard;
    use super::super::now;
    use super::super::tests::{dimension_fixture, fixture, range_fixture};
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Request,
        routing::get,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;
    /// Check detail retrieval and a missing log independently of list projections.
    async fn check_log_details(app: &Router, id: i64) {
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app
            .clone()
            .oneshot(request(&format!("/api/logs/{id}")))
            .await
            .unwrap();
        let detail: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(detail["request_body"], "boundary");
        assert_eq!(detail["response_body"], "response");
        let response = app.clone().oneshot(request("/api/logs/-1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn log_detail_returns_bodies_and_not_found() {
        let (path, database) = range_fixture().await;
        let app = Router::new()
            .route("/api/logs", get(logs))
            .route("/api/logs/{id}", get(log_detail))
            .with_state(database.clone());
        let request = Request::builder()
            .uri("/api/logs?since=-86400&until=-86400")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let logs: Value = serde_json::from_slice(&body).unwrap();
        assert!(logs["logs"][0].get("request_body").is_none());
        assert!(logs["logs"][0].get("response_body").is_none());
        check_log_details(&app, logs["logs"][0]["id"].as_i64().unwrap()).await;
        drop(app);
        drop(database);
        std::fs::remove_file(path).unwrap();
    }

    /// The logs view keeps full-range filter choices while limiting matching results.
    #[tokio::test]
    async fn logs_only_preserves_filters_bounds_and_recent_limit() {
        let (path, store) = dimension_fixture().await;
        fixture(
            &store,
            LogEntry {
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
            },
        )
        .await
        .unwrap();
        let app = Router::new()
            .route("/api/dashboard", get(dashboard))
            .route("/api/logs", get(logs))
            .route("/api/logs/{id}", get(log_detail))
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
            let dashboard = fetch(format!("/api/dashboard?since=100&until=103{filters}")).await;
            let logs = fetch(format!("/api/logs?since=100&until=103{filters}")).await;
            assert_eq!(
                u64::try_from(logs["logs"].as_array().unwrap().len()).unwrap(),
                dashboard["summary"]["requests"].as_u64().unwrap().min(20)
            );
            assert_eq!(logs["total"], dashboard["summary"]["requests"]);
            assert_eq!(logs["filter_options"], dashboard["filter_options"]);
            assert!(logs.get("summary").is_none());
            assert!(logs.get("buckets").is_none());
        }
        let all = fetch("/api/logs".into()).await;
        assert_eq!(all["logs"].as_array().unwrap().len(), 20);
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
        let identity = fetch("/api/logs?providers=%5B%22alpha%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%5D&statuses=%5B%22error%22%5D&virtual_key_ids=%5B%22key-a%22%5D&credential_ids=%5B%22cred-a%22%5D".into()).await;
        assert_eq!(identity["logs"].as_array().unwrap().len(), 1);
        assert!(identity["logs"][0].get("request_body").is_none());
        let success =
            fetch("/api/logs?since=100&until=102&statuses=%5B%22success%22%5D".into()).await;
        assert_eq!(success["logs"].as_array().unwrap().len(), 1);
        assert_eq!(success["logs"][0]["status"], 399);
        let error = fetch("/api/logs?since=100&until=102&statuses=%5B%22error%22%5D".into()).await;
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

    /// Pagination, searching and ordering operate on the complete filtered result set.
    #[tokio::test]
    async fn pages_search_and_sort_all_matching_logs() {
        let (path, store) = dimension_fixture().await;
        let app = Router::new()
            .route("/api/logs", get(logs))
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
        let first = fetch("/api/logs?limit=200".into()).await;
        let second = fetch("/api/logs?limit=200&page=2".into()).await;
        assert_eq!(first["total"], 206);
        assert_eq!(first["page"], 1);
        assert_eq!(first["limit"], 200);
        assert_eq!(first["logs"].as_array().unwrap().len(), 200);
        assert_eq!(second["logs"].as_array().unwrap().len(), 6);
        let ids: Vec<i64> = first["logs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["logs"].as_array().unwrap())
            .map(|row| row["id"].as_i64().unwrap())
            .collect();
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            206
        );
        let equal_time: Vec<_> = first["logs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["logs"].as_array().unwrap())
            .filter(|row| row["created_at"] == 103)
            .map(|row| row["id"].as_i64().unwrap())
            .collect();
        assert!(equal_time.windows(2).all(|pair| pair[0] > pair[1]));
        let old = fetch("/api/logs?search=OUTSIDE".into()).await;
        assert_eq!(old["total"], 1);
        assert_eq!(old["logs"][0]["provider"], "outside");
        let literal = fetch("/api/logs?search=%25".into()).await;
        assert_eq!(literal["total"], 0);
        // Whole numbers also match a status exactly.
        let status = fetch("/api/logs?search=500".into()).await;
        assert_eq!(status["total"], 1);
        assert_eq!(status["logs"][0]["status"], 500);
        let filtered = fetch("/api/logs?providers=%5B%22beta%22%5D&statuses=%5B%22success%22%5D&search=m2&limit=10&page=21".into()).await;
        assert_eq!(filtered["total"], 201);
        assert_eq!(filtered["logs"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["filter_options"], first["filter_options"]);
        let empty = fetch("/api/logs?limit=200&page=3".into()).await;
        assert_eq!(empty["total"], 206);
        assert_eq!(empty["logs"], json!([]));
        for (field, asc, desc) in [
            ("created_at", "99", "104"),
            ("model", "m1", "模型,一"),
            ("status", "199", "500"),
            ("latency_ms", "20", "20"),
            ("path", "/a", "/路径,a"),
        ] {
            for (direction, expected) in [("asc", asc), ("desc", desc)] {
                let value = fetch(format!("/api/logs?limit=1&sort={field}:{direction}")).await;
                let actual = &value["logs"][0][field];
                assert_eq!(
                    actual
                        .as_str()
                        .map_or_else(|| actual.to_string(), str::to_owned),
                    expected
                );
            }
        }
        let ascending = fetch("/api/logs?sort=created_at:asc&limit=200&page=2".into()).await;
        assert_eq!(
            ascending["logs"].as_array().unwrap().last().unwrap()["created_at"],
            104
        );
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_pagination_and_sort() {
        let (path, store) = dimension_fixture().await;
        let app = Router::new()
            .route("/api/logs", get(logs))
            .with_state(store.clone());
        for query in [
            "page=0",
            "page=-1",
            "page=abc",
            "page=18446744073709551615&limit=200",
            "limit=0",
            "limit=201",
            "limit=-1",
            "limit=abc",
            "sort=",
            "sort=model",
            "sort=provider:asc",
            "sort=model:up",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/logs?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    /// Settings changes take effect at cleanup without deleting logs early.
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
        let store = database;
        let app = Router::new()
            .route("/api/logs", get(logs).with_state(store.clone()))
            .route(
                "/api/settings",
                axum::routing::put(crate::server::settings::put_settings)
                    .with_state(settings.clone()),
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
        fixture(&store, entry()).await.unwrap();
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
                .uri("/api/logs")
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
        assert_eq!(body["logs"].as_array().unwrap().len(), 1);
        assert!(body["retention_cutoff"].as_i64().unwrap() >= now() - 86401);

        assert_eq!(
            app.clone().oneshot(put(20)).await.unwrap().status(),
            StatusCode::OK
        );
        super::super::prune_expired(&store).await.unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body["logs"][0].get("request_body").is_none());
        assert!(body["logs"][0].get("response_body").is_none());
        assert_eq!(body["logs"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.clone().oneshot(put(1)).await.unwrap().status(),
            StatusCode::OK
        );
        let response = app.clone().oneshot(request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["logs"].as_array().unwrap().len(), 1);
        super::super::prune_expired(&store).await.unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["logs"], json!([]));
        drop(app);
        drop(store);
        drop(settings);
        std::fs::remove_file(path).unwrap();
    }
}
