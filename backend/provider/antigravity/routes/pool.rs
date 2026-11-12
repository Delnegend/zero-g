//! Credential-pool selection and upstream request handling.

use crate::openai::ChatCompletionRequest;
use crate::provider::antigravity::{AntigravityClient, AntigravityModelInfo, AuthManager};
use crate::provider::common::errors::retryable_status;
use crate::provider::common::{errors::UpstreamError, request_identity::RequestIdentity};
use crate::provider::registry::Provider;
use crate::server::AppState;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use std::future::Future;

/// Failure while selecting credentials or contacting the upstream service.
#[derive(Debug)]
pub(super) enum PoolError {
    /// A local configuration or credential failure.
    Internal(String),
    /// An upstream request failure.
    Upstream(UpstreamError),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(message) => f.write_str(message),
            Self::Upstream(error) => write!(f, "{error}"),
        }
    }
}

/// Tries credentials in order, advancing on retryable upstream failures.
pub(super) async fn try_candidates<C, T, F, Fut>(
    candidates: Vec<C>,
    mut attempt: F,
) -> Result<T, PoolError>
where
    F: FnMut(C) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    let mut candidates = candidates.into_iter().peekable();
    while let Some(candidate) = candidates.next() {
        match attempt(candidate).await {
            Err(PoolError::Upstream(error)) if error.retryable() && candidates.peek().is_some() => {
            }
            result => return result,
        }
    }
    Err(PoolError::Internal(
        "Google Antigravity is not configured. Authenticate on the dashboard Providers page."
            .into(),
    ))
}

/// Loads pooled credentials and runs an operation with each eligible candidate.
pub(super) async fn with_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id)
        .await
        .map_err(PoolError::Internal)?;
    try_candidates(credentials, |credential| {
        let store = state.settings.clone();
        let provider_id = provider.id.clone();
        let operation = &operation;
        async move {
            let auth = AuthManager::load(store, &provider_id, &credential.id)
                .await
                .map_err(PoolError::Internal)?
                .ok_or_else(|| {
                    PoolError::Internal("Antigravity credentials no longer exist".into())
                })?;
            let creds =
                auth.get_credentials()
                    .await
                    .map_err(|error| match error.upstream_status {
                        status if retryable_status(status) => PoolError::Upstream(UpstreamError {
                            status,
                            message: error.message,
                        }),
                        _ => PoolError::Internal(error.message),
                    })?;
            let result = operation(creds).await.map_err(PoolError::Upstream)?;
            if let Some(identity) = identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            Ok(result)
        }
    })
    .await
}

/// Starts an upstream chat stream using the credential pool.
pub(super) async fn pooled_stream(
    state: &AppState,
    provider: &Provider,
    req: &ChatCompletionRequest,
    session_id: &str,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, PoolError> {
    with_pool(state, provider, identity, |creds| async move {
        let models = state.providers.client.fetch_models(&creds).await?;
        let info = models
            .iter()
            .find(|(id, _)| id == &req.model)
            .map(|(_, info)| info);
        let payload = AntigravityClient::build_request_payload(
            req,
            &creds.project_id,
            &req.model,
            session_id,
            info,
        );
        state
            .providers
            .client
            .stream_request(&creds, &payload, req.model.contains("claude"))
            .await
    })
    .await
}

/// Fetches upstream models using the credential pool.
pub(super) async fn pool_models(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    with_pool(state, provider, identity, |creds| async move {
        state.providers.client.fetch_models(&creds).await
    })
    .await
}

/// Fetches upstream models, converting pool failures to strings.
pub(crate) async fn fetch_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
    pool_models(state, provider, None)
        .await
        .map_err(|error| error.to_string())
}

/// Converts a pool failure into an API error response.
pub(super) fn pool_error_response(error: PoolError) -> Response {
    let (status, kind, code, message) = match error {
        PoolError::Internal(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal_error",
            message,
        ),
        PoolError::Upstream(error) => (
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "bad_gateway",
            error.message,
        ),
    };
    (
        status,
        Json(crate::server::error_body(&message, kind, Some(code))),
    )
        .into_response()
}
