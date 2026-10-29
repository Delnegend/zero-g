use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, MapAccess, Visitor},
};
use serde_json::{Value, json};

use crate::server::{AppState, settings::SettingsStore};
use crate::storage::Setting;

/// Settings key for model synchronization configuration.
const CONFIG_KEY: &str = "model_settings";
/// Settings key for the cached pricing sheet.
const PRICING_KEY: &str = "model_pricing_datasheet";
/// Settings key for the cached model parameters sheet.
const PARAMETERS_KEY: &str = "model_parameters_datasheet";
// The Bifrost parameters datasheet is tens of megabytes. Bound both the wire
// body and time spent downloading it without rejecting the published sheet.
/// Maximum accepted size of a downloaded sheet.
const MAX_DATASHEET_BYTES: usize = 128 * 1024 * 1024;
/// Overall timeout for downloading a sheet.
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Frequency at which background jobs check their schedules.
const SCHEDULER_POLL: Duration = Duration::from_secs(5);
/// Conflict reported when sheet URLs change during synchronization.
const SETTINGS_CHANGED: &str =
    "Datasheet URLs changed during sync; retry with the current settings";
/// Serializes concurrent datasheet synchronizations.
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// Cached datasheet entries indexed by their source keys.
#[derive(Serialize, utoipa::ToSchema)]
struct ModelData {
    /// Pricing records.
    #[schema(value_type = std::collections::HashMap<String, Value>)]
    pricing: Value,
    /// Model parameter records.
    #[schema(value_type = std::collections::HashMap<String, Value>)]
    parameters: Value,
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
    paths(get_settings, put_settings, sync, get_data),
    components(schemas(ModelSettings, ModelData, ModelSettingsError, ModelSyncResult))
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

/// Download a datasheet within the configured timeout.
async fn fetch_datasheet(input: &str) -> Result<String, String> {
    tokio::time::timeout(FETCH_TIMEOUT, fetch_datasheet_inner(input))
        .await
        .map_err(|_| "Datasheet request timed out")?
}

/// Download and validate a size-limited JSON object without following redirects.
async fn fetch_datasheet_inner(input: &str) -> Result<String, String> {
    let url = validate_url(input)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Unable to create datasheet HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Datasheet request failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Datasheet server returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > u64::try_from(MAX_DATASHEET_BYTES).unwrap_or(u64::MAX))
    {
        return Err("Datasheet exceeds size limit".into());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Datasheet download failed")?;
        if chunk.len() > MAX_DATASHEET_BYTES.saturating_sub(body.len()) {
            return Err("Datasheet exceeds size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| "Datasheet is not valid JSON")?;
    if !value.is_object() {
        return Err("Datasheet JSON must be an object".into());
    }
    drop(value);
    String::from_utf8(body).map_err(|_| "Datasheet is not valid UTF-8".into())
}

/// Fetch both sheets under the synchronization lock and save them together.
async fn sync_datasheets(store: &SettingsStore) -> Result<(), String> {
    let _guard = SYNC_LOCK.lock().await;
    let settings = load_settings(store).await?;
    settings.validate().map_err(str::to_string)?;
    let pricing = fetch_datasheet(&settings.pricing_url)
        .await
        .map_err(|e| format!("Pricing: {e}"))?;
    let parameters = fetch_datasheet(&settings.parameters_url)
        .await
        .map_err(|e| format!("Parameters: {e}"))?;
    save_datasheets(store, &settings, &pricing, &parameters).await
}

/// Atomically save both sheets only if their configured URLs are unchanged.
async fn save_datasheets(
    store: &SettingsStore,
    settings: &ModelSettings,
    pricing: &str,
    parameters: &str,
) -> Result<(), String> {
    // Commit the pair together. A bad download or failed write never destroys
    // either last-known-good sheet.
    let saved = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            let current = Setting::filter_by_name(CONFIG_KEY)
                .first()
                .exec(&mut tx)
                .await?;
            let current = match current {
                Some(row) => serde_json::from_str::<ModelSettings>(&row.value).ok(),
                None => Some(ModelSettings::default()),
            };
            if !current.is_some_and(|current| {
                current.pricing_url == settings.pricing_url
                    && current.parameters_url == settings.parameters_url
            }) {
                return Ok(false);
            }
            Setting::upsert_by_name(PRICING_KEY)
                .value(pricing)
                .exec(&mut tx)
                .await?;
            Setting::upsert_by_name(PARAMETERS_KEY)
                .value(parameters)
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(true)
        })
        .await
        .map_err(|_| "Unable to save datasheets".to_string())?;
    if saved {
        Ok(())
    } else {
        Err(SETTINGS_CHANGED.into())
    }
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

/// Model identifiers used to select relevant datasheet entries.
#[derive(Default)]
struct CatalogModels {
    /// Discovered model identifiers.
    ids: HashSet<String>,
    /// Pairs of provider identifier or kind and model identifier.
    provider_models: HashSet<(String, String)>,
    /// Datasheet keys for Antigravity's reference API prices and their owners.
    reference_keys: HashMap<String, &'static str>,
}

impl CatalogModels {
    /// Collect model identifiers and provider associations from a catalog.
    fn from_catalog(catalog: &Value) -> Self {
        let mut result = Self::default();
        if let Some(providers) = catalog.get("providers").and_then(Value::as_array) {
            for provider in providers {
                if let Some(models) = provider.get("models").and_then(Value::as_array) {
                    for model in models {
                        if let Some(id) = model.get("id").and_then(Value::as_str) {
                            result.ids.insert(id.to_owned());
                            if provider.get("kind").and_then(Value::as_str) == Some("agy") {
                                if id.starts_with("gemini-") {
                                    for owner in ["gemini", "vertex_ai"] {
                                        result
                                            .reference_keys
                                            .insert(format!("{owner}/{id}"), owner);
                                        result
                                            .reference_keys
                                            .insert(format!("{owner}/{id}-preview"), owner);
                                    }
                                } else if id.starts_with("claude-") {
                                    let base = id.strip_suffix("-thinking").unwrap_or(id);
                                    result.reference_keys.insert(base.to_owned(), "anthropic");
                                    result
                                        .reference_keys
                                        .insert(format!("anthropic/{base}"), "anthropic");
                                }
                            }
                            for field in ["id", "kind"] {
                                if let Some(name) = provider.get(field).and_then(Value::as_str) {
                                    result
                                        .provider_models
                                        .insert((name.to_owned(), id.to_owned()));
                                }
                            }
                        }
                    }
                }
            }
        }
        result
    }

    /// Match either the sheet key or its provider and base model.
    fn matches(&self, key: &str, value: &Value) -> bool {
        if self.ids.contains(key) {
            return true;
        }
        if self
            .reference_keys
            .get(key)
            .is_some_and(|owner| value.get("provider").and_then(Value::as_str) == Some(*owner))
        {
            return true;
        }
        match (
            value.get("provider").and_then(Value::as_str),
            value.get("base_model").and_then(Value::as_str),
        ) {
            (Some(provider), Some(model)) => self
                .provider_models
                .contains(&(provider.to_owned(), model.to_owned())),
            _ => false,
        }
    }
}

/// Streaming deserializer retaining only entries relevant to the catalog.
// Parse one entry at a time instead of constructing the entire large sheet.
// Retain original keys so the UI can match exactly as it does with the source.
struct RelevantEntries<'a>(&'a CatalogModels);

impl<'de> DeserializeSeed<'de> for RelevantEntries<'_> {
    type Value = serde_json::Map<String, Value>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for RelevantEntries<'_> {
    type Value = serde_json::Map<String, Value>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a datasheet object")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut selected = serde_json::Map::new();
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            if self.0.matches(&key, &value) {
                selected.insert(key, value);
            }
        }
        Ok(selected)
    }
}

