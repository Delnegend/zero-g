use axum::body::Bytes;
use axum::http::{HeaderMap, Method};

use crate::provider::common::proxy::{ProxyError, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::{openai, registry::Provider};
use crate::server::settings::SettingsStore;

/// Forward a request with weighted Zen credentials.
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_upstream(
    settings: &SettingsStore,
    provider: &Provider,
    base: &str,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, ProxyError> {
    retry_api_keys(
        settings,
        provider,
        "No connected Zen credentials",
        identity,
        |key| {
            let body = body.clone();
            async move {
                openai::send_with_headers(
                    base,
                    &key,
                    path,
                    query,
                    Method::POST,
                    headers,
                    Some(body),
                    true,
                )
                .await
            }
        },
    )
    .await
}
