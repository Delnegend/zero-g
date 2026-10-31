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

/// Match an exact Claude model ID against Anthropic-owned API reference data.
fn claude_reference<'a>(sheet: &'a Value, id: &str) -> Option<&'a Value> {
    [id.to_owned(), format!("anthropic/{id}")]
        .into_iter()
        .find_map(|key| {
            let entry = sheet.get(&key).filter(|entry| entry.is_object())?;
            (entry.get("provider").and_then(Value::as_str) == Some("anthropic")).then_some(entry)
        })
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
        (claude_reference(&data.pricing, &model.id), true)
    } else {
        (None, false)
    };
    (
        price.map(|value| {
            let mut price = normalize_pricing(&provider.kind, value);
            if reference {
                "claude-reference".clone_into(&mut price.source);
            }
            price
        }),
        price.cloned(),
    )
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
        "openrouter" => crate::provider::openrouter::catalog_info(&model),
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
    let reference = (provider.kind == "claude")
        .then(|| claude_reference(&data.parameters, &model.id))
        .flatten()
        .map(crate::provider::claude::catalog_reference)
        .unwrap_or_default();
    let reasoning = info
        .reasoning
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
        .or_else(|| {
            (provider.kind == "claude")
                .then(|| claude_reference(&data.parameters, &model.id))
                .flatten()
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
mod tests {
    use super::*;
    use crate::database::Database;
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use serde_json::Value;
    use serde_json::json;
    use tower::ServiceExt;

    fn sample_provider(kind: &str, id: &str) -> CachedProvider {
        CachedProvider {
            id: id.into(),
            name: id.into(),
            kind: kind.into(),
            base_url: String::new(),
            models: Vec::new(),
            error: None,
        }
    }

    fn sample_data(pricing: Value, parameters: Value) -> super::super::model_setting::ModelData {
        super::super::model_setting::ModelData {
            pricing,
            parameters,
        }
    }

    /// Reference prices and capabilities apply only to exact Anthropic-owned models.
    #[tokio::test]
    async fn claude_catalog_uses_exact_reference_data_without_overriding_live_fields() {
        let store = store().await;
        Setting::upsert_by_name("model_pricing_datasheet")
            .value(json!({
                "anthropic/claude-example": {"provider":"anthropic","input_cost_per_token":0.000_001,"output_cost_per_token":0.000_005},
                "claude-other": {"provider":"bedrock","input_cost_per_token":0.000_099}
            }).to_string())
            .exec(&mut (*store.database.db).clone())
            .await.unwrap();
        Setting::upsert_by_name("model_parameters_datasheet")
            .value(json!({
                "anthropic/claude-example": {"provider":"anthropic","mode":"chat","max_input_tokens":200_000,"max_output_tokens":64000,"supports_reasoning":true,"supports_function_calling":true,"supports_vision":true,"supports_pdf_input":true},
                "claude-other": {"provider":"bedrock","max_input_tokens":123}
            }).to_string())
            .exec(&mut (*store.database.db).clone())
            .await.unwrap();
        let mut provider = sample_provider("claude", "claude");
        provider.models = vec![
            serde_json::from_value(json!({"id":"claude-example","max_input_tokens":100_000,"capabilities":{"thinking":{"supported":false}}})).unwrap(),
            serde_json::from_value(json!({"id":"claude-other"})).unwrap(),
        ];
        let result = serde_json::to_value(
            normalize_catalog(
                &store,
                CachedCatalog {
                    providers: vec![provider],
                },
            )
            .await,
        )
        .unwrap();
        let models = &result["providers"][0]["models"];
        assert_eq!(models[0]["model"]["context_window"], 100_000);
        assert_eq!(models[0]["model"]["max_output_tokens"], 64000);
        assert_eq!(models[0]["reasoning"], false);
        assert_eq!(models[0]["tool_call"], true);
        assert_eq!(
            models[0]["input_modalities"],
            json!(["text", "image", "pdf"])
        );
        assert_eq!(models[0]["pricing"]["input"], 1.0);
        assert_eq!(models[0]["pricing"]["output"], 5.0);
        assert_eq!(models[0]["pricing"]["source"], "claude-reference");
        assert_eq!(models[1]["pricing"], Value::Null);
        assert_eq!(models[1]["model"].get("context_window"), None);
    }

    #[test]
    fn provider_lookup_precedence_and_owner_restrictions() {
        let provider = sample_provider("openai", "custom");
        let sheet = json!({
            "custom/m": {"provider":"custom", "value":1},
            "openai/m": {"provider":"openai", "value":2},
            "m": {"provider":"custom", "value":3},
            "other/m": {"provider":"custom", "value":4}
        });
        assert_eq!(lookup(&sheet, &provider, "m").unwrap()["value"], 1);
        assert!(lookup(&sheet, &provider, "other/m").is_none());
        let sheet = json!({"custom/m":{"provider":"wrong"}, "openai/m":{"provider":"openai", "value":2}, "m":{"provider":"custom", "value":3}});
        assert_eq!(lookup(&sheet, &provider, "m").unwrap()["value"], 2);
    }

    #[test]
    fn reference_aliases_require_matching_owner_and_base() {
        let sheet = json!({
            "gemini/gemini-x-preview":{"provider":"gemini","base_model":"other"},
            "vertex_ai/gemini-x-preview":{"provider":"vertex_ai","base_model":"gemini-x","value":2},
            "claude-x":{"provider":"anthropic","value":3},
            "anthropic/claude-x":{"provider":"wrong","value":4}
        });
        assert_eq!(reference_price(&sheet, "gemini-x").unwrap()["value"], 2);
        assert_eq!(
            reference_price(&sheet, "claude-x-thinking").unwrap()["value"],
            3
        );
        assert!(reference_price(&sheet, "gemini-y").is_none());
        assert!(
            reference_price(
                &json!({"gemini/gemini-x-preview":{"provider":"wrong","base_model":"gemini-x"}}),
                "gemini-x"
            )
            .is_none()
        );
    }

    #[test]
    fn pricing_selection_units_and_unknown_values() {
        let model: Model = serde_json::from_value(json!({"id":"gemini-x"})).unwrap();
        let agy = sample_provider("agy", "custom");
        let data = sample_data(
            json!({
                "custom/gemini-x":{"provider":"custom","input_cost_per_token":0.000_002,"output_cost_per_token":"invalid"},
                "gemini/gemini-x":{"provider":"gemini","input_cost_per_token":0.000_009}
            }),
            json!({}),
        );
        let result = normalize_model(&agy, model, &data);
        let price = result.pricing.unwrap();
        assert_eq!(price.input, Some(2.0));
        assert_eq!(price.output, None);
        assert_eq!(price.cache_read, None);
        assert_eq!(price.source, "antigravity-reference");
        let zen = sample_provider("zen", "zen");
        let model: Model = serde_json::from_value(json!({
            "id":"m", "reasoning":true, "zen_metadata":{
                "reasoning":false,"tool_call":true,"modalities":{"input":["text", 42, "image"]},
                "cost":{"input":2.5,"output":null,"cache_read":0.1}
            }
        }))
        .unwrap();
        let result = normalize_model(
            &zen,
            model,
            &sample_data(
                json!({"zen/m":{"provider":"zen","input_cost_per_token":1}}),
                json!({}),
            ),
        );
        assert_eq!(result.reasoning, Some(false));
        assert_eq!(result.tool_call, Some(true));
        assert_eq!(result.input_modalities, ["text", "image"]);
        let price = result.pricing.unwrap();
        assert_eq!(price.input, Some(2.5));
        assert_eq!(price.output, None);
        assert_eq!(price.cache_read, Some(0.1));
        assert_eq!(price.source, "models.dev");
        let model: Model =
            serde_json::from_value(json!({"id":"m","zen_metadata":{"cost":[]}})).unwrap();
        assert!(
            normalize_model(&zen, model, &sample_data(json!({}), json!({})))
                .pricing
                .is_none()
        );
    }

    /// Invalid metadata fields do not hide other capabilities or raw details.
    #[test]
    fn catalog_preserves_valid_fields_when_zen_metadata_is_malformed() {
        let mut provider = sample_provider("zen", "zen");
        provider.models.push(
            serde_json::from_value(json!({
                "id":"m",
                "zen_metadata":{
                    "reasoning":"invalid",
                    "tool_call":true,
                    "modalities":{"input":["audio", 42, "pdf"]},
                    "cost":{"input":"invalid", "output":3.0},
                    "extra":"retained"
                }
            }))
            .unwrap(),
        );
        let result = serde_json::to_value(enrich_catalog(
            CachedCatalog {
                providers: vec![provider],
            },
            &sample_data(json!({}), json!({})),
        ))
        .unwrap();
        let model = &result["providers"][0]["models"][0];
        assert_eq!(model["reasoning"], Value::Null);
        assert_eq!(model["tool_call"], true);
        assert_eq!(model["input_modalities"], json!(["audio", "pdf"]));
        assert_eq!(model["pricing"]["input"], Value::Null);
        assert_eq!(model["pricing"]["output"], 3.0);
        assert_eq!(model["provider_metadata"]["extra"], "retained");
        assert_eq!(model["raw_pricing"]["input"], "invalid");
    }

    #[tokio::test]
    async fn normalized_response_uses_current_sheets_without_changing_raw_cache() {
        let store = store().await;
        let mut provider = sample_provider("openai", "custom");
        provider.models.push(serde_json::from_value(json!({"id":"m","name":" ", "display_name":"Model", "description":"details", "custom":42})).unwrap());
        let catalog = CachedCatalog {
            providers: vec![provider],
        };
        save(&store, &catalog).await.unwrap();
        let response = serde_json::to_value(normalize_catalog(&store, catalog).await).unwrap();
        let model = &response["providers"][0]["models"][0];
        assert_eq!(model["model"]["custom"], 42);
        assert_eq!(model["display_name"], "Model");
        assert_eq!(model["description"], "details");
        assert_eq!(model["pricing"], Value::Null);
        assert_eq!(model["parameters"], Value::Null);
        let saved: Value =
            serde_json::from_str(&store.setting_get(CACHE_KEY).await.unwrap().unwrap()).unwrap();
        assert_eq!(saved["providers"][0]["models"][0]["id"], "m");
        assert!(saved["providers"][0]["models"][0].get("model").is_none());
        Setting::upsert_by_name("model_pricing_datasheet")
            .value(r#"{"custom/m":{"provider":"custom","input_cost_per_token":0.000003}}"#)
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        let cached: CachedCatalog = serde_json::from_value(saved).unwrap();
        let response = serde_json::to_value(normalize_catalog(&store, cached).await).unwrap();
        assert_eq!(
            response["providers"][0]["models"][0]["pricing"]["input"],
            3.0
        );
        assert_eq!(
            response["providers"][0]["models"][0]["pricing"]["source"],
            "datasheet"
        );
    }

    /// A damaged datasheet is reported without hiding discovered models.
    #[tokio::test]
    async fn catalog_keeps_models_when_datasheet_is_invalid() {
        let store = store().await;
        Setting::upsert_by_name("model_pricing_datasheet")
            .value("{invalid")
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        let mut provider = sample_provider("openai", "custom");
        provider
            .models
            .push(serde_json::from_value(json!({"id":"m"})).unwrap());
        let response = normalize_catalog(
            &store,
            CachedCatalog {
                providers: vec![provider],
            },
        )
        .await;
        assert_eq!(
            response.datasheets_error.as_deref(),
            Some("Unable to load cached datasheets")
        );
        assert_eq!(response.providers[0].models[0].model.id, "m");
        assert!(response.providers[0].models[0].pricing.is_none());
    }

    /// Antigravity's cached capability flags and enabled MIME types are shown in the catalog.
    #[tokio::test]
    async fn catalog_reports_antigravity_input_modalities() {
        let store = store().await;
        assert_eq!(
            registry_request(&store, "POST", "/api/providers", json!({"kind":"agy"})).await,
            StatusCode::CREATED
        );
        let mut catalog = load(&store).await.unwrap();
        catalog.providers[0].models = [
            json!({"id":"vision","supportsImages":true,"supportedMimeTypes":{"image/png":true,"video/jpeg2000":true,"video/videoframe/jpeg2000":true,"audio/wav":false,"application/pdf":false}}),
            json!({"id":"multimodal","supportsImages":true,"supportsVideo":true,"supportedMimeTypes":{"audio/mpeg":true,"application/pdf":true,"video/mp4":true}}),
            json!({"id":"disabled","supportsImages":false,"supportsVideo":false,"supportedMimeTypes":{"image/png":true,"video/mp4":true,"audio/mpeg":false,"application/pdf":false}}),
            json!({"id":"mime-only","supportedMimeTypes":{"image/webp":true,"video/webm":true}}),
            json!({"id":"plain"}),
        ].into_iter().map(|model| serde_json::from_value(model).unwrap()).collect();
        save(&store, &catalog).await.unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            logs: crate::server::observability::LogStore::new(store.database.clone()),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
            settings: store,
        };
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/model-catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        let models = document["providers"][0]["models"].as_array().unwrap();
        let inputs = |id: &str| {
            &models
                .iter()
                .find(|model| model["model"]["id"] == id)
                .unwrap()["input_modalities"]
        };
        assert_eq!(inputs("vision"), &json!(["text", "image"]));
        assert_eq!(
            inputs("multimodal"),
            &json!(["text", "image", "audio", "video", "pdf"])
        );
        assert_eq!(inputs("disabled"), &json!(["text"]));
        assert_eq!(inputs("mime-only"), &json!(["text", "image", "video"]));
        assert_eq!(inputs("plain"), &json!(["text"]));
    }

    /// Create an in-memory settings store.
    async fn store() -> SettingsStore {
        let database = Database::open(":memory:").await.unwrap();
        SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap()
    }

    /// Send a JSON request to the provider registry.
    async fn registry_request(
        store: &SettingsStore,
        method: &str,
        path: &str,
        body: Value,
    ) -> StatusCode {
        registry::router(store.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Cached models survive reopening and follow registry changes.
    #[tokio::test]
    async fn cache_survives_store_recreation_and_tracks_registry() {
        let store = store().await;
        let provider = |id| json!({"id":id,"name":id,"kind":"openai","base_url":format!("http://{id}.localhost/v1")});
        assert_eq!(
            registry_request(&store, "POST", "/api/providers", provider("first")).await,
            StatusCode::CREATED
        );
        let mut catalog = load(&store).await.unwrap();
        catalog.providers[0].models =
            vec![serde_json::from_value(json!({"id":"cached", "metadata":42})).unwrap()];
        catalog.providers[0].error = Some("offline".into());
        save(&store, &catalog).await.unwrap();
        let reopened = SettingsStore::new(
            store.database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let cached = load(&reopened).await.unwrap();
        assert_eq!(cached.providers[0].models[0].extra["metadata"], 42);
        assert_eq!(cached.providers[0].error.as_deref(), Some("offline"));
        assert_eq!(
            registry_request(&store, "POST", "/api/providers", provider("second")).await,
            StatusCode::CREATED
        );
        assert_eq!(
            registry_request(&store, "DELETE", "/api/providers/first", Value::Null).await,
            StatusCode::NO_CONTENT
        );
        let updated = load(&reopened).await.unwrap();
        assert_eq!(updated.providers.len(), 1);
        assert_eq!(updated.providers[0].id, "second");
        assert!(updated.providers[0].models.is_empty());
    }
}
