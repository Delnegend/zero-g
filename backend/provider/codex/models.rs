use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::openai::{Model, ModelListResponse};
use crate::provider::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexCredentials};
use crate::provider::common::errors::UpstreamError;
use crate::provider::registry::Provider;
use crate::server::AppState;

/// Upstream model list, which may use either supported envelope field.
#[derive(Deserialize)]
struct CodexModelsResponse {
    /// Model entries in the primary envelope.
    #[serde(default)]
    models: Option<Vec<CodexModelEntry>>,
    /// Model entries in the alternate envelope.
    #[serde(default)]
    data: Option<Vec<CodexModelEntry>>,
}

/// Model metadata returned by the Codex endpoint.
#[derive(Serialize, Deserialize)]
struct CodexModelEntry {
    /// Preferred model identifier.
    slug: Option<String>,
    /// Alternate model identifier when no slug is provided.
    id: Option<String>,
    /// Upstream visibility flag used to exclude hidden models.
    visibility: Option<String>,
    /// Human-readable model name.
    display_name: Option<String>,
    /// Default reasoning effort, if supplied.
    default_reasoning_level: Option<String>,
    /// Available reasoning effort metadata.
    supported_reasoning_levels: Option<Vec<CodexReasoningLevel>>,
    /// Model context window size.
    context_window: Option<u64>,
    /// Additional upstream metadata retained in the catalog.
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

/// One upstream reasoning effort and its additional metadata.
#[derive(Serialize, Deserialize)]
struct CodexReasoningLevel {
    /// Reasoning effort identifier.
    effort: Option<String>,
    /// Additional upstream metadata for this effort.
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

/// Extract modalities explicitly advertised by Codex.
pub(crate) fn catalog_info(model: &Model) -> crate::provider::common::catalog::CatalogInfo {
    crate::provider::common::catalog::CatalogInfo {
        input_modalities: model
            .input_modalities
            .clone()
            .or_else(|| modalities(model.extra.get("input_modalities"))),
        ..Default::default()
    }
}

/// Normalize capabilities implemented by the translated Codex route.
pub(crate) fn public_capabilities(model: &mut Model) {
    model.input_modalities = catalog_info(model).input_modalities.map(|modalities| {
        modalities
            .into_iter()
            .filter(|m| m == "text" || m == "image")
            .collect()
    });
    model.output_modalities = Some(vec!["text".to_owned()]);
    // Translation supports function tools, but model support must be explicit.
    model.supports_tools = model
        .supports_tools
        .or_else(|| model.extra.get("supports_tools").and_then(Value::as_bool));
    model.extra.remove("supports_tools");
}

/// Extract modalities from the matching `OpenAI` API reference record.
pub(crate) fn catalog_reference(value: &Value) -> crate::provider::common::catalog::CatalogInfo {
    crate::provider::common::catalog::CatalogInfo {
        input_modalities: modalities(value.get("supported_modalities")),
        ..Default::default()
    }
}

/// Accept only a complete array of modality names.
fn modalities(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Fetches the normalized model list using the supplied credentials.
pub(crate) async fn fetch_codex_models(
    creds: &CodexCredentials,
) -> Result<ModelListResponse, UpstreamError> {
    fetch_models(creds).await
}

/// Fetches normalized models and catalog metadata through the provider credential pool.
pub(crate) async fn fetch_catalog_pool(
    state: &AppState,
    provider: &Provider,
) -> Result<ModelListResponse, String> {
    crate::provider::codex::routes::with_pool(state, provider, None, |creds| async move {
        fetch_models(&creds).await
    })
    .await
    .map_err(|e| e.message)
}

/// Fetches and normalizes an upstream model list, retaining provider metadata.
async fn request_models(creds: &CodexCredentials) -> Result<CodexModelsResponse, UpstreamError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Failed to initialize Codex client")?;

    let url = format!("{CODEX_BASE_URL}/codex/models?client_version={CODEX_CLIENT_VERSION}");
    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "omp")
        .header("version", CODEX_CLIENT_VERSION)
        .header("accept", "application/json");

