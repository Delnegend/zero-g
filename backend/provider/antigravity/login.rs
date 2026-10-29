use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::provider::antigravity::auth::{ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT};

/// Project and tier information returned by Cloud Code Assist.
#[derive(Deserialize)]
struct LoadCodeAssistResponse {
    /// Assigned Cloud AI Companion project identifier.
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
    /// Current tier metadata, which may contain a name.
    #[serde(default, rename = "currentTier")]
    current_tier: Option<serde_json::Value>,
}

/// Status of a Cloud Code Assist onboarding operation.
#[derive(Deserialize)]
struct OnboardOperation {
    /// Operation name used to poll its status.
    name: Option<String>,
    /// Whether the operation has finished.
    done: Option<bool>,
    /// Error returned by the operation, if any.
    error: Option<serde_json::Value>,
}

/// Discovers a project, onboarding the account if needed, with an email fallback.
pub(crate) async fn fetch_or_onboard_project(
    client: &reqwest::Client,
    access_token: &str,
    email: Option<&str>,
) -> Result<String, String> {
    let load_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:loadCodeAssist");
    let load_body = json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let load_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&load_body)
        .send()
        .await
        .map_err(|e| format!("Failed to call loadCodeAssist: {e}"))?;

    if load_resp.status().is_success() {
        let load_data: LoadCodeAssistResponse = load_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse loadCodeAssist response: {e}"))?;

        if let Some(pid) = load_data.cloudaicompanion_project
            && !pid.is_empty()
        {
            return Ok(pid);
        }

        if let Some(tier) = load_data.current_tier
            && let Some(name) = tier.get("name").and_then(|n| n.as_str())
            && !name.is_empty()
        {
            return Ok(name.to_string());
        }
    }

    onboard_user(client, access_token).await?;

    let retry_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&load_body)
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist retry failed: {e}"))?;

    if retry_resp.status().is_success() {
        let load_data: LoadCodeAssistResponse = retry_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse loadCodeAssist retry: {e}"))?;

        if let Some(pid) = load_data.cloudaicompanion_project
            && !pid.is_empty()
        {
            return Ok(pid);
        }
    }

    if let Some(email) = email {
        let clean = email.replace(['@', '.'], "-");
        let fallback = format!("ag-{clean}");
        return Ok(fallback);
    }

    Err("Could not discover or onboard project ID".to_string())
}

/// Starts free-tier onboarding and polls the resulting operation.
async fn onboard_user(client: &reqwest::Client, access_token: &str) -> Result<(), String> {
    let onboard_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:onboardUser");
    let onboard_body = json!({
        "tierType": "TIER_FREE"
    });

    let resp = client
        .post(&onboard_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&onboard_body)
        .send()
        .await
        .map_err(|e| format!("onboardUser request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("onboardUser endpoint failed ({status}): {body}"));
    }

    let mut op: OnboardOperation = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse onboardUser response: {e}"))?;

    let poll_interval = Duration::from_secs(2);
    let max_polls = 15;

    for _ in 0..max_polls {
        if op.done == Some(true) {
            break;
        }

        tokio::time::sleep(poll_interval).await;

        let Some(op_name) = &op.name else {
            break;
        };

        let poll_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal/{op_name}");
        let poll_resp = client
            .get(&poll_url)
            .bearer_auth(access_token)
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .send()
            .await
            .map_err(|e| format!("Failed to poll operation: {e}"))?;

        if !poll_resp.status().is_success() {
            break;
        }

        op = poll_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse operation status: {e}"))?;
    }

    if let Some(err) = op.error {
        return Err(format!("onboardUser operation failed: {err}"));
    }

    Ok(())
}
