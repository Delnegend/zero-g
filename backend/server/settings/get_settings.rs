use super::{SettingsResponse, SettingsStore, internal_error};
use crate::database::transaction_with_retry;
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
    /// Loads the global settings.
    async fn load(store: &SettingsStore) -> Result<SettingsResponse, String> {
        transaction_with_retry!(store.database, |mut tx| {
            let retention = Setting::filter_by_name("log_retention_days")
                .first()
                .exec(&mut tx)
                .await?
                .and_then(|row| row.value.parse::<u32>().ok())
                .unwrap_or(30);
            tx.commit().await?;
            Ok(SettingsResponse {
                log_retention_days: retention,
            })
        })
        .map_err(|e| e.to_string())
    }
    match load(&store).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => internal_error(&err),
    }
}
