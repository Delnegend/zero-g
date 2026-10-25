use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::antigravity::AntigravityClient;
use crate::auth::{ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials};

pub(crate) const SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
pub(crate) const CLAUDE_THINKING_BETA_HEADER: &str = "interleaved-thinking-2025-05-14";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AntigravityDiscoveryResponse {
    #[serde(default)]
    pub(crate) models: HashMap<String, AntigravityModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AntigravityModelInfo {
    #[serde(default, rename = "displayName")]
    pub(crate) display_name: Option<String>,
    #[serde(default, rename = "supportsImages")]
    pub(crate) supports_images: Option<bool>,
    #[serde(default, rename = "supportsThinking")]
    pub(crate) supports_thinking: Option<bool>,
    #[serde(default, rename = "maxTokens")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(default, rename = "maxOutputTokens")]
    pub(crate) max_output_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) disabled: Option<bool>,
}

impl AntigravityClient {
    pub(crate) async fn fetch_models(
        &self,
        creds: &Credentials,
    ) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
        let endpoints = [CLOUD_CODE_ASSIST_ENDPOINT, SANDBOX_ENDPOINT];
        let mut last_err = String::new();

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
                    let disc: AntigravityDiscoveryResponse = r
                        .json()
                        .await
                        .map_err(|e| format!("Failed to parse models response: {e}"))?;
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
                    last_err = format!("Endpoint {endpoint} returned {status}: {body}");
                }
                Err(e) => {
                    last_err = format!("Endpoint {endpoint} failed: {e}");
                }
            }
        }

        Err(last_err)
    }

    pub(crate) fn map_model_id(input: &str) -> String {
        let trimmed = input.trim();
        match trimmed {
            // Claude aliases
            "claude-3-5-sonnet" | "claude-sonnet" | "claude-sonnet-5-5" | "claude" => {
                "claude-sonnet-5-5-medium".to_string()
            }
            "claude-3-5-sonnet-high" | "claude-sonnet-high" => "claude-sonnet-5-5-high".to_string(),
            "claude-3-5-sonnet-low" | "claude-sonnet-low" => "claude-sonnet-5-5-low".to_string(),
            "claude-opus" | "claude-opus-5-5" => "claude-opus-5-5-medium".to_string(),
            "claude-opus-high" => "claude-opus-5-5-high".to_string(),
            "claude-opus-low" => "claude-opus-5-5-low".to_string(),

            // Gemini aliases
            "gemini-flash" | "gemini-3.8-flash" | "gemini-flash-low" => {
                "gemini-3.8-flash-low".to_string()
            }
            "gemini-3.8-flash-medium" | "gemini-flash-medium" => {
                "gemini-3.8-flash-medium".to_string()
            }
            "gemini-3.8-flash-high" | "gemini-flash-high" => "gemini-3.8-flash-high".to_string(),
            "gemini-4" | "gemini-4-argon" | "gemini-pro" | "gemini-argon" => {
                "gemini-4-argon-low".to_string()
            }
            "gemini-4-argon-medium" => "gemini-4-argon-medium".to_string(),
            "gemini-4-argon-high" => "gemini-4-argon-high".to_string(),

            // GPT-OSS
            "gpt-oss" | "gpt-oss-120b" => "gpt-oss-120b-medium".to_string(),

            other => other.to_string(),
        }
    }
}

pub(crate) const MODEL_ALIASES: &[&str] = &[
    "claude-3-5-sonnet",
    "claude-sonnet",
    "claude-opus",
    "gemini-flash",
    "gemini-pro",
    "gpt-oss",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_aliases() {
        assert_eq!(
            AntigravityClient::map_model_id("claude-3-5-sonnet"),
            "claude-sonnet-5-5-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("claude-sonnet"),
            "claude-sonnet-5-5-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("claude-opus"),
            "claude-opus-5-5-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("gemini-flash"),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            AntigravityClient::map_model_id("gemini-pro"),
            "gemini-4-argon-low"
        );
        assert_eq!(
            AntigravityClient::map_model_id("gpt-oss"),
            "gpt-oss-120b-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("unknown-model"),
            "unknown-model"
        );
    }
}
