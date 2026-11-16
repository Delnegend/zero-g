use axum::body::Bytes;
use axum::http::{StatusCode, header};

use super::protocol::{ChatJimmyStats, parse_upstream};
use super::{CHAT_PATH, CLIENT, DEFAULT_BASE};
use crate::provider::common::proxy::{ProxyError, error, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::registry;
use crate::server::settings::SettingsStore;

/// Sends a request upstream to `ChatJimmy` and parses the response.
pub(super) async fn fetch_chatjimmy(
    settings: &SettingsStore,
    provider: &registry::Provider,
    payload_bytes: Bytes,
    identity: Option<&RequestIdentity>,
) -> Result<(String, Option<ChatJimmyStats>), ProxyError> {
    let base_url = if provider.base_url.is_empty() {
        DEFAULT_BASE
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!("{base_url}{CHAT_PATH}");

    let response = retry_api_keys(
        settings,
        provider,
        "No connected ChatJimmy credentials",
        identity,
        |_key| {
            let url = url.clone();
            let payload = payload_bytes.clone();
            async move {
                let client = CLIENT
                    .as_ref()
                    .map_err(|_| "Failed to initialize ChatJimmy client")?;
                client
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::USER_AGENT,
                        concat!("zero-g/", env!("CARGO_PKG_VERSION")),
                    )
                    .body(payload)
                    .send()
                    .await
                    .map_err(|_| "Failed to contact ChatJimmy upstream")
            }
        },
    )
    .await?;

    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "ChatJimmy upstream returned error status",
            "upstream_error",
            "bad_gateway",
        ));
    }

    let Ok(raw_text) = response.text().await else {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Failed to read ChatJimmy response",
            "upstream_error",
            "bad_gateway",
        ));
    };

    if raw_text.trim().is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "ChatJimmy returned empty response (input likely exceeded the ~6,000 token limit)",
            "invalid_request_error",
            "context_length_exceeded",
        ));
    }

    Ok(parse_upstream(&raw_text))
}
