//! Credential-pool selection and upstream request handling.


use axum::response::Response;

use crate::openai::ChatCompletionRequest;
use crate::provider::antigravity::{AntigravityClient, AntigravityModelInfo, AuthManager};
use crate::provider::common::cooldown;
use crate::provider::common::errors::UpstreamError;
use crate::provider::common::pool::{eligible, rank_accounts, run_candidates};
pub(super) use crate::provider::common::pool::PoolError;
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::credentials::Credential;
use crate::provider::registry::Provider;
use crate::server::AppState;

/// Message returned when no Antigravity account is available.
const NOT_CONFIGURED: &str =
    "Google Antigravity is not configured. Authenticate on the dashboard Providers page.";

/// Tries credentials in order, advancing on retryable upstream failures.
#[cfg(test)]
pub(super) async fn try_candidates<C, T, F, Fut>(
    candidates: Vec<C>,
    attempt: F,
) -> Result<T, PoolError>
where
    F: FnMut(C) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    crate::provider::common::pool::try_candidates(candidates, NOT_CONFIGURED, attempt).await
}

/// Executes an operation in the supplied weighted credential order.
async fn with_candidates<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    credentials: Vec<Credential>,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(String, crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    run_candidates(
        credentials,
        identity,
        NOT_CONFIGURED,
        |credential| {
            let id = credential.id.clone();
            async move {
                AuthManager::credentials(&state.settings, &provider.id, &id)
                    .await
                    .map_err(PoolError::from_credential)
                    .and_then(|creds| {
                        creds.ok_or_else(|| {
                            PoolError::Internal("Antigravity credentials no longer exist".into())
                        })
                    })
                    .map(|creds| Some((id, creds)))
            }
        },
        |(credential_id, creds)| operation(credential_id, creds),
    )
    .await
}

/// Discovers each eligible account independently, retaining its weighted order.
async fn account_models(
    state: &AppState,
    provider: &Provider,
    credentials: &[Credential],
) -> Vec<Result<Vec<(String, AntigravityModelInfo)>, PoolError>> {
    let mut catalogs = Vec::with_capacity(credentials.len());
    for credential in credentials {
        catalogs.push(
            with_candidates(
                state,
                provider,
                None,
                vec![credential.clone()],
                |credential_id, creds| async move {
                    state
                        .providers
                        .client
                        .fetch_models(Some(&credential_id), &creds)
                        .await
                },
            )
            .await,
        );
    }
    catalogs
}

/// Runs generation on advertising accounts (plus failed lookups), or the full pool if none advertise.
pub(super) async fn with_model_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    model: &str,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(String, crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = eligible(&state.settings, provider).await?;
    let catalogs = account_models(state, provider, &credentials).await;
    let mut earliest = None;
    let eligible_accounts: Vec<_> = credentials
        .into_iter()
        .zip(catalogs)
        .filter(|(credential, _)| {
            cooldown::remaining(&credential.id).is_none_or(|wait| {
                earliest = Some(earliest.map_or(wait, |old: std::time::Duration| old.min(wait)));
                false
            })
        })
        .collect();
    if eligible_accounts.is_empty()
        && let Some(wait) = earliest
    {
        return Err(PoolError::Cooling(wait));
    }
    let candidates = rank_accounts(eligible_accounts, model);
    with_candidates(state, provider, identity, candidates, operation).await
}

/// Collects discovery across accounts so aliases and `/v1/models` include every account's models.
pub(super) async fn all_pool_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    let credentials = eligible(&state.settings, provider).await?;
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
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if succeeded {
        Ok(models)
    } else {
        Err(first_error.unwrap_or_else(|| PoolError::Internal(NOT_CONFIGURED.into())))
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
    with_model_pool(
        state,
        provider,
        identity,
        &req.model,
        |credential_id, creds| async move {
            let models = state
                .providers
                .client
                .fetch_models(Some(&credential_id), &creds)
                .await?;
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
                .stream_request(
                    Some(&credential_id),
                    &creds,
                    &payload,
                    req.model.contains("claude"),
                )
                .await
        },
    )
    .await
}

/// Fetches upstream models, converting pool failures to strings.
pub(crate) async fn fetch_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
    all_pool_models(state, provider)
        .await
        .map_err(|error| error.to_string())
}

/// Converts a pool failure into an API error response.
pub(super) fn pool_error_response(error: PoolError) -> Response {
    error.into_response("All Antigravity accounts are rate limited", false)
}
