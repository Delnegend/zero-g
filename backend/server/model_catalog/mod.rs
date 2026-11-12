use std::collections::HashMap;
use std::time::Duration;

use axum::{
    Router,
    routing::{get, post},
};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::{DefaultOnError, VecSkipError, serde_as};

use crate::database::transaction_with_retry;
use crate::openai::Model;
use crate::provider::{catalog::discover, registry};
use crate::server::{AppState, settings::SettingsStore};
use crate::storage::Setting;

/// Read the current catalog.
mod get;
/// Refresh the catalog on demand.
mod refresh;

use get::__path_get_catalog;
use refresh::__path_post_refresh;

/// Settings key for the persisted discovery snapshot.
const CACHE_KEY: &str = "model_catalog";
/// Maximum time allowed for one provider discovery.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discovered models and the latest discovery error for a provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedProvider {
    /// Registry identifier.
    id: String,
    /// Display name from the registry.
    name: String,
    /// Provider implementation kind.
    kind: String,
    /// Endpoint used to decide whether cached results remain valid.
    #[serde(default)]
    base_url: String,
    /// Last successfully discovered model records.
    models: Vec<Model>,
    /// Most recent discovery failure, if any.
    error: Option<String>,
}

/// Persisted snapshot of provider discovery results.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct CachedCatalog {
    /// Providers reconciled with the registry on load.
    providers: Vec<CachedProvider>,
}

/// Catalog response enriched with current datasheet metadata.
#[derive(Serialize, utoipa::ToSchema)]
struct ModelCatalog {
    /// Providers reconciled with the registry.
    providers: Vec<CatalogProvider>,
    /// Error loading cached datasheets, if any.
    #[schema(required = true)]
    datasheets_error: Option<String>,
}

/// Discovered provider and normalized models.
#[derive(Serialize, utoipa::ToSchema)]
struct CatalogProvider {
    /// Registry identifier.
    id: String,
    /// Registry display name.
    name: String,
    /// Provider implementation kind.
    kind: String,
    /// Endpoint used for discovery.
    base_url: String,
    /// Enriched discovery records.
    models: Vec<CatalogModel>,
    /// Most recent discovery failure.
    #[schema(required = true)]
    error: Option<String>,
}

/// A raw model and its normalized dashboard metadata.
#[derive(Serialize, utoipa::ToSchema)]
struct CatalogModel {
    /// Original provider model, including extra metadata.
    model: Model,
    /// Human-readable name.
    display_name: String,
    /// Description when provided.
    #[schema(required = true)]
    description: Option<String>,
    /// Supported reasoning effort values.
    reasoning_efforts: Vec<String>,
    /// Reasoning capability, if known.
    #[schema(required = true)]
    reasoning: Option<bool>,
    /// Tool calling capability, if known.
    #[schema(required = true)]
    tool_call: Option<bool>,
    /// Supported input modalities.
    input_modalities: Vec<String>,
    /// Prices in USD per million tokens.
    #[schema(required = true)]
    pricing: Option<CatalogPricing>,
    /// Original pricing record.
    #[schema(required = true, value_type = Option<Object>)]
    raw_pricing: Option<Value>,
    /// Original parameters record.
    #[schema(required = true, value_type = Option<Object>)]
    parameters: Option<Value>,
    /// Original provider-specific metadata.
    #[schema(required = true, value_type = Option<Object>)]
    provider_metadata: Option<Value>,
}

/// Normalized prices in USD per million tokens.
#[derive(Serialize, utoipa::ToSchema)]
struct CatalogPricing {
    /// Input price.
    #[schema(required = true)]
    input: Option<f64>,
    /// Output price.
    #[schema(required = true)]
    output: Option<f64>,
    /// Cache write price.
    #[schema(required = true)]
    cache_write: Option<f64>,
    /// Cache read price.
    #[schema(required = true)]
    cache_read: Option<f64>,
    /// Source and interpretation of the displayed prices.
    source: String,
}

/// Zen model details, tolerating malformed individual fields.
#[serde_as]
#[derive(Default, Deserialize)]
struct ZenMetadata {
    /// Whether the model supports reasoning.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    reasoning: Option<bool>,
    /// Whether the model supports tool calls.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    tool_call: Option<bool>,
    /// Advertised modalities.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    modalities: Option<ZenModalities>,
}

/// Zen modalities relevant to the catalog.
#[serde_as]
#[derive(Default, Deserialize)]
struct ZenModalities {
    /// Input modalities, ignoring non-string entries.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError<VecSkipError<_>>")]
    input: Vec<String>,
}

