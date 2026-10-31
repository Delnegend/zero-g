use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::StreamExt;
use serde::de::{DeserializeSeed, MapAccess, Visitor};
use serde_json::{Value, json};

use super::{CONFIG_KEY, ModelSettings, load_settings, validate_url};
use crate::database::transaction_with_retry;
use crate::server::settings::SettingsStore;
use crate::storage::Setting;

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

/// Conflict reported when sheet URLs change during synchronization.
pub(super) const SETTINGS_CHANGED: &str =
    "Datasheet URLs changed during sync; retry with the current settings";
/// Serializes concurrent datasheet synchronizations.
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Cached datasheet entries indexed by their source keys.
#[derive(Default)]
pub(in crate::server) struct ModelData {
    /// Pricing records.
    pub(in crate::server) pricing: Value,
    /// Model parameter records.
    pub(in crate::server) parameters: Value,
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
pub(super) async fn sync_datasheets(store: &SettingsStore) -> Result<(), String> {
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
    let saved = transaction_with_retry!(store.database, |mut tx| {
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
    .map_err(|_| "Unable to save datasheets".to_string())?;
    if saved {
        Ok(())
    } else {
        Err(SETTINGS_CHANGED.into())
    }
}

/// Model identifiers used to select relevant datasheet entries.
#[derive(Default)]
pub(in crate::server) struct CatalogModels {
    /// Discovered model identifiers.
    ids: HashSet<String>,
    /// Pairs of provider identifier or kind and model identifier.
    provider_models: HashSet<(String, String)>,
    /// Datasheet keys for Antigravity's reference API prices and their owners.
    reference_keys: HashMap<String, &'static str>,
}

impl CatalogModels {
    /// Collect model identifiers and provider associations from a catalog.
    pub(in crate::server) fn from_entries<'a>(
        entries: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    ) -> Self {
        let mut result = Self::default();
        for (provider_id, kind, id) in entries {
            result.ids.insert(id.to_owned());
            if kind == "agy" {
                if id.starts_with("gemini-") {
                    for owner in ["gemini", "vertex_ai"] {
                        result.reference_keys.insert(format!("{owner}/{id}"), owner);
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
            if kind == "claude" {
                result.reference_keys.insert(id.to_owned(), "anthropic");
                result
                    .reference_keys
                    .insert(format!("anthropic/{id}"), "anthropic");
            }
            for name in [provider_id, kind] {
                result
                    .provider_models
                    .insert((name.to_owned(), id.to_owned()));
            }
        }
        result
    }

    /// Match either the sheet key or its provider and base model.
    fn matches(&self, key: &str, value: &Value) -> bool {
        if self.ids.contains(key) {
            return true;
        }
        if key.split_once('/').is_some_and(|(provider, model)| {
            self.provider_models
                .contains(&(provider.to_owned(), model.to_owned()))
        }) {
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

/// Load each cached sheet once, retaining only records relevant to this catalog.
pub(in crate::server) async fn catalog_data(
    store: &SettingsStore,
    models: &CatalogModels,
) -> Result<ModelData, String> {
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
    Ok(ModelData {
        pricing: read(store, PRICING_KEY, models).await?,
        parameters: read(store, PARAMETERS_KEY, models).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};

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

    /// Select datasheet entries corresponding to discovered models.
    #[test]
    fn filters_datasheets_by_catalog() {
        let models = CatalogModels::from_entries([
            ("my-provider", "openai", "gpt-test"),
            ("my-provider", "openai", "exact"),
        ]);
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
        let models = CatalogModels::from_entries([
            ("agy", "agy", "gemini-3-pro"),
            ("agy", "agy", "claude-sonnet-4-6-thinking"),
        ]);
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
