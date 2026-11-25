//! Anthropic Messages pass-through for Claude Code and other Anthropic-format clients.
//!
//! Bodies are forwarded byte for byte with only the token swapped. Clients must send their
//! own Claude Code identity; the gateway adds none.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use super::upstream as client;
use crate::provider::{common::request_identity::RequestIdentity, registry};
use crate::server::{AppState, settings::Permissions};

/// Fixed identifier and kind of the Claude provider.
const PROVIDER: &str = "claude";
/// Request headers that are never forwarded: credentials, hop-by-hop, and rebuilt ones.
const DROPPED: [&str; 14] = [
    "authorization",
    "x-api-key",
    "cookie",
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    // Compressed replies would hide token usage from request capture.
    "accept-encoding",
];

/// Anthropic-shaped error response, which Anthropic clients display verbatim.
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    let kind = match status.as_u16() {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        _ => "api_error",
    };
    (
        status,
        Json(json!({"type": "error", "error": {"type": kind, "message": message}})),
    )
        .into_response()
}

/// Client headers minus credentials, with the beta flags a subscription token needs.
fn upstream_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(incoming.len());
    for (name, value) in incoming {
        if !DROPPED.contains(&name.as_str()) && name.as_str() != "anthropic-beta" {
            headers.append(name.clone(), value.clone());
        }
    }
    let mut betas: Vec<&str> = incoming
        .get_all("anthropic-beta")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|beta| !beta.is_empty())
        .collect();
    for beta in client::REQUIRED_BETAS {
        if !betas.contains(&beta) {
            betas.push(beta);
        }
    }
    if let Ok(value) = HeaderValue::from_str(&betas.join(",")) {
        headers.insert("anthropic-beta", value);
    }
    headers
        .entry("anthropic-version")
        .or_insert(HeaderValue::from_static("2023-06-01"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers
}

/// Forward an Anthropic Messages request to Anthropic with a stored subscription token.
pub(crate) async fn messages(
    state: &AppState,
    permissions: &Permissions,
    identity: Option<&RequestIdentity>,
    headers: &HeaderMap,
    path_and_query: &str,
    body: Bytes,
) -> Response {
    forward(
        state,
        permissions,
        identity,
        headers,
        &format!("{}{path_and_query}", client::API_ROOT),
        body,
    )
    .await
}

/// Extract the origin prefix before `/v1/` for catalog discovery.
fn origin(url: &str) -> &str {
    url.split_once("/v1/")
        .map_or(client::API_ROOT, |(prefix, _)| prefix)
}

/// Authorize, prepare, and send the request to `url`, streaming the reply back unchanged.
async fn forward(
    state: &AppState,
    permissions: &Permissions,
    identity: Option<&RequestIdentity>,
    headers: &HeaderMap,
    url: &str,
    body: Bytes,
) -> Response {
    if !permissions.providers.is_empty() && !permissions.providers.iter().any(|p| p == PROVIDER) {
        return error(
            StatusCode::FORBIDDEN,
            "Virtual key does not permit this provider",
        );
    }
    let provider = match registry::get(&state.settings.database, PROVIDER).await {
        Ok(Some(provider)) if provider.kind == PROVIDER => provider,
        Ok(_) => {
            return error(StatusCode::NOT_FOUND, "Claude provider is not configured");
        }
        Err(_) => {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load provider");
        }
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "Request body must be JSON");
    };
    let Some(model) = value.get("model").and_then(serde_json::Value::as_str) else {
        return error(StatusCode::BAD_REQUEST, "model is required");
    };
    if !permissions.allows_model(model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
        );
    }
    let upstream_headers = upstream_headers(headers);
    let attempt = client::first_accepted_at(
        &state.settings,
        &provider,
        origin(url),
        model,
        |creds, _credential| {
            let headers = upstream_headers.clone();
            let body = body.clone();
            async move {
                client::forward(
                    &client::HTTP_CLIENT,
                    url,
                    &creds.access_token,
                    headers,
                    body,
                )
                .await
            }
        },
    )
    .await;
    match attempt {
        Ok((response, credential)) => {
            if response.status().is_success()
                && let Some(identity) = identity
            {
                identity.set_credential(&credential.id, &credential.name);
            }
            let wait = (response.status() == StatusCode::TOO_MANY_REQUESTS
                && !response.headers().contains_key("retry-after"))
            .then(|| crate::provider::common::cooldown::wait_for(response.headers()));
            let mut reply = crate::provider::openai::forward(response);
            if let Some(wait) = wait {
                let secs = crate::provider::common::cooldown::retry_after_secs(wait);
                if let Ok(value) = secs.to_string().parse() {
                    reply.headers_mut().insert("retry-after", value);
                }
            }
            reply
        }
        Err(client::Refusal::CoolingDown(wait)) => {
            let mut response = error(
                StatusCode::TOO_MANY_REQUESTS,
                "All Claude accounts are rate limited; retry after the Retry-After delay",
            );
            let secs = crate::provider::common::cooldown::retry_after_secs(wait);
            if let Ok(value) = secs.to_string().parse() {
                response.headers_mut().insert("retry-after", value);
            }
            response
        }
        Err(client::Refusal::Failed(status, message)) => error(status, &message),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