/// Price fields from Zen or a per-token datasheet.
#[serde_as]
#[derive(Default, Deserialize)]
struct PriceFields {
    /// Zen input price per million tokens.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    input: Option<f64>,
    /// Zen output price per million tokens.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    output: Option<f64>,
    /// Zen cache write price per million tokens.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    cache_write: Option<f64>,
    /// Zen cache read price per million tokens.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    cache_read: Option<f64>,
    /// Datasheet input price per token.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    input_cost_per_token: Option<f64>,
    /// Datasheet output price per token.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    output_cost_per_token: Option<f64>,
    /// Datasheet cache write price per token.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    cache_creation_input_token_cost: Option<f64>,
    /// Datasheet cache read price per token.
    #[serde(default)]
    #[serde_as(as = "DefaultOnError")]
    cache_read_input_token_cost: Option<f64>,
}

/// Model discovery dashboard operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_catalog, post_refresh),
    components(schemas(ModelCatalog, CatalogProvider, CatalogModel, CatalogPricing, Model))
)]
pub(crate) struct Api;

/// Load the snapshot and reconcile it against the current registry.
// Reconcile with the registry on every read: new providers appear immediately and
// deleted providers never leak out of an old persisted snapshot.
async fn load(store: &SettingsStore) -> Result<CachedCatalog, String> {
    let cached = match store.setting_get(CACHE_KEY).await? {
        Some(value) => serde_json::from_str::<CachedCatalog>(&value).map_err(|e| e.to_string())?,
        None => CachedCatalog::default(),
    };
    let mut cached: HashMap<_, _> = cached
        .providers
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let providers = registry::list(&store.database)
        .await?
        .into_iter()
        .map(|p| {
            let prior = cached
                .remove(&p.id)
                .filter(|old| old.kind == p.kind && old.base_url == p.base_url);
            let (models, error) = prior.map(|old| (old.models, old.error)).unwrap_or_default();
            CachedProvider {
                id: p.id,
                name: p.name,
                kind: p.kind,
                base_url: p.base_url,
                models,
                error,
            }
        })
        .collect();
    Ok(CachedCatalog { providers })
}

/// Persist a catalog snapshot transactionally.
async fn save(store: &SettingsStore, catalog: &CachedCatalog) -> Result<(), String> {
    let value = serde_json::to_string(catalog).map_err(|e| e.to_string())?;
    transaction_with_retry!(store.database, |mut tx| {
        Setting::upsert_by_name(CACHE_KEY)
            .value(value.as_str())
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// Discover providers concurrently and retain last successful models on failure.
pub(super) async fn refresh_catalog(state: &AppState) -> Result<CachedCatalog, String> {
    let current = registry::list(&state.settings.database).await?;
    let mut catalog = load(&state.settings).await?;
    let results = join_all(current.iter().map(|provider| async move {
        tokio::time::timeout(DISCOVERY_TIMEOUT, discover(state, provider))
            .await
            .unwrap_or_else(|_| Err("Provider model discovery timed out".into()))
    }))
    .await;
    let mut results: HashMap<_, _> = current
        .into_iter()
        .zip(results)
        .map(|(p, r)| (p.id, r))
        .collect();
    for provider in &mut catalog.providers {
        if let Some(result) = results.remove(&provider.id) {
            match result {
                Ok(models) => {
                    provider.models = models;
                    provider.error = None;
                }
                Err(error) => provider.error = Some(error),
            }
        }
    }
    save(&state.settings, &catalog).await?;
    // Registry may change while network calls are running.
    load(&state.settings).await
}

/// Find a record using the dashboard's provider-specific matching precedence.
fn lookup<'a>(sheet: &'a Value, provider: &CachedProvider, id: &str) -> Option<&'a Value> {
    for key in [
        format!("{}/{id}", provider.id),
        format!("{}/{id}", provider.kind),
        id.to_owned(),
    ] {
        let Some(entry) = sheet.get(&key).filter(|entry| entry.is_object()) else {
            continue;
        };
        let Some(owner) = entry.get("provider").and_then(Value::as_str) else {
            continue;
        };
        if owner != provider.id && owner != provider.kind {
            continue;
        }
        if key == id && id.contains('/') && !id.starts_with(&format!("{owner}/")) {
            continue;
        }
        return Some(entry);
    }
    None
}

/// Match only explicit reference prices and declared Gemini preview aliases.
fn reference_price<'a>(sheet: &'a Value, id: &str) -> Option<&'a Value> {
    let candidates: Vec<(String, &str, bool)> = if id.starts_with("gemini-") {
        vec![
            (format!("gemini/{id}"), "gemini", false),
            (format!("vertex_ai/{id}"), "vertex_ai", false),
            (format!("gemini/{id}-preview"), "gemini", true),
            (format!("vertex_ai/{id}-preview"), "vertex_ai", true),
        ]
    } else if id.starts_with("claude-") {
        let base = id.strip_suffix("-thinking").unwrap_or(id);
        vec![
            (base.to_owned(), "anthropic", false),
            (format!("anthropic/{base}"), "anthropic", false),
        ]
    } else {
        Vec::new()
    };
    candidates.into_iter().find_map(|(key, owner, alias)| {
        let entry = sheet.get(&key).filter(|entry| entry.is_object())?;
        (entry.get("provider").and_then(Value::as_str) == Some(owner)
            && (!alias || entry.get("base_model").and_then(Value::as_str) == Some(id)))
        .then_some(entry)
    })
}

