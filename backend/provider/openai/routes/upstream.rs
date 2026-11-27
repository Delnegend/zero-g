use axum::body::Bytes;
use axum::http::{HeaderMap, Method};

use super::Upstream;
use crate::provider::common::proxy::{ProxyError, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::{openai, registry::Provider};
use crate::server::settings::SettingsStore;

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
    if matches!(upstream, Upstream::Codev) {
        return crate::provider::codev::send(
            settings, provider, path, query, method, headers, body, identity,
        )
        .await;
    }
    retry_api_keys(
        settings,
        provider,
        match upstream {
            Upstream::OpenAi(_) => "No connected OpenAI-compatible credentials",
            Upstream::Go => "No connected OpenCode Go credentials",
            Upstream::Codev => "No connected CoDev credentials",
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
                    Upstream::Codev => Err("CoDev requests are sent above"),
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
