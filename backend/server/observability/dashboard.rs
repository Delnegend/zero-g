//! Aggregate metrics for the observability dashboard.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use serde::Serialize;
use tracing::error;

use super::filter::{Filter, FilterOptions, options};
use super::retention::{cutoff, read_retention};
use crate::database::{Database, transaction_with_retry};
use crate::storage::RequestLog;

/// Serve dashboard metrics for a filtered range.
#[utoipa::path(get, path = "/api/dashboard", operation_id = "getDashboard",
    params(("since" = Option<i64>, Query, description = "Inclusive lower Unix timestamp"),
        ("until" = Option<i64>, Query, description = "Inclusive upper Unix timestamp"),
        ("providers" = Option<String>, Query, description = "JSON array of exact providers; empty means all"),
        ("models" = Option<String>, Query, description = "JSON array of exact models; empty means all"),
        ("paths" = Option<String>, Query, description = "JSON array of exact request paths; empty means all"),
        ("statuses" = Option<String>, Query, description = "JSON array of success (200–399) and/or error (400 and above); empty means all"),
        ("virtual_key_ids" = Option<String>, Query, description = "JSON array of exact virtual key IDs; empty means all"),
        ("credential_ids" = Option<String>, Query, description = "JSON array of exact provider credential IDs; empty means all")),
    responses((status = 200, body = DashboardResponse),
        (status = 400, description = "Invalid query"),
        (status = 500, description = "Log query failed")))]
pub(in crate::server) async fn dashboard(
    State(database): State<Database>,
    Query(filter): Query<Filter>,
) -> Response {
    match query(&database, &filter).await {
        Ok(data) => Json(data).into_response(),
        Err(err) => {
            error!("Log query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Aggregate every matching request in one snapshot.
pub(super) async fn query(
    database: &Database,
    filter: &Filter,
) -> Result<DashboardResponse, String> {
    let range = filter.range();
    let interval = interval(range.since, range.upper)?;
    let (retention_days, filter_options, rows) = transaction_with_retry!(database, |mut tx| {
        let retention_days = read_retention(&mut tx).await?;
        let filter_options = options(&mut tx, range).await?;
        let fields = RequestLog::fields();
        let rows = RequestLog::filter(filter.condition(range))
            .select((
                fields.status(),
                fields.latency_ms(),
                fields.input_tokens(),
                fields.output_tokens(),
                fields.created_at(),
                fields.provider(),
                fields.model(),
            ))
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok((retention_days, filter_options, rows))
    })
    .map_err(|e| e.to_string())?;
    let (summary, buckets, breakdown) = aggregate_rows(&rows, interval)?;
    Ok(DashboardResponse {
        summary,
        interval,
        buckets,
        filter_options,
        breakdown,
        retention_cutoff: cutoff(retention_days),
    })
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
            by_provider: BTreeMap::new(),
            by_model: BTreeMap::new(),
        });
        bucket.requests = bucket.requests.saturating_add(1);
        bucket.successes = bucket
            .successes
            .saturating_add(usize::from((200..400).contains(&row.0)));
        bucket.errors = bucket.errors.saturating_add(usize::from(row.0 >= 400));
        increment_count(&mut bucket.by_model, &row.6);
        if let Some(provider) = bucket.by_provider.get_mut(&row.5) {
            provider.requests = provider.requests.saturating_add(1);
            if let Some(tokens) = row.2 {
                provider.input_tokens =
                    Some(provider.input_tokens.unwrap_or(0).saturating_add(tokens));
            }
            if let Some(tokens) = row.3 {
                provider.output_tokens =
                    Some(provider.output_tokens.unwrap_or(0).saturating_add(tokens));
            }
        } else {
            bucket.by_provider.insert(
                row.5.clone(),
                ProviderBucket {
                    requests: 1,
                    input_tokens: row.2,
                    output_tokens: row.3,
                },
            );
        }
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

/// Calendar granularity used for dashboard request buckets.
#[derive(Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum Interval {
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

/// Aggregate statistics for the selected range.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ObservabilitySummary {
    /// Number of requests.
    pub(super) requests: usize,
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
pub(super) struct ObservabilityBucket {
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
    /// Request and token totals by provider in this bucket.
    by_provider: BTreeMap<String, ProviderBucket>,
    /// Request counts by model in this bucket.
    by_model: BTreeMap<String, usize>,
}

/// Available request and token totals for one provider in a time bucket.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderBucket {
    /// Number of requests.
    requests: usize,
    /// Sum of reported input tokens, or null if none were reported.
    #[schema(required = true)]
    input_tokens: Option<i64>,
    /// Sum of reported output tokens, or null if none were reported.
    #[schema(required = true)]
    output_tokens: Option<i64>,
}

/// Request counts grouped by provider and model.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct Breakdown {
    /// Counts by provider.
    providers: Vec<BreakdownEntry>,
    /// Counts by model.
    models: Vec<BreakdownEntry>,
}

/// One dimension value and its request count.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BreakdownEntry {
    /// Dimension value.
    name: String,
    /// Number of requests for the value.
    requests: usize,
}

