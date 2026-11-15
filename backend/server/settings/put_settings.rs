use super::keys::{
    BODY_RETENTION_DAYS, DISCOVERY_INTERVAL_MINUTES, IntSetting, LOG_RETENTION_DAYS,
    PRICING_INTERVAL_HOURS,
};
use super::{SettingsResponse, SettingsStore, UpdateSettings, get_settings, internal_error};
use crate::database::transaction_with_retry;
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
    State(store): State<SettingsStore>,
    Json(input): Json<UpdateSettings>,
) -> Response {
    let updates: [(&IntSetting, Option<u64>, &str); 4] = [
        (
            &LOG_RETENTION_DAYS,
            input.log_retention_days.map(u64::from),
            "Invalid log retention",
        ),
        (
            &BODY_RETENTION_DAYS,
            input.body_retention_days.map(u64::from),
            "Invalid body retention",
        ),
        (
            &PRICING_INTERVAL_HOURS,
            input.pricing_interval_hours,
            "Interval is too large",
        ),
        (
            &DISCOVERY_INTERVAL_MINUTES,
            input.discovery_interval_minutes,
            "Interval is too large",
        ),
    ];
    for (setting, value, message) in &updates {
        if value.is_some_and(|value| !setting.range.contains(&value)) {
            return (StatusCode::BAD_REQUEST, *message).into_response();
        }
    }
    if updates.iter().any(|(_, value, _)| value.is_some()) {
        let result = transaction_with_retry!(store.database, |mut tx| {
            for (setting, value, _) in &updates {
                if let Some(value) = value {
                    setting.write(&mut tx, *value).await?;
                }
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
