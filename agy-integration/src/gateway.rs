//! The one HTTP call both commands make: an authenticated POST to the Zero-G gateway.

use std::time::Duration;

use serde_json::Value;

use crate::config::Settings;

/// The longest error body kept for a failed report.
const MAX_REASON: usize = 300;

/// Post `body` to `path` on the gateway, never following redirects.
pub(crate) async fn post(
    settings: &Settings,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .user_agent(concat!(
            "zero-g-agy-integration/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{}{path}", settings.url))
        .bearer_auth(&settings.key)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("Zero-G did not accept the report: {error}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let reason = read_reason(response).await;
    Err(format!(
        "Zero-G did not accept the report: HTTP {status} {reason}"
    ))
}

/// Read at most `MAX_REASON` characters of an error body.
async fn read_reason(mut response: reqwest::Response) -> String {
    let mut reason = String::new();
    let mut count = 0;
    while count < MAX_REASON
        && let Ok(Some(chunk)) = response.chunk().await
    {
        for character in String::from_utf8_lossy(&chunk).chars() {
            if count == MAX_REASON {
                break;
            }
            reason.push(character);
            count = count.saturating_add(1);
        }
    }
    reason
}
