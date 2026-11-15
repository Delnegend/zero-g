use super::keys::{
    ALL, BODY_RETENTION_DAYS, DISCOVERY_INTERVAL_MINUTES, LOG_RETENTION_DAYS,
    PRICING_INTERVAL_HOURS,
};
use super::{SettingsResponse, SettingsStore, internal_error};
use crate::storage::Setting;
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};

/// Returns current global settings.
#[utoipa::path(
    get,
    path = "/api/settings",
    operation_id = "getSettings",
    responses(
        (status = 200, description = "Current settings", body = SettingsResponse),
        (status = 500, description = "Settings store error")
    )
)]
pub(crate) async fn get_settings(State(store): State<SettingsStore>) -> Response {
    /// Loads every integer setting in one query.
    async fn load(store: &SettingsStore) -> toasty::Result<SettingsResponse> {
        let rows = Setting::filter(
            Setting::fields()
                .name()
                .in_list(ALL.map(|setting| setting.name.to_owned()).to_vec()),
        )
        .exec(&mut (*store.database.db).clone())
        .await?;
        let value = |setting: &super::keys::IntSetting| {
            setting.parse(
                rows.iter()
                    .find(|row| row.name == setting.name)
                    .map(|row| row.value.as_str()),
            )
        };
        let days = |setting| {
            value(setting).and_then(|days| {
                u32::try_from(days).map_err(|e| toasty::Error::from_args(format_args!("{e}")))
            })
        };
        Ok(SettingsResponse {
            log_retention_days: days(&LOG_RETENTION_DAYS)?,
            body_retention_days: days(&BODY_RETENTION_DAYS)?,
            pricing_interval_hours: value(&PRICING_INTERVAL_HOURS)?,
            discovery_interval_minutes: value(&DISCOVERY_INTERVAL_MINUTES)?,
        })
    }
    match load(&store).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}