/// Match an exact model ID against the specified API provider's reference data.
fn api_reference<'a>(sheet: &'a Value, owner: &str, id: &str) -> Option<&'a Value> {
    let find = |key: &str| {
        let entry = sheet.get(key).filter(|entry| entry.is_object())?;
        (entry.get("provider").and_then(Value::as_str) == Some(owner)).then_some(entry)
    };
    find(id).or_else(|| find(&format!("{owner}/{id}")))
}

/// Convert a pricing record to USD per million tokens.
fn normalize_pricing(kind: &str, price: &Value) -> CatalogPricing {
    let zen = kind == "zen";
    let fields = PriceFields::deserialize(price).unwrap_or_default();
    let number = |value: Option<f64>| {
        value
            .map(|n| n * if zen { 1.0 } else { 1_000_000.0 })
            .filter(|n| n.is_finite())
    };
    CatalogPricing {
        input: number(if zen {
            fields.input
        } else {
            fields.input_cost_per_token
        }),
        output: number(if zen {
            fields.output
        } else {
            fields.output_cost_per_token
        }),
        cache_write: number(if zen {
            fields.cache_write
        } else {
            fields.cache_creation_input_token_cost
        }),
        cache_read: number(if zen {
            fields.cache_read
        } else {
            fields.cache_read_input_token_cost
        }),
        source: match zen {
            true => "models.dev",
            false if kind == "agy" => "antigravity-reference",
            false => "datasheet",
        }
        .to_owned(),
    }
}

/// Select provider prices first, then an appropriately scoped datasheet record.
fn catalog_pricing(
    provider: &CachedProvider,
    model: &Model,
    data: &super::model_setting::ModelData,
    supplied: Option<crate::provider::common::catalog::CatalogPrices>,
) -> (Option<CatalogPricing>, Option<Value>) {
    if let Some(price) = supplied {
        return (
            Some(CatalogPricing {
                input: price.input,
                output: price.output,
                cache_write: price.cache_write,
                cache_read: price.cache_read,
                source: price.source.to_owned(),
            }),
            Some(price.raw),
        );
    }
    let (price, reference) = if provider.kind == "zen" {
        (
            model
                .extra
                .get("zen_metadata")
                .and_then(|value| value.get("cost"))
                .filter(|value| value.is_object()),
            false,
        )
    } else if let Some(price) = lookup(&data.pricing, provider, &model.id) {
        (Some(price), false)
    } else if provider.kind == "agy" {
        (reference_price(&data.pricing, &model.id), false)
    } else if provider.kind == "claude" {
        (api_reference(&data.pricing, "anthropic", &model.id), true)
    } else if provider.kind == "codex" {
        (api_reference(&data.pricing, "openai", &model.id), true)
    } else {
        (None, false)
    };
    (
        price.map(|value| {
            let mut price = normalize_pricing(&provider.kind, value);
            if reference {
                price.source = format!("{}-reference", provider.kind);
            }
            price
        }),
        price.cloned(),
    )
}

/// Extract provider-specific reference details from matching datasheet records.
fn catalog_reference(
    provider: &CachedProvider,
    model: &Model,
    data: &super::model_setting::ModelData,
) -> crate::provider::common::catalog::CatalogInfo {
    match provider.kind.as_str() {
        "claude" => api_reference(&data.parameters, "anthropic", &model.id)
            .map(crate::provider::claude::catalog_reference),
        "codex" => [
            lookup(&data.parameters, provider, &model.id),
            api_reference(&data.parameters, "openai", &model.id),
            api_reference(&data.pricing, "openai", &model.id),
        ]
        .into_iter()
        .flatten()
        .map(crate::provider::codex::models::catalog_reference)
        .find(|reference| reference.input_modalities.is_some()),
        _ => None,
    }
    .unwrap_or_default()
}

