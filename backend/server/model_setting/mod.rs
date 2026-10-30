use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::server::{AppState, settings::SettingsStore};

/// Datasheet synchronization and catalog filtering.
mod datasheet;
/// Read model settings.
mod get;
/// Background model refresh scheduling.
mod schedule;
/// Synchronize model datasheets.
mod sync;
/// Update model settings.
mod update;

pub(super) use datasheet::{CatalogModels, ModelData, catalog_data};
use get::__path_get_settings;
pub(crate) use schedule::run_background;
use sync::__path_sync;
use update::__path_put_settings;

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

/// Build routes for model configuration and metadata.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/model-settings",
            get(get::get_settings).put(update::put_settings),
        )
        .route("/api/model-settings/sync", post(sync::sync))
        .with_state(state)
}

/// Shared in-memory gateway for endpoint behavior tests.
#[cfg(test)]
mod test_support {
    use crate::{
        database::Database,
        provider::Providers,
        server::{
            AppState, dashboard_auth_config, observability::LogStore, settings::SettingsStore,
        },
    };

    /// Create an isolated gateway state.
    pub(super) async fn state() -> AppState {
        let database = Database::open(":memory:").await.unwrap();
        let settings = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        AppState {
            providers: Providers::new(),
            settings,
            logs: LogStore::new(database),
            dashboard_auth: dashboard_auth_config::test_auth().await,
        }
    }
}
