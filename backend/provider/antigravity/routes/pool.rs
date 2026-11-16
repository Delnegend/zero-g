//! Credential-pool selection and upstream request handling.

use crate::openai::ChatCompletionRequest;
use crate::provider::antigravity::{AntigravityClient, AntigravityModelInfo, AuthManager};
use crate::provider::common::cooldown;
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
    /// Every credential is temporarily rate limited.
    Cooling(std::time::Duration),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(message) => f.write_str(message),
            Self::Upstream(error) => write!(f, "{error}"),
            Self::Cooling(_) => f.write_str("All Antigravity accounts are rate limited"),
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
    let credentials = eligible_credentials(state, provider).await?;
    with_candidates(state, provider, identity, credentials, operation).await
}

/// Loads the weighted credential order, excluding accounts in cooldown.
async fn eligible_credentials(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<crate::provider::credentials::Credential>, PoolError> {
    let mut credentials = crate::provider::credentials::list(&state.settings, provider)
        .await
        .map_err(PoolError::Internal)?;
    let mut earliest = None;
    credentials.retain(|credential| {
        cooldown::remaining(&credential.id).is_none_or(|wait| {
            earliest = Some(earliest.map_or(wait, |old: std::time::Duration| old.min(wait)));
            false
        })
    });
    if credentials.is_empty()
        && let Some(wait) = earliest
    {
        return Err(PoolError::Cooling(wait));
    }
    Ok(credentials)
}

/// Executes an operation in the supplied weighted credential order.
async fn with_candidates<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    credentials: Vec<crate::provider::credentials::Credential>,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    try_candidates(credentials, |credential| {
        let operation = &operation;
        async move {
            let creds = AuthManager::credentials(&state.settings, &provider.id, &credential.id)
                .await
                .map_err(|error| {
                    if error.upstream_status == Some(StatusCode::TOO_MANY_REQUESTS) {
                        cooldown::start(
                            &credential.id,
                            cooldown::wait_for(&reqwest::header::HeaderMap::new()),
                        );
                    }
                    match error.upstream_status {
                        status if retryable_status(status) => PoolError::Upstream(UpstreamError {
                            status,
                            message: error.message,
                            headers: Box::default(),
                        }),
                        _ => PoolError::Internal(error.message),
                    }
                })?
                .ok_or_else(|| {
                    PoolError::Internal("Antigravity credentials no longer exist".into())
                })?;
            let result = operation(creds).await.map_err(|error| {
                if error.status == Some(StatusCode::TOO_MANY_REQUESTS) {
                    cooldown::start(&credential.id, cooldown::wait_for(&error.headers));
                }
                PoolError::Upstream(error)
            })?;
            cooldown::clear(&credential.id);
            if let Some(identity) = identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            Ok(result)
        }
    })
    .await
}

/// Discovers each eligible account independently, retaining its weighted order.
async fn account_models(
    state: &AppState,
    provider: &Provider,
    credentials: &[crate::provider::credentials::Credential],
) -> Vec<Result<Vec<(String, AntigravityModelInfo)>, PoolError>> {
    let mut catalogs = Vec::with_capacity(credentials.len());
    for credential in credentials {
        catalogs.push(with_candidates(state, provider, None, vec![credential.clone()], |creds| async move {
            state.providers.client.fetch_models(&creds).await
        }).await);
    }
    catalogs
}

/// Runs generation on advertising accounts, or the original pool if none advertise.
pub(super) async fn with_model_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    model: &str,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = eligible_credentials(state, provider).await?;
    let catalogs = account_models(state, provider, &credentials).await;
    let mut earliest = None;
    let eligible: Vec<_> = credentials.into_iter().zip(catalogs).filter(|(credential, _)| {
        cooldown::remaining(&credential.id).is_none_or(|wait| {
            earliest = Some(earliest.map_or(wait, |old: std::time::Duration| old.min(wait)));
            false
        })
    }).collect();
    if eligible.is_empty() && let Some(wait) = earliest {
        return Err(PoolError::Cooling(wait));
    }
    let candidates = select_advertising(eligible, model);
    with_candidates(state, provider, identity, candidates, operation).await
}

