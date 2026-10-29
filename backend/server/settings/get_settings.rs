use super::{SettingsResponse, SettingsStore, internal_error};
use crate::database::transaction_with_retry;
use crate::storage::{Secret, Setting};
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};

/// Returns current settings and whether an OAuth secret is configured.
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
    /// Loads the settings and validates any stored OAuth secret.
    async fn load(store: &SettingsStore) -> Result<SettingsResponse, String> {
        transaction_with_retry!(store.database, |mut tx| {
            let retention = Setting::filter_by_name("log_retention_days")
                .first()
                .exec(&mut tx)
                .await?
                .and_then(|row| row.value.parse::<u32>().ok())
                .unwrap_or(30);
            let client_id = Setting::filter_by_name("oauth_client_id")
                .first()
                .exec(&mut tx)
                .await?
                .map_or_else(String::new, |row| row.value);
            let secret = Secret::filter_by_name("oauth_client_secret")
                .first()
                .exec(&mut tx)
                .await?;
            if let Some(row) = &secret
                && let Err(err) = store.decrypt("oauth_client_secret", &row.value)
            {
                return Ok(Err(err));
            }
            tx.commit().await?;
            Ok(Ok(SettingsResponse {
                log_retention_days: retention,
                oauth_client_id: client_id,
                oauth_client_secret_set: secret.is_some(),
            }))
        })
        .map_err(|e| e.to_string())?
    }
    match load(&store).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => internal_error(&err),
    }
}
