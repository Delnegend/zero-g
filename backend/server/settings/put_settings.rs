use super::{SettingsResponse, SettingsStore, UpdateSettings, get_settings, internal_error};
use crate::server::observability::LogStore;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

/// Validates and atomically applies supplied settings.
#[utoipa::path(
    put,
    path = "/api/settings",
    operation_id = "updateSettings",
    request_body = UpdateSettings,
    responses(
        (status = 200, description = "Updated settings", body = SettingsResponse),
        (status = 400, description = "Invalid log retention", body = String, content_type = "text/plain"),
        (status = 500, description = "Settings store error")
    )
)]
pub(crate) async fn put_settings(
    State((store, logs)): State<(SettingsStore, LogStore)>,
    Json(input): Json<UpdateSettings>,
) -> Response {
    if input
        .log_retention_days
        .is_some_and(|days| !(1..=36500).contains(&days))
    {
        return (StatusCode::BAD_REQUEST, "Invalid log retention").into_response();
    }
    if let Some(days) = input.log_retention_days
        && let Err(err) = logs.save_retention(i64::from(days)).await
    {
        return internal_error(&err);
    }
    get_settings(State(store)).await
}
