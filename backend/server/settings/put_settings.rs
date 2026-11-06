use super::{SettingsResponse, SettingsStore, UpdateSettings, get_settings, internal_error};
use crate::database::transaction_with_retry;
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
    if input
        .pricing_interval_hours
        .is_some_and(|h| h.checked_mul(3600).is_none())
        || input
            .discovery_interval_minutes
            .is_some_and(|m| m.checked_mul(60).is_none())
    {
        return (StatusCode::BAD_REQUEST, "Interval is too large").into_response();
    }
    if let Some(days) = input.log_retention_days
        && let Err(err) = logs.save_retention(i64::from(days)).await
    {
        return internal_error(&err);
    }
    if input.pricing_interval_hours.is_some() || input.discovery_interval_minutes.is_some() {
        let store = store.clone();
        let pricing = input.pricing_interval_hours;
        let discovery = input.discovery_interval_minutes;
        let result = transaction_with_retry!(store.database, |mut tx| {
            if let Some(hours) = pricing {
                crate::storage::Setting::upsert_by_name("pricing_interval_hours")
                    .value(hours.to_string())
                    .exec(&mut tx)
                    .await?;
            }
            if let Some(minutes) = discovery {
                crate::storage::Setting::upsert_by_name("discovery_interval_minutes")
                    .value(minutes.to_string())
                    .exec(&mut tx)
                    .await?;
            }
            tx.commit().await?;
            Ok(())
        });
        if let Err(err) = result {
            return internal_error(&err.to_string());
        }
    }
    get_settings(State(store)).await
}