/// Filters a weighted pool only when at least one account advertises the model.
fn select_advertising<C>(
    candidates: Vec<(C, Result<Vec<(String, AntigravityModelInfo)>, PoolError>)>,
    model: &str,
) -> Vec<C> {
    let has_match = candidates.iter().any(|(_, catalog)| catalog.as_ref().is_ok_and(|models| models.iter().any(|(id, _)| id == model)));
    candidates.into_iter().filter_map(|(candidate, catalog)| {
        (!has_match || catalog.as_ref().is_ok_and(|models| models.iter().any(|(id, _)| id == model))).then_some(candidate)
    }).collect()
}

#[cfg(test)]
mod selection_tests {
    use super::{PoolError, select_advertising};
    use crate::provider::antigravity::AntigravityModelInfo;

    fn catalog(ids: &[&str]) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
        Ok(ids.iter().map(|id| ((*id).to_owned(), serde_json::from_value(serde_json::json!({})).unwrap())).collect())
    }

    #[test]
    fn selects_advertisers_in_weighted_order_and_preserves_unknown_models() {
        for reverse in [false, true] {
            let mut accounts = vec![("mvn", catalog(&["claude-sonnet-4-6"])), ("kien", catalog(&["claude-sonnet-5-5-medium"]))];
            if reverse { accounts.reverse(); }
            assert_eq!(select_advertising(accounts, "claude-sonnet-5-5-medium"), vec!["kien"]);
            let mut accounts = vec![("mvn", catalog(&["claude-sonnet-4-6"])), ("kien", catalog(&["claude-sonnet-5-5-medium"]))];
            if reverse { accounts.reverse(); }
            let expected = if reverse { vec!["kien", "mvn"] } else { vec!["mvn", "kien"] };
            assert_eq!(select_advertising(accounts, "undiscovered"), expected);
        }
    }
}

/// Collects discovery across accounts so aliases do not depend on pool order.
pub(super) async fn all_pool_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    let credentials = eligible_credentials(state, provider).await?;
    let catalogs = account_models(state, provider, &credentials).await;
    let mut models = Vec::new();
    let mut first_error = None;
    let mut succeeded = false;
    for catalog in catalogs {
        match catalog {
            Ok(catalog) => {
                succeeded = true;
                for (id, info) in catalog {
                    if !models.iter().any(|(existing, _)| existing == &id) {
                        models.push((id, info));
                    }
                }
            }
            Err(error) => { if first_error.is_none() { first_error = Some(error); } }
        }
    }
    if succeeded { Ok(models) } else {
        Err(first_error.unwrap_or_else(|| PoolError::Internal("Google Antigravity is not configured. Authenticate on the dashboard Providers page.".into())))
    }
}

/// Starts an upstream chat stream using the credential pool.
pub(super) async fn pooled_stream(
    state: &AppState,
    provider: &Provider,
    req: &ChatCompletionRequest,
    session_id: &str,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, PoolError> {
    with_model_pool(state, provider, identity, &req.model, |creds| async move {
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
    let (status, kind, code, message, headers) = match error {
        PoolError::Cooling(wait) => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(crate::server::error_body(
                    "All Antigravity accounts are rate limited",
                    "rate_limit_error",
                    Some("rate_limited"),
                )),
            )
                .into_response();
            if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            return response;
        }
        PoolError::Internal(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal_error",
            message,
            None,
        ),
        PoolError::Upstream(error) => {
            let limited = error.status == Some(StatusCode::TOO_MANY_REQUESTS);
            (
                if limited {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::BAD_GATEWAY
                },
                if limited {
                    "rate_limit_error"
                } else {
                    "upstream_error"
                },
                if limited {
                    "rate_limited"
                } else {
                    "bad_gateway"
                },
                error.message,
                Some(error.headers),
            )
        }
    };
    let mut response = (
        status,
        Json(crate::server::error_body(&message, kind, Some(code))),
    )
        .into_response();
    if let Some(headers) = headers {
        cooldown::copy_rate_limit_headers(&headers, response.headers_mut());
    }
    response
}