/// Filter a cached datasheet to discovered models.
fn filter_datasheet(sheet: &str, models: &CatalogModels) -> Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_str(sheet);
    let selected = RelevantEntries(models)
        .deserialize(&mut deserializer)
        .map_err(|_| "Invalid cached datasheet")?;
    deserializer.end().map_err(|_| "Invalid cached datasheet")?;
    Ok(Value::Object(selected))
}

/// Return cached pricing and parameters for discovered models.
#[utoipa::path(get, path = "/api/model-settings/data", operation_id = "getModelData",
    responses((status = 200, body = ModelData), (status = 500, body = ModelSettingsError)))]
async fn get_data(State(state): State<AppState>) -> Response {
    async fn read(
        store: &SettingsStore,
        key: &str,
        models: &CatalogModels,
    ) -> Result<Value, String> {
        store
            .setting_get(key)
            .await?
            .map_or_else(|| Ok(json!({})), |value| filter_datasheet(&value, models))
    }
    let catalog = match state.settings.setting_get("model_catalog").await {
        Ok(Some(value)) => match serde_json::from_str::<Value>(&value) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to load cached model catalog",
                );
            }
        },
        Ok(None) => json!({}),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load cached model catalog",
            );
        }
    };
    let models = CatalogModels::from_catalog(&catalog);
    let pricing = read(&state.settings, PRICING_KEY, &models).await;
    let parameters = read(&state.settings, PARAMETERS_KEY, &models).await;
    match (pricing, parameters) {
        (Ok(pricing), Ok(parameters)) => Json(ModelData {
            pricing,
            parameters,
        })
        .into_response(),
        _ => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to load cached datasheets",
        ),
    }
}

