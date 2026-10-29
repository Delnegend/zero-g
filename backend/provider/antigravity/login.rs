use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::provider::antigravity::auth::ANTIGRAVITY_USER_AGENT;
/// Project information returned by Cloud Code Assist.
#[derive(Deserialize)]
struct LoadCodeAssistResponse {
    /// Assigned Cloud AI Companion project identifier.
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
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
    /// Onboarding result, possibly containing the assigned project.
    response: Option<LoadCodeAssistResponse>,
}

/// Uses the supplied Cloud Code Assist endpoint for project discovery.
pub(super) async fn fetch_or_onboard_project_at(
    client: &reqwest::Client,
    access_token: &str,
    endpoint: &str,
) -> Result<String, String> {
    let load_url = format!("{endpoint}/v1internal:loadCodeAssist");
    let load_body = json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
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

    if !load_resp.status().is_success() {
        return Err(format!(
            "loadCodeAssist endpoint failed ({})",
            load_resp.status()
        ));
    }
    let load_data: LoadCodeAssistResponse = load_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse loadCodeAssist response: {e}"))?;
    if let Some(pid) = load_data.cloudaicompanion_project
        && !pid.trim().is_empty()
    {
        return Ok(pid);
    }

    if let Some(project) = onboard_user(client, access_token, endpoint).await? {
        return Ok(project);
    }

    let retry_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&load_body)
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist retry failed: {e}"))?;

    if !retry_resp.status().is_success() {
        return Err(format!(
            "loadCodeAssist retry failed ({})",
            retry_resp.status()
        ));
    }
    let load_data: LoadCodeAssistResponse = retry_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse loadCodeAssist retry: {e}"))?;
    if let Some(pid) = load_data.cloudaicompanion_project
        && !pid.trim().is_empty()
    {
        return Ok(pid);
    }

    Err("Could not discover or onboard project ID".to_string())
}

/// Starts free-tier onboarding and polls the resulting operation.
async fn onboard_user(
    client: &reqwest::Client,
    access_token: &str,
    endpoint: &str,
) -> Result<Option<String>, String> {
    let onboard_url = format!("{endpoint}/v1internal:onboardUser");
    let onboard_body = json!({
        "tierId": "free-tier",
        "metadata": { "ideType": "ANTIGRAVITY" }
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
        if let Some(err) = &op.error {
            return Err(format!("onboardUser operation failed: {err}"));
        }
        if op.done == Some(true) {
            return Ok(op
                .response
                .and_then(|response| response.cloudaicompanion_project)
                .filter(|project| !project.trim().is_empty()));
        }

        let Some(op_name) = &op.name else {
            return Err("onboardUser operation has no name".to_string());
        };
        // Operation names are relative API paths, never arbitrary URLs.
        if op_name.is_empty()
            || op_name.split('/').any(str::is_empty)
            || !op_name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
            })
            || op_name.split('/').any(|part| part == "." || part == "..")
        {
            return Err("Invalid onboardUser operation name".to_string());
        }
        tokio::time::sleep(poll_interval).await;

        let poll_url = format!("{endpoint}/v1internal/{op_name}");
        let poll_resp = client
            .get(&poll_url)
            .bearer_auth(access_token)
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .send()
            .await
            .map_err(|e| format!("Failed to poll operation: {e}"))?;

        if !poll_resp.status().is_success() {
            return Err(format!(
                "onboardUser polling failed ({})",
                poll_resp.status()
            ));
        }

        op = poll_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse operation status: {e}"))?;
    }

    if let Some(err) = op.error {
        return Err(format!("onboardUser operation failed: {err}"));
    }
    if op.done == Some(true) {
        Ok(op
            .response
            .and_then(|response| response.cloudaicompanion_project)
            .filter(|project| !project.trim().is_empty()))
    } else {
        Err("onboardUser operation timed out".to_string())
    }
}
