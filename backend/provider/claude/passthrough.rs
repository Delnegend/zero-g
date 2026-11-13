//! Anthropic Messages pass-through for Claude Code and other Anthropic-format clients.
//!
//! Claude Code bodies are forwarded byte for byte with only the token swapped. Other
//! clients get the minimum Claude Code identity a subscription token requires.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};

use super::{client, payload};
use crate::provider::{common::request_identity::RequestIdentity, registry};
use crate::server::{AppState, settings::Permissions};

/// Fixed identifier and kind of the Claude provider.
const PROVIDER: &str = "claude";
/// Start of Claude Code's billing attribution block.
const BILLING_PREFIX: &str = "x-anthropic-billing-header:";
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

/// Concatenated text of a message's content.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// Whether a system block's text starts with the given prefix.
fn starts_with(block: &Value, prefix: &str) -> bool {
    block
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| text.starts_with(prefix))
}

/// Whether the body already carries Claude Code's billing block.
fn has_billing(value: &Value) -> bool {
    value
        .get("system")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| starts_with(block, BILLING_PREFIX))
        })
}

/// Billing text with its attestation reset to the placeholder `client::attest` fills.
fn reset_attestation(text: &str) -> String {
    match text.split_once("cch=") {
        Some((head, rest)) => {
            let tail = rest.split_once(';').map_or("", |(_, tail)| tail);
            format!("{head}cch=00000;{tail}")
        }
        None => format!("{text} cch=00000;"),
    }
}

/// Make a body acceptable to a subscription token: plain model name, billing block first
/// awaiting attestation, and the Claude Code identity line.
fn normalize(value: &mut Value, model: &str) -> Result<(), String> {
    let first_user = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| message.get("content"))
        .map(text_of)
        .unwrap_or_default();
    let object = value
        .as_object_mut()
        .ok_or("Request body must be a JSON object")?;
    object.insert("model".into(), json!(model));
    let mut blocks = match object.remove("system") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        Some(Value::Array(blocks)) => blocks,
        Some(_) => return Err("system must be a string or an array of content blocks".into()),
    };
    let billing = if let Some(index) = blocks
        .iter()
        .position(|block| starts_with(block, BILLING_PREFIX))
    {
        blocks
            .remove(index)
            .get("text")
            .and_then(Value::as_str)
            .map(reset_attestation)
            .unwrap_or_default()
    } else {
        if !blocks
            .iter()
            .any(|block| starts_with(block, payload::IDENTITY))
        {
            blocks.insert(0, json!({"type": "text", "text": payload::IDENTITY}));
        }
        payload::billing_header(&first_user)
    };
    // `client::attest` locates the placeholder in a plain first system block.
    blocks.insert(0, json!({"type": "text", "text": billing}));
    object.insert("system".into(), Value::Array(blocks));
    Ok(())
}

/// Request body as sent upstream.
enum Outgoing {
    /// A Claude Code body forwarded byte for byte.
    Exact(Bytes),
    /// A normalized body, attested per credential.
    Normalized {
        /// Body awaiting attestation.
        value: Value,
        /// Whether to add Claude Code's per-credential `metadata.user_id`.
        needs_user_id: bool,
    },
}

/// Client headers minus credentials, with the beta flags a subscription token needs.
fn upstream_headers(incoming: &HeaderMap, normalized: bool) -> HeaderMap {
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
    if normalized {
        if let Ok(agent) = HeaderValue::from_str(&client::user_agent()) {
            headers.insert(header::USER_AGENT, agent);
        }
        headers.insert("x-app", HeaderValue::from_static("cli"));
    }
    headers
}

/// Forward an Anthropic Messages request to Anthropic with a stored subscription token.
pub(crate) async fn messages(
    state: &AppState,
    permissions: &Permissions,
    identity: Option<&RequestIdentity>,
    session: &str,
    headers: &HeaderMap,
    path_and_query: &str,
    body: Bytes,
) -> Response {
    forward(
        state,
        permissions,
        identity,
        session,
        headers,
        &format!("{}{path_and_query}", client::API_ROOT),
        body,
    )
    .await
}

/// Authorize, prepare, and send the request to `url`, streaming the reply back unchanged.
async fn forward(
    state: &AppState,
    permissions: &Permissions,
    identity: Option<&RequestIdentity>,
    session: &str,
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
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "Request body must be JSON");
    };
    let Some(requested) = value.get("model").and_then(Value::as_str) else {
        return error(StatusCode::BAD_REQUEST, "model is required");
    };
    let prefixed = requested.starts_with("claude/");
    let model = requested
        .strip_prefix("claude/")
        .unwrap_or(requested)
        .to_owned();
    if !permissions.allows_model(&model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
        );
    }
    let claude_code = has_billing(&value);
    let outgoing = if claude_code && !prefixed {
        Outgoing::Exact(body)
    } else {
        if let Err(message) = normalize(&mut value, &model) {
            return error(StatusCode::BAD_REQUEST, &message);
        }
        if let Err(message) = client::attest(&value) {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &message);
        }
        let needs_user_id = value.pointer("/metadata/user_id").is_none();
        Outgoing::Normalized {
            value,
            needs_user_id,
        }
    };
    let upstream_headers = upstream_headers(headers, !claude_code);
    let http = match client::http() {
        Ok(http) => http,
        Err(message) => return error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    };
    let attempt = client::first_accepted(&state.settings, &provider.id, |creds, credential| {
        let body = match &outgoing {
            Outgoing::Exact(body) => Ok(body.clone()),
            Outgoing::Normalized {
                value,
                needs_user_id,
            } => {
                let mut value = value.clone();
                if *needs_user_id && let Some(object) = value.as_object_mut() {
                    let user_id =
                        client::user_id(&credential.id, session, creds.account_id.as_deref());
                    let metadata = object.entry("metadata").or_insert_with(|| json!({}));
                    if let Some(metadata) = metadata.as_object_mut() {
                        metadata.insert("user_id".into(), json!(user_id));
                    }
                }
                client::attest(&value).map(Bytes::from)
            }
        };
        let (http, headers) = (&http, upstream_headers.clone());
        async move { client::forward(http, url, &creds.access_token, headers, body?).await }
    })
    .await;
    match attempt {
        Ok((response, credential)) => {
            if response.status().is_success()
                && let Some(identity) = identity
            {
                identity.set_credential(&credential.id, &credential.name);
            }
            crate::provider::openai::forward(response)
        }
        Err((status, message)) => error(status, &message),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
