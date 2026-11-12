//! Model metadata conversion and permission-aware model resolution.

use super::pool::{PoolError, pool_error_response, pool_models};
use crate::openai::Model;
use crate::provider::antigravity::{
    AntigravityModelInfo,
    models::{ResolveError, normalize_models},
};
use crate::provider::registry::Provider;
use crate::server::{AppState, settings::Permissions};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};

/// Returns the response for a model disallowed by the virtual key.
fn model_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(crate::server::error_body(
            "Model is not allowed for this virtual key",
            "permission_error",
            Some("model_not_allowed"),
        )),
    )
        .into_response()
}

/// Upstream reports the context window as `maxTokens`, separate from
/// `maxOutputTokens`; callers that only see `max_output_tokens` fall back to
/// their own defaults, so both are carried through.
pub(crate) fn to_model_entry(id: String, info: AntigravityModelInfo) -> Model {
    let reasoning = info.supports_thinking;
    // Standalone IDs ignore an effort parameter, so no selectable levels or
    // default effort can be advertised for them.
    let effort_tier = None;
    let supported_efforts = None;

    let mut extra: serde_json::Map<String, serde_json::Value> = info.extra.into_iter().collect();
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
        "input_modalities",
        "output_modalities",
        "supports_tools",
    ] {
        extra.remove(key);
    }
    extra.insert("displayName".into(), serde_json::json!(info.display_name));
    extra.insert(
        "supportsThinking".into(),
        serde_json::json!(info.supports_thinking),
    );
    extra.insert("maxTokens".into(), serde_json::json!(info.max_tokens));
    extra.insert(
        "maxOutputTokens".into(),
        serde_json::json!(info.max_output_tokens),
    );
    extra.insert("disabled".into(), serde_json::json!(info.disabled));
    Model {
        id,
        object: Some("model".to_string()),
        created: Some(1_700_000_000),
        owned_by: Some("google-antigravity".to_string()),
        name: info.display_name.clone(),
        display_name: info.display_name,
        reasoning,
        default_reasoning_effort: effort_tier,
        supported_reasoning_efforts: supported_efforts,
        context_window: info.max_tokens,
        max_output_tokens: info.max_output_tokens,
        input_modalities: Some(crate::provider::antigravity::models::input_modalities(
            &extra,
        )),
        // Cloud Code Assist is translated to OpenAI chat/responses text output.
        output_modalities: Some(vec!["text".to_owned()]),
        supports_tools: None,
        extra,
    }
}

/// Resolves a requested model and reasoning effort against permitted models.
pub(super) fn resolve_model(
    models: Vec<(String, AntigravityModelInfo)>,
    model: &str,
    effort: Option<&str>,
    permissions: &Permissions,
) -> Result<String, ResolveError> {
    for family in normalize_models(models) {
        if family.id == model {
            return family.resolve(effort, permissions);
        }
        if family.grouped && family.variants.iter().any(|(id, _)| id == model) {
            return if permissions.allows_model(model) || permissions.allows_model(&family.id) {
                Ok(model.to_owned())
            } else {
                Err(ResolveError::Forbidden)
            };
        }
    }
    // Preserve direct access to upstream models not present in discovery.
    if permissions.allows_model(model) {
        Ok(model.to_owned())
    } else {
        Err(ResolveError::Forbidden)
    }
}

/// Failure fetching or resolving a requested model.
pub(super) enum RequestModelError {
    /// Model discovery failed.
    Pool(PoolError),
    /// Model resolution or authorization failed.
    Resolve(ResolveError),
}

impl RequestModelError {
    /// Converts the failure into an API error response.
    pub(super) fn into_response(self) -> Response {
        match self {
            Self::Pool(error) => pool_error_response(error),
            Self::Resolve(ResolveError::Forbidden) => model_forbidden(),
            Self::Resolve(ResolveError::Unsupported) => (
                StatusCode::BAD_REQUEST,
                Json(crate::server::error_body(
                    "Reasoning effort is not supported by this model",
                    "invalid_request_error",
                    Some("unsupported_reasoning_effort"),
                )),
            )
                .into_response(),
        }
    }
}

/// Resolves a request model, fetching discovery data when needed.
pub(super) async fn resolve_request_model(
    state: &AppState,
    provider: &Provider,
    permissions: &Permissions,
    model: &str,
    effort: Option<&str>,
) -> Result<String, RequestModelError> {
    if ["-low", "-medium", "-high"]
        .iter()
        .any(|suffix| model.ends_with(suffix))
        && permissions.allows_model(model)
    {
        return Ok(model.to_owned());
    }
    let models = pool_models(state, provider, None)
        .await
        .map_err(RequestModelError::Pool)?;
    resolve_model(models, model, effort, permissions).map_err(RequestModelError::Resolve)
}
