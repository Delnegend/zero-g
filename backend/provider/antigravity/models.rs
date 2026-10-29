use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provider::antigravity::AntigravityClient;
use crate::provider::antigravity::auth::{
    ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials,
};
use crate::provider::antigravity::client::UpstreamError;

pub(super) const SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
pub(super) const CLAUDE_THINKING_BETA_HEADER: &str = "interleaved-thinking-2025-05-14";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AntigravityDiscoveryResponse {
    #[serde(default)]
    models: HashMap<String, AntigravityModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AntigravityModelInfo {
    #[serde(default, rename = "displayName")]
    pub(crate) display_name: Option<String>,
    #[serde(default, rename = "supportsThinking")]
    pub(crate) supports_thinking: Option<bool>,
    #[serde(default, rename = "maxTokens")]
    pub(crate) max_tokens: Option<u64>,
    #[serde(default, rename = "maxOutputTokens")]
    pub(crate) max_output_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) disabled: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: HashMap<String, serde_json::Value>,
}

impl AntigravityClient {
    pub(crate) async fn fetch_models(
        &self,
        creds: &Credentials,
    ) -> Result<Vec<(String, AntigravityModelInfo)>, UpstreamError> {
        let endpoints = [CLOUD_CODE_ASSIST_ENDPOINT, SANDBOX_ENDPOINT];
        let mut last_err = UpstreamError::from("No upstream endpoint available".to_string());

        for endpoint in &endpoints {
            let url = format!("{endpoint}/v1internal:fetchAvailableModels");
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", creds.access_token))
                .header("Content-Type", "application/json")
                .header("User-Agent", ANTIGRAVITY_USER_AGENT)
                .json(&json!({}))
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let disc: AntigravityDiscoveryResponse = r.json().await.map_err(|e| {
                        UpstreamError::from(format!("Failed to parse models response: {e}"))
                    })?;
                    let mut models = Vec::new();
                    for (id, info) in disc.models {
                        if info.disabled.unwrap_or(false) {
                            continue;
                        }
                        if id.starts_with("chat_") || id.starts_with("tab_") {
                            continue;
                        }
                        models.push((id, info));
                    }
                    models.sort_by(|a, b| a.0.cmp(&b.0));
                    return Ok(models);
                }
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    last_err = UpstreamError {
                        status: Some(status),
                        message: format!("Endpoint {endpoint} returned {status}: {body}"),
                    };
                    if !status.is_server_error() {
                        return Err(last_err);
                    }
                }
                Err(e) => {
                    last_err = UpstreamError::from(format!("Endpoint {endpoint} failed: {e}"));
                }
            }
        }

        Err(last_err)
    }
}