/// Dashboard aggregates for a filtered time range.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct DashboardResponse {
    /// Aggregate request and token statistics.
    pub(super) summary: ObservabilitySummary,
    /// UTC bucket interval.
    interval: Interval,
    /// Time-bucketed metrics.
    buckets: Vec<ObservabilityBucket>,
    /// Available filter dimensions.
    filter_options: FilterOptions,
    /// Request counts by provider and model.
    breakdown: Breakdown,
    /// Earliest retained timestamp in Unix seconds.
    pub(super) retention_cutoff: i64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::LogEntry;
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

    /// Check dashboard responses for bounded time ranges.
    async fn check_bounded_ranges(app: &Router) {
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=-86400&until=-86400"))
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
            json!([{"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}}])
        );
        assert!(bounded.get("daily").is_none());
        assert!(bounded.get("logs").is_none());
        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=-86400&until=-86400"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let aggregate: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(aggregate["summary"], bounded["summary"]);
        assert_eq!(aggregate["buckets"], bounded["buckets"]);
        assert_eq!(aggregate["filter_options"], bounded["filter_options"]);
        assert!(aggregate.get("logs").is_none());
        assert!(!String::from_utf8_lossy(&body).contains("boundary"));
        assert!(!String::from_utf8_lossy(&body).contains("response"));

        let response = app
            .clone()
            .oneshot(request("/api/dashboard?until=-86400"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let until_only: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(until_only["summary"]["requests"], 2);
        assert_eq!(until_only["interval"], "day");
        assert_eq!(
            until_only["buckets"],
            json!([
                {"start": -172_800, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}},
                {"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}}
            ])
        );
        assert!(until_only.get("logs").is_none());

        let response = app
            .clone()
            .oneshot(request(
                "/api/dashboard?since=9223372036854775807&until=9223372036854775807",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=0&until=-1"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let reversed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reversed["summary"]["requests"], 0);
        assert!(reversed.get("logs").is_none());
        assert_eq!(reversed["buckets"], json!([]));
        for uri in [
            "/api/dashboard?since=nope",
            "/api/dashboard?since=9223372036854775808",
            "/api/dashboard?since=",
            "/api/dashboard?until=nope",
            "/api/dashboard?until=9223372036854775808",
            "/api/dashboard?until=",
        ] {
            assert_eq!(
                app.clone().oneshot(request(uri)).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
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
        fixture(
            &store,
            entry(
                Some(("key-a", "Old name")),
                Some(("cred-a", "Deleted credential")),
                200,
            ),
        )
        .await
        .unwrap();
        fixture(&store, entry(Some(("key-a", "Renamed")), None, 500))
            .await
            .unwrap();
        fixture(
            &store,
            entry(
                Some(("key-b", "Other")),
                Some(("cred-b", "Other credential")),
                200,
            ),
        )
        .await
        .unwrap();
        let app = Router::new()
            .route("/api/dashboard", get(dashboard))
            .with_state(store.clone());
        let response = app.clone().oneshot(Request::builder()
            .uri("/api/dashboard?since=100&until=103&providers=%5B%22alpha%22%5D&statuses=%5B%22success%22%5D&virtual_key_ids=%5B%22key-a%22%5D&credential_ids=%5B%22cred-a%22%5D")
            .body(Body::empty()).unwrap()).await.unwrap();
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["summary"]["requests"], 1);

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
            .uri("/api/dashboard?since=100&until=103&virtual_key_ids=%5B%22key-a%22%2C%22key-b%22%5D")
            .body(Body::empty()).unwrap()).await.unwrap();
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["summary"]["requests"], 3);

        for uri in [
            "/api/dashboard?virtual_key_ids=nope",
            "/api/dashboard?credential_ids=%5Bnull%5D",
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

    /// Apply time bounds to summaries and buckets.
    #[tokio::test]
    async fn range_filters_all_aggregates() {
        let (path, store) = range_fixture().await;
        let app = Router::new()
            .route("/api/dashboard", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=-86400"))
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
                {"start": -31_536_000, "requests": 202, "successes": 201, "errors": 1, "input_tokens": 404, "output_tokens": 606, "avg_latency_ms": 10.0, "by_model": {"model": 202}, "by_provider": {"codex": {"requests": 202, "input_tokens": 404, "output_tokens": 606}}},
                {"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}}
            ])
        );
        let hourly = serde_json::to_value(
            query(
                &store,
                &Filter {
                    since: Some(-86400),
                    until: Some(0),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(hourly["interval"], "hour");
        assert_eq!(hourly["summary"]["requests"], 203);
        assert_eq!(
            hourly["buckets"],
            json!([
                {"start": -86400, "requests": 1, "successes": 0, "errors": 1, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}},
                {"start": -3600, "requests": 201, "successes": 201, "errors": 0, "input_tokens": 402, "output_tokens": 603, "avg_latency_ms": 10.0, "by_model": {"model": 201}, "by_provider": {"codex": {"requests": 201, "input_tokens": 402, "output_tokens": 603}}},
                {"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": 2, "output_tokens": 3, "avg_latency_ms": 10.0, "by_model": {"model": 1}, "by_provider": {"codex": {"requests": 1, "input_tokens": 2, "output_tokens": 3}}}
            ])
        );
        assert!(result.get("logs").is_none());

        let response = app
            .clone()
            .oneshot(request("/api/dashboard"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let all: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(all["summary"]["requests"], 205);
        assert!(all.get("logs").is_none());
        check_bounded_ranges(&app).await;

        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=9223372036854775807"))
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
            .route("/api/dashboard", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request("/api/dashboard?since=100&until=103&providers=%5B%22alpha%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%5D&statuses=%5B%22success%22%5D")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["summary"]["input_tokens"], Value::Null);
        assert_eq!(value["summary"]["output_tokens"], Value::Null);
        assert_eq!(
            value["buckets"],
            json!([{"start": 0, "requests": 1, "successes": 1, "errors": 0, "input_tokens": null, "output_tokens": null, "avg_latency_ms": 20.0, "by_model": {"m1": 1}, "by_provider": {"alpha": {"requests": 1, "input_tokens": null, "output_tokens": null}}}])
        );

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
                "/api/dashboard?since=100&until=103&providers=%5B%22alpha%22%5D&statuses=%5B%22error%22%5D",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["summary"]["input_tokens"], 0);
        assert_eq!(value["buckets"][0]["output_tokens"], Value::Null);

        let response = app
            .clone()
            .oneshot(request("/api/dashboard?since=100&until=103"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 204);
        assert_eq!(value["summary"]["successes"], 202);
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["summary"]["input_tokens"], 201);
        assert_eq!(value["summary"]["output_tokens"], 409);
        assert!(value.get("logs").is_none());
        assert_eq!(value["buckets"][0]["by_model"], json!({"m1": 2, "m2": 202}));
        assert_eq!(
            value["buckets"][0]["by_provider"],
            json!({"alpha": {"requests": 2, "input_tokens": 0, "output_tokens": null}, "beta": {"requests": 202, "input_tokens": 201, "output_tokens": 409}})
        );
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
            .route("/api/dashboard", get(dashboard))
            .with_state(store.clone());
        let request = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request(
            "/api/dashboard?since=100&until=103&providers=%5B%22alpha%22%2C%22beta%22%5D&models=%5B%22m1%22%2C%22m2%22%5D&paths=%5B%22%2Fa%22%2C%22%2Fb%22%5D&statuses=%5B%22success%22%2C%22error%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 203);
        assert_eq!(value["summary"]["successes"], 202);
        assert_eq!(value["summary"]["errors"], 1);
        assert!(value.get("logs").is_none());
        assert_eq!(value["buckets"][0]["by_model"], json!({"m1": 2, "m2": 201}));

        let response = app.clone().oneshot(request(
            "/api/dashboard?since=100&until=103&providers=%5B%22alpha%22%2C%22beta%22%5D&models=%5B%22m1%22%5D&paths=%5B%22%2Fa%22%2C%22%2Fb%22%5D&statuses=%5B%22error%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);

        assert_eq!(
            value["filter_options"],
            json!({"providers":["alpha","beta"],"models":["m1","m2"],"paths":["/a","/b"],"virtual_keys":[],"credentials":[]})
        );

        let response = app.clone().oneshot(request(
            "/api/dashboard?since=100&until=103&providers=%5B%5D&models=%5B%5D&paths=%5B%5D&statuses=%5B%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 204);
        assert!(value.get("logs").is_none());

        let response = app.clone().oneshot(request(
            "/api/dashboard?since=104&until=104&providers=%5B%22%E6%8F%90%E4%BE%9B%2C%E8%80%85%22%5D&models=%5B%22%E6%A8%A1%E5%9E%8B%2C%E4%B8%80%22%5D&paths=%5B%22%2F%E8%B7%AF%E5%BE%84%2Ca%22%5D",
        )).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 1);
        assert_eq!(value["buckets"][0]["by_model"], json!({"模型,一": 1}));
        for uri in [
            "/api/dashboard?statuses=%5B%22other%22%5D",
            "/api/dashboard?statuses=",
            "/api/dashboard?providers=alpha",
            "/api/dashboard?models=%5B1%5D",
            "/api/dashboard?paths=%5Bnull%5D",
            "/api/dashboard?statuses=%5B%22success%22%2C%22other%22%5D",
            "/api/dashboard?statuses=%7B%7D",
            "/api/dashboard?page=1",
            "/api/dashboard?search=alpha",
        ] {
            assert_eq!(
                app.clone().oneshot(request(uri)).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        let response = app
            .clone()
            .oneshot(request(
                "/api/dashboard?since=100&until=103&providers=%5B%22Alpha%22%5D",
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["summary"]["requests"], 0);
        assert!(value.get("logs").is_none());
        assert_eq!(
            value["filter_options"]["providers"],
            json!(["alpha", "beta"])
        );
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

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
