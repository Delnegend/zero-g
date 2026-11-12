use crate::openai::Model;
use crate::provider;
use crate::server::{AppState, settings::Permissions};
use axum::{
    Json, Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::Value;

/// Chat Completions endpoint.
mod chat_completions;
/// Decisions endpoint.
mod decisions;
/// Shared request validation and provider dispatch.
mod dispatch;
/// Model discovery endpoint.
mod models;
/// Responses endpoint.
mod responses;

use chat_completions::{__path_inference, inference};
use decisions::{__path_inference_decisions, inference_decisions};
use models::{__path_models, models};
use responses::{__path_inference_responses, inference_responses};

/// Maximum size in bytes of a unified inference JSON request.
pub(super) const INFERENCE_BODY_LIMIT: usize = 20_000_000;

/// Inference request with provider-qualified model and provider-specific fields.
#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
struct InferenceRequest {
    /// Model identifier in `provider/model` form.
    model: String,
    /// Remaining provider-specific request fields are passed through.
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

/// Discovery failure for an individual provider.
#[derive(serde::Serialize, utoipa::ToSchema)]
struct InferenceDiscoveryError {
    /// Provider identifier.
    provider: String,
    /// Discovery failure message.
    message: String,
}

/// OpenAI-compatible model list with per-provider discovery errors.
#[derive(serde::Serialize, utoipa::ToSchema)]
struct InferenceModelList {
    /// List object discriminator.
    object: String,
    /// Provider model records, with qualified IDs.
    data: Vec<Model>,
    /// Providers whose discovery failed.
    errors: Vec<InferenceDiscoveryError>,
}

/// Public unified inference operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(models, inference, inference_responses, inference_decisions),
    components(schemas(InferenceRequest, InferenceModelList, InferenceDiscoveryError, Model))
)]
pub(crate) struct Api;

/// Return a consistent OpenAI-compatible error.
fn error(status: StatusCode, message: &str, kind: &str, code: &str) -> Response {
    (status, Json(super::error_body(message, kind, Some(code)))).into_response()
}

/// Authenticate before consulting the registry or discovering any models.
async fn permissions(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
) -> Result<Permissions, crate::provider::common::proxy::ProxyError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim());
    let Some(token) = token else {
        return Err(crate::provider::common::proxy::error(
            StatusCode::UNAUTHORIZED,
            "A virtual key is required",
            "authentication_error",
            "invalid_api_key",
        ));
    };
    match state.settings.permissions(token, identity).await {
        Ok(Some(permissions)) => Ok(permissions),
        Ok(None) => Err(crate::provider::common::proxy::error(
            StatusCode::UNAUTHORIZED,
            "Invalid virtual key",
            "authentication_error",
            "invalid_api_key",
        )),
        Err(_) => Err(crate::provider::common::proxy::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load permissions",
            "server_error",
            "internal_error",
        )),
    }
}

/// Authenticate inference requests before reading or logging their bodies.
pub(super) async fn authenticate_post(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }
    let identity = crate::provider::common::request_identity::RequestIdentity::default();
    match permissions(&state, request.headers(), Some(&identity)).await {
        Ok(permissions) => {
            request.extensions_mut().insert(identity);
            request.extensions_mut().insert(permissions);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

/// The sole public inference API.
pub(super) fn router(state: &AppState) -> Router<AppState> {
    let internal: Router = provider::router(&state.settings)
        .layer(axum::extract::DefaultBodyLimit::max(INFERENCE_BODY_LIMIT))
        .with_state(state.clone());
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(inference))
        .route("/v1/responses", post(inference_responses))
        .route("/v1/decisions", post(inference_decisions))
        .layer(axum::Extension(internal))
}

/// Inference routing and authorization tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
