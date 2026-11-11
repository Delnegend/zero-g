use super::{InferenceDiscoveryError, InferenceModelList, error, permissions};
use crate::provider::{catalog, registry};
use crate::server::AppState;
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::future::join_all;
use serde_json::json;

/// Apply catalog facts already present on this permitted discovery record.
fn normalize_model(kind: &str, model: &mut crate::openai::Model) {
    let info = match kind {
        "zen" | "go" => crate::provider::zen::catalog::catalog_info(model, kind),
        "claude" => crate::provider::claude::catalog_info(model),
        "openrouter" => crate::provider::openrouter::catalog_info(model),
        "chatjimmy" => crate::provider::chatjimmy::catalog_info(model),
        _ => return,
    };
    // Anthropic's max_input_tokens is input-only, not a combined context limit.
    if kind != "claude" {
        model.context_window = model
            .context_window
            .or(info.context_window)
            .filter(|n| *n > 0);
    }
    model.max_output_tokens = model
        .max_output_tokens
        .or(info.max_output_tokens)
        .filter(|n| *n > 0);
    model.reasoning = model.reasoning.or(info.reasoning);
    model.supports_tools = model.supports_tools.or(info.tool_call);
    if model.input_modalities.is_none() {
        model.input_modalities = info.input_modalities.map(|modalities| {
            modalities
                .into_iter()
                .filter(|m| {
                    if kind == "claude" {
                        m == "text" || m == "image"
                    } else {
                        matches!(m.as_str(), "text" | "image" | "audio" | "video" | "pdf")
                    }
                })
                .collect()
        });
    }
    if kind == "claude" {
        // The translator accepts text and images, but not PDF content parts.
        if let Some(modalities) = &mut model.input_modalities {
            modalities.retain(|m| m == "text" || m == "image");
            if !modalities.iter().any(|m| m == "text") {
                modalities.insert(0, "text".into());
            }
        }
        model.supported_reasoning_efforts = info.reasoning_efforts;
        model.output_modalities = Some(vec!["text".into()]);
    }
    // A flattened provider field must never shadow a normalized field.
    for key in [
        "context_window",
        "max_output_tokens",
        "reasoning",
        "input_modalities",
        "output_modalities",
        "supports_tools",
        "supported_reasoning_efforts",
        "default_reasoning_effort",
    ] {
        model.extra.remove(key);
    }
}

/// Aggregate permitted model records, without exposing upstream errors.
#[utoipa::path(get, path = "/v1/models", operation_id = "listInferenceModels",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    responses((status = 200, body = InferenceModelList),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 500, description = "Registry or permissions error", body = Value),
        (status = 502, description = "All provider discoveries failed", body = Value)))]
pub(super) async fn models(State(state): State<AppState>, request: Request) -> Response {
    let permissions = match permissions(&state, request.headers(), request.extensions().get()).await
    {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Ok(providers) = registry::list(&state.settings.database).await else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load providers",
            "server_error",
            "internal_error",
        );
    };
    let providers: Vec<_> = providers
        .into_iter()
        .filter(|p| permissions.providers.is_empty() || permissions.providers.contains(&p.id))
        .collect();
    let results = join_all(providers.iter().map(|provider| async {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            catalog::discover_permitted(&state, provider, &permissions),
        )
        .await
    }))
    .await;
    let mut data = Vec::new();
    let mut errors = Vec::new();
    let mut succeeded = false;
    for (provider, result) in providers.iter().zip(results) {
        match result {
            Ok(Ok(models)) => {
                succeeded = true;
                for mut model in models {
                    normalize_model(&provider.kind, &mut model);
                    model.id = format!("{}/{}", provider.id, model.id);
                    data.push(model);
                }
            }
            _ => errors.push(InferenceDiscoveryError {
                provider: provider.id.clone(),
                message: "Model discovery failed".into(),
            }),
        }
    }
    if !succeeded && !errors.is_empty() {
        let mut body = super::super::error_body(
            "Model discovery failed",
            "upstream_error",
            Some("discovery_failed"),
        );
        if let Some(object) = body.as_object_mut() {
            object.insert("errors".into(), json!(errors));
        }
        return (StatusCode::BAD_GATEWAY, Json(body)).into_response();
    }
    Json(InferenceModelList {
        object: "list".into(),
        data,
        errors,
    })
    .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::normalize_model;
    use crate::openai::Model;
    use serde_json::json;

    #[test]
    fn public_metadata_preserves_explicit_values_and_unknowns() {
        let mut known: Model = serde_json::from_value(json!({
            "id": "fixture", "context_length": 64000,
            "top_provider": {"max_completion_tokens": 4096},
            "architecture": {"input_modalities": ["text", "image"]},
            "supported_parameters": ["tools"], "custom": {"nested": true}
        }))
        .unwrap();
        normalize_model("openrouter", &mut known);
        let value = serde_json::to_value(known).unwrap();
        assert_eq!(value["context_window"], 64000);
        assert_eq!(value["max_output_tokens"], 4096);
        assert_eq!(value["input_modalities"], json!(["text", "image"]));
        assert_eq!(value["supports_tools"], true);
        assert_eq!(value["custom"]["nested"], true);
        assert!(value.get("output_modalities").is_none());

        let mut unsupported: Model = serde_json::from_value(json!({
            "id": "fixture", "supported_parameters": []
        }))
        .unwrap();
        normalize_model("openrouter", &mut unsupported);
        let value = serde_json::to_value(unsupported).unwrap();
        assert_eq!(value["supports_tools"], false);
        assert!(value.get("input_modalities").is_none());
        assert!(value.get("context_window").is_none());
    }

    #[test]
    fn claude_reports_only_gateway_supported_modalities_and_no_input_only_context() {
        let mut model: Model = serde_json::from_value(json!({
            "id": "fixture", "max_input_tokens": 100000, "max_tokens": 0,
            "capabilities": {"image_input": {"supported": true}, "pdf_input": {"supported": true}}
        }))
        .unwrap();
        normalize_model("claude", &mut model);
        let value = serde_json::to_value(model).unwrap();
        assert!(value.get("context_window").is_none());
        assert!(value.get("max_output_tokens").is_none());
        assert_eq!(value["input_modalities"], json!(["text", "image"]));
        assert_eq!(value["output_modalities"], json!(["text"]));
    }

    #[test]
    fn zen_catalog_populates_false_and_limits_without_new_discovery() {
        let mut model: Model = serde_json::from_value(json!({
            "id": "fixture", "zen_metadata": {"tool_call": false,
                "limit": {"context": 32000, "output": 2048},
                "modalities": {"input": ["text", "image"]}}
        }))
        .unwrap();
        normalize_model("zen", &mut model);
        assert_eq!(model.supports_tools, Some(false));
        assert_eq!(model.context_window, Some(32000));
        assert_eq!(model.max_output_tokens, Some(2048));
        assert_eq!(
            model.input_modalities,
            Some(vec!["text".into(), "image".into()])
        );
    }
}