/// Build routes for model configuration and metadata.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/model-settings", get(get_settings).put(put_settings))
        .route("/api/model-settings/sync", post(sync))
        .route("/api/model-settings/data", get(get_data))
        .with_state(state)
}

/// Determine whether a nonzero interval has elapsed since the last run.
fn due(last: Option<Instant>, seconds: u64) -> bool {
    seconds != 0 && last.is_none_or(|last| last.elapsed().as_secs() >= seconds)
}

/// Run independent periodic datasheet and provider-discovery loops.
pub(crate) async fn run_background(state: AppState) {
    // Discovery must not wait behind large or slow datasheet downloads.
    let pricing = async {
        let mut last = None;
        loop {
            match load_settings(&state.settings).await {
                Ok(settings) if due(last, settings.pricing_interval_hours.saturating_mul(3600)) => {
                    last = Some(Instant::now());
                    if let Err(message) = sync_datasheets(&state.settings).await {
                        tracing::warn!("Model datasheet sync failed: {message}");
                    }
                }
                Ok(_) => {}
                Err(_) => tracing::warn!("Unable to load model settings for datasheet refresh"),
            }
            tokio::time::sleep(SCHEDULER_POLL).await;
        }
    };
    let discovery = async {
        let mut last = None;
        loop {
            match load_settings(&state.settings).await {
                Ok(settings)
                    if due(last, settings.discovery_interval_minutes.saturating_mul(60)) =>
                {
                    last = Some(Instant::now());
                    if let Err(error) = crate::server::model_catalog::refresh_catalog(&state).await
                    {
                        tracing::error!("Failed to refresh model catalog: {error}");
                    }
                }
                Ok(_) => {}
                Err(_) => tracing::warn!("Unable to load model settings for model discovery"),
            }
            tokio::time::sleep(SCHEDULER_POLL).await;
        }
    };
    tokio::join!(pricing, discovery);
}

