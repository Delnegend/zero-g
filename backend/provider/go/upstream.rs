use axum::body::Bytes;
use axum::http::{HeaderMap, Method, header};

use super::{DEFAULT_BASE, MODELS_BASE};
use crate::provider::registry::Provider;

/// Select the configured or default base URL for inference or model discovery.
pub(crate) fn base(provider: &Provider, models: bool) -> &str {
    if provider.base_url.is_empty() || provider.base_url.trim_end_matches('/') == DEFAULT_BASE {
        if models { MODELS_BASE } else { DEFAULT_BASE }
    } else {
        &provider.base_url
    }
}

/// Send an authenticated request to an `OpenCode` Go upstream.
pub(crate) async fn send_upstream(
    base: &str,
    key: &str,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, &'static str> {
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let mut request = crate::util::HTTP_CLIENT
        .request(method, url)
        .header(
            header::USER_AGENT,
            concat!("zero-g/", env!("CARGO_PKG_VERSION")),
        )
        .bearer_auth(key);
    for name in [
        "accept",
        "content-type",
        "x-opencode-session",
        "session_id",
        "session-id",
        "x-session-id",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    request
        .send()
        .await
        .map_err(|_| "Failed to contact OpenCode Go provider")
}
