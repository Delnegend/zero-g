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
mod tests;
