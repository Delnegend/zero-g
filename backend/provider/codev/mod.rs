/// SSO sign-in and gateway key provisioning.
mod auth;
/// Dashboard sign-in routes.
pub(crate) mod dashboard_login;
/// Model sizes and prices.
mod models;
/// Usage recorded from gateway responses.
pub(crate) mod usage;

pub(crate) use models::catalog_info;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode};

use crate::provider::common::cooldown;
use crate::provider::common::proxy::{ProxyError, error, settings_error};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::login::CredentialError;
use crate::provider::{common, credentials, openai, registry::Provider};
use crate::server::{AppState, settings::SettingsStore};

/// `CoDev` backend.
pub(crate) struct Backend;

/// Sends one request with a credential, re-issuing its gateway key once if the gateway rejects it.
#[allow(clippy::too_many_arguments)]
async fn send_with(
    settings: &SettingsStore,
    provider: &Provider,
    credential: &str,
    path: &str,
    query: Option<&str>,
    method: &Method,
    headers: &HeaderMap,
    body: Option<&Bytes>,
) -> Result<Option<reqwest::Response>, CredentialError> {
    let mut reissue = false;
    loop {
        let Some(gateway) = auth::gateway(settings, &provider.id, credential, reissue).await?
        else {
            return Ok(None);
        };
        let response = openai::send(
            &gateway.base,
            &gateway.api_key,
            path,
            query,
            method.clone(),
            headers,
            body.cloned(),
        )
        .await
        .map_err(|message| CredentialError::from(message.to_owned()))?;
        if response.status() == StatusCode::UNAUTHORIZED && !reissue {
            reissue = true;
            continue;
        }
        return Ok(Some(response));
    }
}

/// Sends a gateway request, trying each connected credential in weighted order.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, ProxyError> {
    let credentials = credentials::list(settings, provider)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    let mut soonest: Option<std::time::Duration> = None;
    let mut sign_in_failed = false;
    for credential in credentials {
        if let Some(wait) = cooldown::remaining(&credential.id) {
            soonest = Some(soonest.map_or(wait, |current| current.min(wait)));
            continue;
        }
        let response = match send_with(
            settings,
            provider,
            &credential.id,
            path,
            query,
            &method,
            headers,
            body.as_ref(),
        )
        .await
        {
            Ok(Some(response)) => response,
            Ok(None) => continue,
            Err(problem) => {
                tracing::warn!(credential = %credential.id, "CoDev credential unusable: {problem}");
                sign_in_failed = true;
                continue;
            }
        };
        let (store, id, response_headers) = (
            settings.clone(),
            credential.id.clone(),
            response.headers().clone(),
        );
        tokio::spawn(async move { usage::record(&store, &id, &response_headers).await });
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            cooldown::start(&credential.id, cooldown::wait_for(response.headers()));
        }
        if !common::errors::retryable_status(Some(response.status())) {
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
    if let Some(response) = last {
        return Ok(response);
    }
    if let Some(wait) = soonest {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "All credentials for this provider are rate limited",
            "rate_limit_error",
            "rate_limited",
        )
        .with_retry_after(wait));
    }
    Err(if sign_in_failed {
        error(
            StatusCode::BAD_GATEWAY,
            "No CoDev credential could get a gateway key; try signing in again",
            "upstream_error",
            "credential_error",
        )
    } else {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No connected CoDev credentials",
            "upstream_error",
            "no_credentials",
        )
    })
}

impl common::Provider for Backend {
    const DESCRIPTOR: common::ProviderDescriptor = common::ProviderDescriptor {
        value: "codev",
        label: None,
        oauth: true,
        api_key: false,
        fixed_id: Some("codev"),
        fixed_name: Some("CoDev"),
        keyless: false,
        default_base_url: None,
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        openai::routes::codev_router(settings)
    }

    async fn discover(
        state: &AppState,
        provider: &Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        let response = send(
            &state.settings,
            provider,
            "models",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
            None,
        )
        .await
        .map_err(|e| e.message().to_owned())?;
        if !response.status().is_success() {
            return Err(format!(
                "CoDev model discovery returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let value = openai::parse_model_list(
            response.bytes_stream(),
            "Failed to read CoDev model list",
            "CoDev model list is too large",
            "Invalid CoDev model list",
        )
        .await?;
        let mut models = openai::models_from_list(&value);
        // Prices are optional extras; a failure here must not hide the models.
        if let Ok(response) = send(
            &state.settings,
            provider,
            "model/info",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
            None,
        )
        .await
            && response.status().is_success()
            && let Ok(info) = response.json::<serde_json::Value>().await
        {
            models::attach_prices(&mut models, &info);
        }
        Ok(models)
    }
}

impl common::QuotaProvider for Backend {
    async fn quota(
        store: &SettingsStore,
        _provider: &str,
        credential: &str,
    ) -> Result<crate::provider::quota::QuotaSnapshot, String> {
        usage::stored(store, credential).await
    }
}

impl common::DashboardProvider for Backend {
    fn dashboard_router(state: AppState) -> Router {
        dashboard_login::router(state)
    }
}
