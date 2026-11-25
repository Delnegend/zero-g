//! The one HTTP call both commands make: an authenticated POST to the Zero-G gateway.

use std::time::Duration;

use serde_json::Value;

use crate::config::Settings;

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
    let reason: String = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    Err(format!(
        "Zero-G did not accept the report: HTTP {status} {reason}"
    ))
}
