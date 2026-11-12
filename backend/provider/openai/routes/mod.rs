use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method};
#[cfg(test)]
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
use crate::provider::common::proxy::filter_models;
use crate::provider::common::proxy::{ProxyError, check_model, model_response, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::openai;
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};

/// Build authenticated routes for an OpenAI-compatible provider.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    router_for(settings, "openai", None)
}

/// Selects the upstream endpoint and transport for compatible routes.
#[derive(Clone)]
pub(crate) enum Upstream {
    /// OpenAI-compatible endpoint, optionally overriding the provider base URL.
    OpenAi(Option<std::sync::Arc<str>>),
    /// `OpenCode` Go endpoints for model discovery and inference.
    Go,
}

/// Build routes for an OpenAI-compatible provider kind.
pub(crate) fn router_for(
    settings: &SettingsStore,
    kind: &'static str,
    fixed_base: Option<&str>,
) -> Router<AppState> {
    router_with(
        settings,
        kind,
        Upstream::OpenAi(fixed_base.map(std::sync::Arc::from)),
    )
}

/// Build routes using `OpenCode` Go's distinct discovery and inference endpoints.
pub(crate) fn go_router(settings: &SettingsStore) -> Router<AppState> {
    router_with(settings, "go", Upstream::Go)
}

/// Builds compatible routes with the selected upstream transport.
fn router_with(
    settings: &SettingsStore,
    kind: &'static str,
    upstream: Upstream,
) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(Extension(upstream))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), kind),
            settings::virtual_key_auth,
        ))
}

/// Resolve credentials and send a request upstream.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    upstream: &Upstream,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, ProxyError> {
    retry_api_keys(
        settings,
        provider,
        match upstream {
            Upstream::OpenAi(_) => "No connected OpenAI-compatible credentials",
            Upstream::Go => "No connected OpenCode Go credentials",
        },
        identity,
        |key| {
            let method = method.clone();
            let body = body.clone();
            async move {
                match upstream {
                    Upstream::OpenAi(fixed_base) => {
                        openai::send(
                            fixed_base.as_deref().unwrap_or(&provider.base_url),
                            &key,
                            path,
                            query,
                            method,
                            headers,
                            body,
                        )
                        .await
                    }
                    Upstream::Go => {
                        crate::provider::go::send_upstream(
                            crate::provider::go::base(provider, path == "models"),
                            &key,
                            path,
                            query,
                            method,
                            headers,
                            body,
                        )
                        .await
                    }
                }
            }
        },
    )
    .await
}

/// Fetch and filter models allowed by the virtual key.
pub(crate) async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(upstream): Extension<Upstream>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let response = match send(
        &state.settings,
        &provider,
        &upstream,
        "models",
        query.as_deref(),
        Method::GET,
        &headers,
        None,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(response) => response,
        Err(response) => return response.into_response(),
    };
    model_response(response, &permissions).await
}

/// Forward an inference request after checking model permissions.
#[allow(clippy::too_many_arguments)]
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    upstream: &Upstream,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
    identity: Option<&RequestIdentity>,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    match send(
        &state.settings,
        &provider,
        upstream,
        path,
        query.as_deref(),
        Method::POST,
        &headers,
        Some(body),
        identity,
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(response) => response.into_response(),
    }
}

/// Handle chat completion requests.
#[allow(clippy::too_many_arguments)]
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(upstream): Extension<Upstream>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        &upstream,
        query,
        headers,
        body,
        "chat/completions",
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
}

/// Handle responses API requests.
#[allow(clippy::too_many_arguments)]
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(upstream): Extension<Upstream>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        &upstream,
        query,
        headers,
        body,
        "responses",
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
