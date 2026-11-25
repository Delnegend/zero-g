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
        "openrouter" => crate::provider::openrouter::catalog_info(model),
        "chatjimmy" => crate::provider::chatjimmy::catalog_info(model),
        _ => return,
    };
    info.apply_to_route(model);
}

/// List Claude models for an Anthropic client, in Anthropic's list shape with plain ids.
async fn anthropic_models(state: &AppState, request: Request) -> Response {
    use crate::provider::claude::passthrough::error as anthropic_error;
    let permissions = match permissions(state, request.headers(), request.extensions().get()).await
    {
        Ok(p) => p,
        Err(e) => return anthropic_error(e.status(), e.message()),
    };
    if !permissions.providers.is_empty() && !permissions.providers.iter().any(|p| p == "claude") {
        return anthropic_error(
            StatusCode::FORBIDDEN,
            "Virtual key does not permit this provider",
        );
    }
    let provider = match registry::get(&state.settings.database, "claude").await {
        Ok(Some(provider)) if provider.kind == "claude" => provider,
        Ok(_) => {
            return anthropic_error(StatusCode::NOT_FOUND, "Claude provider is not configured");
        }
        Err(_) => {
            return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load provider");
        }
    };
    catalog::discover_permitted(state, &provider, &permissions)
        .await
        .map_or_else(
            |_| anthropic_error(StatusCode::BAD_GATEWAY, "Model discovery failed"),
            |mut models| {
                models.retain(|model| permissions.allows_model(&model.id));
                let first = models.first().map(|model| model.id.clone());
                let last = models.last().map(|model| model.id.clone());
                Json(json!({"data": models, "has_more": false, "first_id": first, "last_id": last}))
                    .into_response()
            },
        )
}

/// Aggregate permitted model records, without exposing upstream errors.
#[utoipa::path(get, path = "/v1/models", operation_id = "listInferenceModels",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    responses((status = 200, body = InferenceModelList),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 500, description = "Registry or permissions error", body = Value),
        (status = 502, description = "All provider discoveries failed", body = Value)))]
pub(super) async fn models(State(state): State<AppState>, request: Request) -> Response {
    if request.headers().contains_key("anthropic-version") {
        return anthropic_models(&state, request).await;
    }
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
        // Claude is reachable only through `/v1/messages`, so it has no place in this list.
        .filter(|p| p.kind != "claude")
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
                let source = if provider.name.is_empty() {
                    &provider.id
                } else {
                    &provider.name
                };
                for mut model in models {
                    normalize_model(&provider.kind, &mut model);
                    let base = model
                        .display_name
                        .as_deref()
                        .or(model.name.as_deref())
                        .unwrap_or(&model.id);
                    model.display_name = Some(format!("{base} ({source})"));
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
