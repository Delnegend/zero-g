use axum::body::Bytes;
use axum::http::{HeaderMap, Method};

use crate::provider::common::proxy::{ProxyError, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::{openai, registry::Provider};
use crate::server::settings::SettingsStore;

/// Send a Decisions request with weighted OpenRouter credentials.
pub(super) async fn send_decision(
    settings: &SettingsStore,
    provider: &Provider,
    api_root: &str,
    identity: Option<&RequestIdentity>,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<reqwest::Response, ProxyError> {
    retry_api_keys(
        settings,
        provider,
        "No connected OpenRouter credentials",
        identity,
        |key| {
            let body = body.clone();
            async move {
                openai::send(
                    api_root,
                    &key,
                    "alpha/decisions",
                    query,
                    Method::POST,
                    headers,
                    Some(body),
                )
                .await
            }
        },
    )
    .await
}
