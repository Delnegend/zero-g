use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::server::{AppState, settings::SettingsStore};
use crate::storage::Setting;

/// Datasheet synchronization and catalog filtering.
mod datasheet;
/// Background model refresh scheduling.
mod schedule;

pub(super) use datasheet::{CatalogModels, ModelData, catalog_data};
use datasheet::{SETTINGS_CHANGED, sync_datasheets};
pub(crate) use schedule::run_background;

/// Settings key for model synchronization configuration.
const CONFIG_KEY: &str = "model_settings";

/// URLs and intervals governing model metadata refreshes.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct ModelSettings {
    /// Pricing datasheet URL.
    pricing_url: String,
    /// Model parameters datasheet URL.
    parameters_url: String,
    /// Datasheet refresh interval in hours; zero disables refresh.
    pricing_interval_hours: u64,
    /// Provider discovery interval in minutes; zero disables discovery.
    discovery_interval_minutes: u64,
}

/// Model settings API error.
#[derive(Serialize, utoipa::ToSchema)]
struct ModelSettingsError {
    /// Error message.
    error: String,
}

/// Successful datasheet synchronization.
#[derive(Serialize, utoipa::ToSchema)]
struct ModelSyncResult {
    /// Whether synchronization succeeded.
    ok: bool,
}

/// Model synchronization and cached datasheet operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_settings, put_settings, sync),
    components(schemas(ModelSettings, ModelSettingsError, ModelSyncResult))
)]
pub(crate) struct Api;

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            pricing_url: "https://getbifrost.ai/datasheet".into(),
            parameters_url: "https://getbifrost.ai/datasheet/model-parameters".into(),
            pricing_interval_hours: 24,
            discovery_interval_minutes: 60,
        }
    }
}

impl ModelSettings {
    /// Validate URLs and ensure intervals fit when converted to seconds.
    fn validate(&self) -> Result<(), &'static str> {
        validate_url(&self.pricing_url)?;
        validate_url(&self.parameters_url)?;
        if self.pricing_interval_hours.checked_mul(3600).is_none()
            || self.discovery_interval_minutes.checked_mul(60).is_none()
        {
            return Err("Interval is too large");
        }
        Ok(())
    }
}

/// Accept only HTTP(S) URLs without credentials or fragments.
fn validate_url(input: &str) -> Result<reqwest::Url, &'static str> {
    let url =
        reqwest::Url::parse(input).map_err(|_| "Datasheet URL must be a valid HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Datasheet URL must be HTTP(S) without userinfo or a fragment");
    }
    Ok(url)
}

/// Load saved configuration or use defaults when none exists.
async fn load_settings(store: &SettingsStore) -> Result<ModelSettings, String> {
    store
        .setting_get(CONFIG_KEY)
        .await
        .map_err(|_| "Unable to load model settings")?
        .map_or_else(
            || Ok(ModelSettings::default()),
            |value| {
                serde_json::from_str(&value)
                    .map_err(|_| "Stored model settings are invalid".to_string())
            },
        )
}

/// Return the current model synchronization settings.
#[utoipa::path(get, path = "/api/model-settings", operation_id = "getModelSettings",
    responses((status = 200, body = ModelSettings), (status = 500, body = ModelSettingsError)))]
async fn get_settings(State(state): State<AppState>) -> Response {
    load_settings(&state.settings).await.map_or_else(
        |_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load model settings",
            )
        },
        |settings| Json(settings).into_response(),
    )
}

/// Validate and persist model synchronization settings.
#[utoipa::path(put, path = "/api/model-settings", operation_id = "updateModelSettings",
    request_body = ModelSettings,
    responses((status = 200, body = ModelSettings), (status = 400, body = ModelSettingsError),
        (status = 500, body = ModelSettingsError)))]
async fn put_settings(
    State(state): State<AppState>,
    Json(mut settings): Json<ModelSettings>,
) -> Response {
    let defaults = ModelSettings::default();
    if settings.pricing_url.trim().is_empty() {
        settings.pricing_url = defaults.pricing_url;
    }
    if settings.parameters_url.trim().is_empty() {
        settings.parameters_url = defaults.parameters_url;
    }
    if let Err(message) = settings.validate() {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let Ok(value) = serde_json::to_string(&settings) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to serialize model settings",
        );
    };
    let result = state
        .settings
        .database
        .retry(|| async {
            let mut db = (*state.settings.database.db).clone();
            let mut tx = db.transaction().await?;
            Setting::upsert_by_name(CONFIG_KEY)
                .value(value.as_str())
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await;
    if result.is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save model settings",
        );
    }
    Json(settings).into_response()
}

/// Construct a JSON error response.
fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(ModelSettingsError {
            error: message.to_owned(),
        }),
    )
        .into_response()
}

/// Handle an on-demand datasheet synchronization.
#[utoipa::path(post, path = "/api/model-settings/sync", operation_id = "syncModelSettings",
    responses((status = 200, body = ModelSyncResult), (status = 409, body = ModelSettingsError),
        (status = 502, body = ModelSettingsError)))]
async fn sync(State(state): State<AppState>) -> Response {
    match sync_datasheets(&state.settings).await {
        Ok(()) => Json(ModelSyncResult { ok: true }).into_response(),
        Err(message) if message == SETTINGS_CHANGED => error(StatusCode::CONFLICT, &message),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

/// Build routes for model configuration and metadata.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/model-settings", get(get_settings).put(put_settings))
        .route("/api/model-settings/sync", post(sync))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reject invalid URLs and overflowing refresh intervals.
    #[test]
    fn validates_urls_and_intervals() {
        let mut settings = ModelSettings::default();
        assert!(settings.validate().is_ok());
        settings.pricing_url = "http://127.0.0.1/data".into();
        assert!(settings.validate().is_ok());
        settings.pricing_url = "ftp://example.com/data".into();
        assert!(settings.validate().is_err());
        settings.pricing_url = "https://user:secret@example.com/data".into();
        assert!(settings.validate().is_err());
        settings.pricing_url = "https://example.com/data#fragment".into();
        assert!(settings.validate().is_err());
        settings.pricing_url = "https://example.com/data".into();
        settings.pricing_interval_hours = u64::MAX;
        assert!(settings.validate().is_err());
    }
}
