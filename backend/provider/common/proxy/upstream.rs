use axum::http::StatusCode;
use std::future::Future;

use crate::provider::common::cooldown;
use crate::server::settings::SettingsStore;
use super::{ProxyError, error, settings_error};

/// Resolve weighted API keys in order and retry only credential-related responses.
pub(crate) async fn retry_api_keys<F, Fut>(
    settings: &SettingsStore,
    provider: &crate::provider::registry::Provider,
    no_credentials: &'static str,
    identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
    mut send: F,
) -> Result<reqwest::Response, ProxyError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<reqwest::Response, &'static str>>,
{
    if provider.keyless {
        let response = send(String::new()).await.map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if response.status().is_success()
            && let Some(identity) = identity
        {
            identity.set_credential("keyless", "Keyless");
        }
        return Ok(response);
    }
    let credentials = crate::provider::credentials::list(settings, provider)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    let mut soonest: Option<std::time::Duration> = None;
    for credential in credentials {
        if let Some(wait) = cooldown::remaining(&credential.id) {
            soonest = Some(soonest.map_or(wait, |current| current.min(wait)));
            continue;
        }
        let key = settings
            .secret_get(&crate::provider::credentials::SecretKind::ApiKey.name(&credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = send(key).await.map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            cooldown::start(&credential.id, cooldown::wait_for(response.headers()));
        }
        if !crate::provider::common::errors::retryable_status(Some(response.status())) {
            if response.status().is_success() {
                cooldown::clear(&credential.id);
                if let Some(identity) = identity {
                    identity.set_credential(&credential.id, &credential.name);
                }
            }
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        soonest.map_or_else(
            || {
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    no_credentials,
                    "upstream_error",
                    "no_credentials",
                )
            },
            |wait| {
                error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "All credentials for this provider are rate limited",
                    "rate_limit_error",
                    "rate_limited",
                )
                .with_retry_after(wait)
            },
        )
    })
}
