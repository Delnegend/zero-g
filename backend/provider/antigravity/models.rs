use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provider::antigravity::AntigravityClient;
use crate::provider::antigravity::auth::{
    ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials,
};
use crate::provider::common::errors::UpstreamError;

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

pub(crate) struct ModelFamily {
    pub(crate) id: String,
    pub(crate) variants: Vec<(String, AntigravityModelInfo)>,
    pub(crate) grouped: bool,
}

fn tier(id: &str) -> Option<(&str, &str)> {
    [("-low", "low"), ("-medium", "medium"), ("-high", "high")]
        .into_iter()
        .find_map(|(suffix, effort)| {
            id.strip_suffix(suffix)
                .filter(|base| !base.is_empty())
                .map(|base| (base, effort))
        })
}

pub(crate) fn normalize_models(
    mut models: Vec<(String, AntigravityModelInfo)>,
) -> Vec<ModelFamily> {
    models.sort_by(|a, b| a.0.cmp(&b.0));
    let mut counts = std::collections::HashMap::new();
    for (id, _) in &models {
        if let Some((base, _)) = tier(id) {
            let count = counts.entry(base).or_insert(0usize);
            *count = count.saturating_add(1);
        }
    }
    let grouped: std::collections::HashSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(base, _)| base.to_owned())
        .collect();
    let mut families = std::collections::BTreeMap::<String, ModelFamily>::new();
    for (id, info) in models {
        let base = if grouped.contains(&id) {
            id.clone()
        } else {
            tier(&id)
                .map(|(base, _)| base)
                .filter(|base| grouped.contains(*base))
                .unwrap_or(&id)
                .to_owned()
        };
        let is_grouped = grouped.contains(&base);
        families
            .entry(base.clone())
            .or_insert_with(|| ModelFamily {
                id: base,
                variants: Vec::new(),
                grouped: is_grouped,
            })
            .variants
            .push((id, info));
    }
    families.into_values().collect()
}

impl ModelFamily {
    fn allowed(&self, id: &str, permissions: &crate::server::settings::Permissions) -> bool {
        permissions.allows_model(id) || (self.grouped && permissions.allows_model(&self.id))
    }

    pub(crate) fn entry(
        &self,
        permissions: &crate::server::settings::Permissions,
    ) -> Option<(crate::openai::ModelEntry, &AntigravityModelInfo)> {
        let available: Vec<_> = self
            .variants
            .iter()
            .filter(|(id, _)| self.allowed(id, permissions))
            .collect();
        let (_, info) = available
            .iter()
            .find(|(id, _)| id == &self.id)
            .or_else(|| {
                ["medium", "low", "high"].iter().find_map(|effort| {
                    available
                        .iter()
                        .find(|(id, _)| tier(id).is_some_and(|(_, e)| e == *effort))
                })
            })
            .or_else(|| available.first())?;
        let efforts: Vec<String> = ["low", "medium", "high"]
            .iter()
            .filter(|effort| {
                available
                    .iter()
                    .any(|(id, _)| tier(id).is_some_and(|(_, e)| e == **effort))
            })
            .map(|effort| (*effort).to_owned())
            .collect();
        let mut entry =
            crate::provider::antigravity::routes::to_model_entry(self.id.clone(), (*info).clone());
        if self.grouped {
            for name in [&mut entry.name, &mut entry.display_name] {
                if let Some(value) = name
                    && let Some(base) = [" (Low)", " (Medium)", " (High)"]
                        .iter()
                        .find_map(|suffix| value.strip_suffix(suffix))
                {
                    *value = base.to_owned();
                }
            }
            entry.reasoning = Some(true);
            entry.supported_reasoning_efforts = Some(efforts);
            entry.default_reasoning_effort =
                tier(&self.resolve(None, permissions).ok()?).map(|(_, e)| e.to_owned());
        }
        Some((entry, info))
    }

    pub(crate) fn resolve(
        &self,
        effort: Option<&str>,
        permissions: &crate::server::settings::Permissions,
    ) -> Result<String, ResolveError> {
        let selected = if !self.grouped {
            self.variants.first()
        } else if let Some(effort) = effort {
            let canonical = match effort.to_ascii_lowercase().as_str() {
                "low" | "minimal" => "low",
                "medium" => "medium",
                "high" | "xhigh" | "max" | "ultra" => "high",
                _ => return Err(ResolveError::Unsupported),
            };
            self.variants
                .iter()
                .find(|(id, _)| tier(id).is_some_and(|(_, e)| e == canonical))
        } else {
            self.variants
                .iter()
                .find(|(id, _)| id == &self.id && self.allowed(id, permissions))
                .or_else(|| {
                    ["medium", "low", "high"].iter().find_map(|effort| {
                        self.variants.iter().find(|(id, _)| {
                            tier(id).is_some_and(|(_, e)| e == *effort)
                                && self.allowed(id, permissions)
                        })
                    })
                })
        };
        let (id, _) = selected.ok_or_else(|| {
            if effort.is_none() {
                ResolveError::Forbidden
            } else {
                ResolveError::Unsupported
            }
        })?;
        if !self.allowed(id, permissions) {
            return Err(ResolveError::Forbidden);
        }
        Ok(id.clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    Forbidden,
    Unsupported,
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