/// Normalize one model using filtered datasheets.
fn normalize_model(
    provider: &CachedProvider,
    mut model: Model,
    data: &super::model_setting::ModelData,
) -> CatalogModel {
    let info = match provider.kind.as_str() {
        "zen" | "go" => crate::provider::zen::catalog::catalog_info(&model, &provider.kind),
        "claude" => crate::provider::claude::catalog_info(&model),
        "codex" => crate::provider::codex::models::catalog_info(&model),
        "openrouter" => crate::provider::openrouter::catalog_info(&model),
        "chatjimmy" => crate::provider::chatjimmy::catalog_info(&model),
        _ => crate::provider::common::catalog::CatalogInfo::default(),
    };
    let zen = (provider.kind == "zen")
        .then(|| {
            model
                .extra
                .get("zen_metadata")
                .filter(|value| value.is_object())
        })
        .flatten();
    let (pricing, raw_pricing) = catalog_pricing(provider, &model, data, info.pricing);
    let display_name = model
        .name
        .as_ref()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            model
                .display_name
                .as_ref()
                .filter(|name| !name.trim().is_empty())
        })
        .unwrap_or(&model.id)
        .clone();
    let description = model
        .extra
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let metadata = zen
        .and_then(|value| ZenMetadata::deserialize(value).ok())
        .unwrap_or_default();
    let reference = catalog_reference(provider, &model, data);
    let reasoning = info
        .reasoning
        .or(model.reasoning)
        .or(reference.reasoning)
        .or(metadata.reasoning);
    let tool_call = info
        .tool_call
        .or(reference.tool_call)
        .or(metadata.tool_call);
    let input_modalities = info
        .input_modalities
        .or(reference.input_modalities)
        .unwrap_or_else(|| {
            if provider.kind == "agy" {
                crate::provider::antigravity::models::input_modalities(&model.extra)
            } else {
                metadata
                    .modalities
                    .map(|modalities| modalities.input)
                    .unwrap_or_default()
            }
        });
    let reasoning_efforts = model
        .supported_reasoning_efforts
        .clone()
        .or(info.reasoning_efforts)
        .unwrap_or_default();
    let parameters = lookup(&data.parameters, provider, &model.id)
        .or_else(|| match provider.kind.as_str() {
            "claude" => api_reference(&data.parameters, "anthropic", &model.id),
            "codex" => api_reference(&data.parameters, "openai", &model.id),
            _ => None,
        })
        .cloned();
    let provider_metadata = zen.cloned();
    model.context_window = model
        .context_window
        .or(info.context_window)
        .or(reference.context_window);
    model.max_output_tokens = model
        .max_output_tokens
        .or(info.max_output_tokens)
        .or(reference.max_output_tokens);
    CatalogModel {
        model,
        display_name,
        description,
        reasoning_efforts,
        reasoning,
        tool_call,
        input_modalities,
        pricing,
        raw_pricing,
        parameters,
        provider_metadata,
    }
}

/// Enrich a raw snapshot from the latest cached sheets without altering persistence.
async fn normalize_catalog(store: &SettingsStore, catalog: CachedCatalog) -> ModelCatalog {
    let models = super::model_setting::CatalogModels::from_entries(
        catalog.providers.iter().flat_map(|provider| {
            provider.models.iter().map(|model| {
                (
                    provider.id.as_str(),
                    provider.kind.as_str(),
                    model.id.as_str(),
                )
            })
        }),
    );
    let (data, datasheets_error) = match super::model_setting::catalog_data(store, &models).await {
        Ok(data) => (data, None),
        Err(error) => {
            tracing::warn!("Unable to load cached model datasheets: {error}");
            (
                super::model_setting::ModelData::default(),
                Some("Unable to load cached datasheets".to_owned()),
            )
        }
    };
    let mut result = enrich_catalog(catalog, &data);
    result.datasheets_error = datasheets_error;
    result
}

/// Build the dashboard view from a discovery snapshot and its filtered datasheets.
fn enrich_catalog(catalog: CachedCatalog, data: &super::model_setting::ModelData) -> ModelCatalog {
    ModelCatalog {
        datasheets_error: None,
        providers: catalog
            .providers
            .into_iter()
            .map(|mut provider| {
                let models = std::mem::take(&mut provider.models)
                    .into_iter()
                    .map(|model| normalize_model(&provider, model, data))
                    .collect();
                CatalogProvider {
                    id: provider.id,
                    name: provider.name,
                    kind: provider.kind,
                    base_url: provider.base_url,
                    models,
                    error: provider.error,
                }
            })
            .collect(),
    }
}

/// Build dashboard routes for catalog access and refresh.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/model-catalog", get(get::get_catalog))
        .route("/api/model-catalog/refresh", post(refresh::post_refresh))
        .with_state(state)
}

/// Catalog persistence and registry reconciliation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
