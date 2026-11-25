use super::{INFERENCE_BODY_LIMIT, InferenceRequest, error, permissions};
use crate::provider::registry;
use crate::server::AppState;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tower::ServiceExt;

/// Decode and validate the provider-qualified model without changing request fields.
fn parse_inference(
    body: &[u8],
) -> Result<(String, String, InferenceRequest), crate::provider::common::proxy::ProxyError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        crate::provider::common::proxy::error(
            StatusCode::BAD_REQUEST,
            "Invalid JSON request",
            "invalid_request_error",
            "invalid_request",
        )
    })?;
    let invalid_model = || {
        crate::provider::common::proxy::error(
            StatusCode::BAD_REQUEST,
            "Model must be provider/model",
            "invalid_request_error",
            "invalid_model",
        )
    };
    let (id, model) = value
        .get("model")
        .and_then(Value::as_str)
        .and_then(|m| m.split_once('/'))
        .filter(|(id, model)| !id.is_empty() && !model.is_empty())
        .ok_or_else(invalid_model)?;
    let id = id.to_owned();
    let model = model.to_owned();
    let value = serde_json::from_value(value).map_err(|_| invalid_model())?;
    Ok((id, model, value))
}

/// Why a provider kind cannot serve OpenAI-format requests, if it cannot.
fn unsupported_reason(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("Claude is available only through the Anthropic /v1/messages endpoint"),
        "agy" => Some("Google Antigravity only reports quota and serves no model requests"),
        _ => None,
    }
}

/// The refusal for a provider kind that cannot serve OpenAI-format requests.
fn refuse_unsupported(kind: &str) -> Option<Response> {
    unsupported_reason(kind).map(|reason| {
        error(
            StatusCode::BAD_REQUEST,
            reason,
            "invalid_request_error",
            "unsupported_provider",
        )
    })
}

/// Authenticate and dispatch an inference request to its selected provider.
pub(super) async fn dispatch(
    State(state): State<AppState>,
    axum::Extension(router): axum::Extension<Router>,
    request: Request,
) -> Response {
    let permissions = if let Some(permissions) = request
        .extensions()
        .get::<crate::server::settings::Permissions>()
    {
        permissions.clone()
    } else {
        match permissions(&state, request.headers(), request.extensions().get()).await {
            Ok(p) => p,
            Err(e) => return e.into_response(),
        }
    };
    let (mut parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, INFERENCE_BODY_LIMIT).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body too large",
            "invalid_request_error",
            "body_too_large",
        );
    };
    let (id, model, mut value) = match parse_inference(&body) {
        Ok(value) => value,
        Err(err) => return err.into_response(),
    };
    if !permissions.providers.is_empty() && !permissions.providers.contains(&id) {
        return error(
            StatusCode::FORBIDDEN,
            "Virtual key does not permit this provider",
            "permission_error",
            "provider_not_allowed",
        );
    }
    let provider = match registry::get(&state.settings.database, &id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "Provider not found",
                "invalid_request_error",
                "provider_not_found",
            );
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load provider",
                "server_error",
                "internal_error",
            );
        }
    };
    if let Some(response) = refuse_unsupported(&provider.kind) {
        return response;
    }
    if parts.uri.path() == "/v1/decisions" && !permissions.allows_model(&model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        );
    }
    value.model = model;
    let path = parts
        .uri
        .path_and_query()
        .map_or("/v1/chat/completions", |p| p.as_str());
    let uri = format!("/{}/{id}{path}", provider.kind);
    let Ok(uri) = uri.parse() else {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid request URL",
            "invalid_request_error",
            "invalid_request",
        );
    };
    parts.uri = uri;
    parts.headers.remove(header::CONTENT_LENGTH);
    // The provider router reuses this provider and the authenticated permissions.
    parts.extensions.insert(provider);
    let Ok(body) = serde_json::to_vec(&value) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to serialize request",
            "server_error",
            "internal_error",
        );
    };
    match router
        .oneshot(Request::from_parts(parts, Body::from(body)))
        .await
    {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::unsupported_reason;

    #[test]
    fn only_claude_and_antigravity_refuse_openai_format_requests() {
        assert!(unsupported_reason("claude").is_some_and(|text| text.contains("/v1/messages")));
        assert!(unsupported_reason("agy").is_some());
        for kind in [
            "codex",
            "cursor",
            "zen",
            "go",
            "openai",
            "openrouter",
            "chatjimmy",
        ] {
            assert!(unsupported_reason(kind).is_none(), "{kind}");
        }
    }
}
