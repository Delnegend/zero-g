use std::sync::LazyLock;

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, header};

/// Shared client used for OpenAI-compatible upstream requests.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        // Never send the upstream credential to a redirect target.
        .redirect(reqwest::redirect::Policy::none())
        .build()
});

/// Send an authenticated OpenAI-compatible upstream request.
pub(crate) async fn send(
    base: &str,
    key: &str,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, &'static str> {
    send_with_headers(base, key, path, query, method, headers, body, false).await
}

/// Forward additional end-to-end headers without exposing the caller's virtual key.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_with_headers(
    base: &str,
    key: &str,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
    all_headers: bool,
) -> Result<reqwest::Response, &'static str> {
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize OpenAI-compatible provider")?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        let allowed = name == header::ACCEPT
            || name == header::CONTENT_TYPE
            || (all_headers
                && !matches!(
                    name.as_str(),
                    "authorization"
                        | "cookie"
                        | "x-api-key"
                        | "proxy-connection"
                        | "host"
                        | "content-length"
                        | "connection"
                        | "transfer-encoding"
                        | "keep-alive"
                        | "proxy-authorization"
                        | "proxy-authenticate"
                        | "te"
                        | "trailer"
                        | "upgrade"
                ));
        if allowed {
            request = request.header(name, value);
        }
    }
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    // Reqwest errors can contain URLs, which may contain credentials.
    request
        .send()
        .await
        .map_err(|_| "Failed to contact OpenAI-compatible provider")
}
