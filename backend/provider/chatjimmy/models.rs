use serde_json::Value;

use super::{CLIENT, DEFAULT_BASE, DEFAULT_MODEL, MODELS_PATH};
use crate::openai::Model;
use crate::provider::common::catalog::{CatalogInfo, CatalogPrices};

/// Provider-supplied model details and reference pricing for `ChatJimmy` models.
pub(crate) fn catalog_info(_model: &Model) -> CatalogInfo {
    CatalogInfo {
        context_window: Some(6064),
        max_output_tokens: Some(2048),
        reasoning: Some(false),
        tool_call: Some(false),
        input_modalities: Some(vec!["text".to_string()]),
        reasoning_efforts: None,
        pricing: Some(CatalogPrices {
            input: Some(0.0),
            output: Some(0.0),
            cache_write: None,
            cache_read: None,
            source: "chatjimmy-reference",
            raw: serde_json::json!({
                "input_cost_per_token": 0.0,
                "output_cost_per_token": 0.0,
                "info": "Free public demo on Taalas HC1 silicon"
            }),
        }),
    }
}

/// Discovers models available from the `ChatJimmy` endpoint.
pub(super) async fn discover_models() -> Result<Vec<Model>, String> {
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize ChatJimmy client")?;
    let resp = client
        .get(format!("{DEFAULT_BASE}{MODELS_PATH}"))
        .send()
        .await;
    let mut models = if let Ok(r) = resp
        && r.status().is_success()
        && let Ok(value) = r.json::<Value>().await
    {
        let list = crate::provider::openai::models_from_list(&value);
        if list.is_empty() {
            fallback_models()
        } else {
            list
        }
    } else {
        fallback_models()
    };
    for model in &mut models {
        if model.context_window.is_none() {
            model.context_window = Some(6064);
        }
        if model.max_output_tokens.is_none() {
            model.max_output_tokens = Some(2048);
        }
        if model.name.is_none() {
            model.name = Some("Llama 3.1 8B".to_string());
        }
        if model.display_name.is_none() {
            model.display_name = Some("Llama 3.1 8B (Taalas HC1)".to_string());
        }
        if model.reasoning.is_none() {
            model.reasoning = Some(false);
        }
        model.input_modalities = Some(vec!["text".to_owned()]);
        model.output_modalities = Some(vec!["text".to_owned()]);
        model.supports_tools = Some(false);
    }
    Ok(models)
}

/// Default fallback model when upstream discovery is unreachable.
fn fallback_models() -> Vec<Model> {
    vec![Model {
        id: DEFAULT_MODEL.to_string(),
        object: Some("model".to_string()),
        created: Some(1_700_000_000),
        owned_by: Some("Taalas Inc.".to_string()),
        name: Some("Llama 3.1 8B".to_string()),
        display_name: Some("Llama 3.1 8B (Taalas HC1)".to_string()),
        context_window: Some(6064),
        max_output_tokens: Some(2048),
        reasoning: Some(false),
        input_modalities: Some(vec!["text".to_owned()]),
        output_modalities: Some(vec!["text".to_owned()]),
        supports_tools: Some(false),
        ..Default::default()
    }]
}