/// Model metadata validation, scheduling, and persistence tests.
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

    /// Accept local HTTP datasheets without following redirects.
    #[tokio::test]
    async fn fetches_local_http_and_rejects_redirects() {
        let app = Router::new()
            .route(
                "/data",
                get(|| async { Json(json!({"model": {"price": 1}})) }),
            )
            .route(
                "/redirect",
                get(|| async { axum::response::Redirect::temporary("/data") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let data = fetch_datasheet(&format!("http://{address}/data"))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&data).unwrap(),
            json!({"model": {"price": 1}})
        );
        let error = fetch_datasheet(&format!("http://{address}/redirect"))
            .await
            .unwrap_err();
        assert!(error.contains("HTTP 307"));
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let mut settings = ModelSettings {
            pricing_url: format!("http://{address}/data"),
            parameters_url: format!("http://{address}/data"),
            ..ModelSettings::default()
        };
        let value = serde_json::to_string(&settings).unwrap();
        Setting::upsert_by_name(CONFIG_KEY)
            .value(value)
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        sync_datasheets(&store).await.unwrap();
        let pricing = store.setting_get(PRICING_KEY).await.unwrap().unwrap();
        let parameters = store.setting_get(PARAMETERS_KEY).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&pricing).unwrap(),
            json!({"model": {"price": 1}})
        );
        assert_eq!(parameters, pricing);
        settings.parameters_url = format!("http://{address}/missing");
        let value = serde_json::to_string(&settings).unwrap();
        Setting::upsert_by_name(CONFIG_KEY)
            .value(value)
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        assert!(
            sync_datasheets(&store)
                .await
                .unwrap_err()
                .contains("HTTP 404")
        );
        assert_eq!(
            store.setting_get(PRICING_KEY).await.unwrap().unwrap(),
            pricing
        );
        assert_eq!(
            store.setting_get(PARAMETERS_KEY).await.unwrap().unwrap(),
            parameters
        );
        server.abort();
    }

    /// Zero intervals disable jobs and new schedules run immediately.
    #[test]
    fn disabled_and_initial_schedules() {
        assert!(!due(None, 0));
        assert!(due(None, 60));
        assert!(!due(Some(Instant::now()), 60));
    }

    /// Select datasheet entries corresponding to discovered models.
    #[test]
    fn filters_datasheets_by_catalog() {
        let models = CatalogModels::from_catalog(&json!({
            "providers": [{"id": "my-provider", "kind": "openai", "models": [{"id": "gpt-test"}, {"id": "exact"}]}]
        }));
        let sheet = r#"{
            "exact": {"input_cost_per_token": 1},
            "openai/gpt-test": {"provider": "openai", "base_model": "gpt-test"},
            "other/gpt-test": {"provider": "other", "base_model": "gpt-test"},
            "openai/another": {"provider": "openai", "base_model": "another"}
        }"#;
        let filtered = filter_datasheet(sheet, &models).unwrap();
        assert_eq!(filtered.as_object().unwrap().len(), 2);
        assert!(filtered.get("exact").is_some());
        assert!(filtered.get("openai/gpt-test").is_some());
        assert!(filter_datasheet("[]", &models).is_err());
        assert!(filter_datasheet("{} trailing", &models).is_err());
    }

    /// Antigravity reference prices survive filtering without admitting unrelated models.
    #[test]
    fn retains_antigravity_reference_prices() {
        let models = CatalogModels::from_catalog(&json!({
            "providers": [{"id": "agy", "kind": "agy", "models": [
                {"id": "gemini-3-pro"},
                {"id": "claude-sonnet-4-6-thinking"}
            ]}]
        }));
        let sheet = r#"{
            "gemini/gemini-3-pro-preview": {"provider":"gemini","base_model":"gemini-3-pro","input_cost_per_token":0.000002},
            "vertex_ai/gemini-3-pro-preview": {"provider":"vertex_ai","base_model":"gemini-3-pro"},
            "claude-sonnet-4-6": {"provider":"anthropic","input_cost_per_token":0.000003},
            "gemini/gemini-3-flash-preview": {"provider":"gemini","base_model":"gemini-3-flash"},
            "vertex_ai/gemini-3-pro": {"provider":"other"}
        }"#;
        let filtered = filter_datasheet(sheet, &models).unwrap();
        assert!(filtered.get("gemini/gemini-3-pro-preview").is_some());
        assert!(filtered.get("vertex_ai/gemini-3-pro-preview").is_some());
        assert!(filtered.get("claude-sonnet-4-6").is_some());
        assert!(filtered.get("gemini/gemini-3-flash-preview").is_none());
        assert!(filtered.get("vertex_ai/gemini-3-pro").is_none());
    }

    /// Reject stale downloads without replacing the cached pair.
    #[tokio::test]
    async fn changed_urls_do_not_replace_cached_pair() {
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let original = ModelSettings::default();
        save_datasheets(&store, &original, r#"{"old":1}"#, r#"{"old":2}"#)
            .await
            .unwrap();
        let mut changed = original.clone();
        changed.parameters_url = "https://example.com/new".into();
        let value = serde_json::to_string(&changed).unwrap();
        Setting::upsert_by_name(CONFIG_KEY)
            .value(value)
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        assert_eq!(
            save_datasheets(&store, &original, r#"{"new":1}"#, r#"{"new":2}"#)
                .await
                .unwrap_err(),
            SETTINGS_CHANGED
        );
        assert_eq!(
            store.setting_get(PRICING_KEY).await.unwrap().unwrap(),
            r#"{"old":1}"#
        );
        assert_eq!(
            store.setting_get(PARAMETERS_KEY).await.unwrap().unwrap(),
            r#"{"old":2}"#
        );
        save_datasheets(&store, &changed, r#"{"new":1}"#, r#"{"new":2}"#)
            .await
            .unwrap();
        assert_eq!(
            store.setting_get(PRICING_KEY).await.unwrap().unwrap(),
            r#"{"new":1}"#
        );
        assert_eq!(
            store.setting_get(PARAMETERS_KEY).await.unwrap().unwrap(),
            r#"{"new":2}"#
        );
    }
}