    if let Some(account_id) = &creds.account_id {
        req = req.header("chatgpt-account-id", account_id);
    }
    if let Some(residency) = &creds.residency {
        req = req.header("x-openai-internal-codex-residency", residency);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to request Codex models: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(UpstreamError {
            status: Some(status),
            message: format!("Codex models returned {status}: {body}"),
        });
    }

    resp.json()
        .await
        .map_err(|e| format!("Failed to parse Codex models response: {e}").into())
}

/// Normalize the upstream model list while retaining provider metadata.
async fn fetch_models(creds: &CodexCredentials) -> Result<ModelListResponse, UpstreamError> {
    let parsed = request_models(creds).await?;

    let entries = parsed
        .models
        .or(parsed.data)
        .ok_or("Invalid Codex model list")?;
    let mut models = Vec::new();

    for entry in entries {
        let Value::Object(mut extra) =
            serde_json::to_value(&entry).map_err(|_| "Failed to serialize Codex model")?
        else {
            return Err("Failed to serialize Codex model".into());
        };
        for key in [
            "id",
            "object",
            "created",
            "owned_by",
            "name",
            "display_name",
            "reasoning",
            "default_reasoning_effort",
            "supported_reasoning_efforts",
            "context_window",
            "max_output_tokens",
            "output_modalities",
        ] {
            extra.remove(key);
        }
        let slug = entry.slug.or(entry.id);
        let Some(model_id) = slug.filter(|id| !id.is_empty()) else {
            continue;
        };

        if let Some(vis) = &entry.visibility
            && (vis.eq_ignore_ascii_case("hide") || vis.eq_ignore_ascii_case("hidden"))
        {
            continue;
        }
        let supported_efforts: Option<Vec<String>> = entry
            .supported_reasoning_levels
            .map(|levels| {
                levels
                    .into_iter()
                    .filter_map(|l| l.effort)
                    .filter(|e| !e.trim().is_empty())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty());

        let has_reasoning = supported_efforts.is_some()
            || entry
                .default_reasoning_level
                .as_deref()
                .is_some_and(|l| l != "none");

        let default_effort = entry.default_reasoning_level.filter(|l| l != "none");

        let model = Model {
            id: model_id,
            object: Some("model".to_string()),
            created: Some(1_700_000_000),
            owned_by: Some("openai-codex".to_string()),
            name: entry.display_name.clone(),
            display_name: entry.display_name,
            reasoning: if has_reasoning { Some(true) } else { None },
            default_reasoning_effort: default_effort,
            supported_reasoning_efforts: supported_efforts,
            context_window: entry.context_window,
            max_output_tokens: None,
            input_modalities: None,
            output_modalities: None,
            supports_tools: None,
            extra,
        };
        let mut model = model;
        // The upstream does not publish an output cap and this translator does
        // not pass client output-limit parameters upstream. Keep it absent.
        public_capabilities(&mut model);
        model.extra.remove("input_modalities");
        models.push(model);
    }
    Ok(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

#[cfg(test)]
mod public_capability_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_capabilities_keep_unknown_input_and_output_limit() {
        let mut unknown = Model {
            id: "fixture".into(),
            ..Default::default()
        };
        public_capabilities(&mut unknown);
        let value = serde_json::to_value(&unknown).unwrap();
        assert!(value.get("input_modalities").is_none());
        assert!(value.get("max_output_tokens").is_none());
        assert_eq!(value.get("supports_tools"), None);
        let mut known: Model = serde_json::from_value(
            json!({"id":"fixture", "input_modalities":["text","image","video"]}),
        )
        .unwrap();
        public_capabilities(&mut known);
        assert_eq!(
            known.input_modalities,
            Some(vec!["text".into(), "image".into()])
        );
        for supported in [true, false] {
            let mut model = Model {
                id: "fixture".into(),
                extra: serde_json::Map::from_iter([("supports_tools".into(), json!(supported))]),
                ..Default::default()
            };
            public_capabilities(&mut model);
            assert_eq!(model.supports_tools, Some(supported));
        }
    }
}
