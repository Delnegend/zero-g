//! The `quota` command: push agy's quota and subscription plan to Zero-G once.

use std::time::Duration;

use serde_json::json;

use crate::{agy, config::Settings, gateway};

/// Longest the push may take.
const PUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// One collection run.
pub(crate) async fn run(settings: &Settings) -> Result<(), String> {
    let account = settings
        .account
        .as_deref()
        .ok_or("set ZERO_G_AGY_ACCOUNT (environment or settings file)")?;
    let report = agy::usage(&settings.agy)
        .await
        .map_err(|error| format!("could not read quota from agy: {error}"))?;
    let mut body = json!({"account": account, "report": report});
    if let (Some(tier), Some(object)) = (
        agy::cached_tier(settings, account).await,
        body.as_object_mut(),
    ) {
        object.insert("tier".to_owned(), tier);
    }
    gateway::post(settings, "/v1/agy/quota", &body, PUSH_TIMEOUT).await
}
