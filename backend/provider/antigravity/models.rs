use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::provider::antigravity::AntigravityClient;
use crate::provider::antigravity::auth::{ANTIGRAVITY_USER_AGENT, Credentials};
use crate::provider::common::errors::UpstreamError;

/// Sandbox Cloud Code Assist endpoint used as a fallback.
pub(super) const SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
/// Anthropic beta header value enabling interleaved thinking.
pub(super) const CLAUDE_THINKING_BETA_HEADER: &str = "interleaved-thinking-2025-05-14";

/// Model discovery response returned by Cloud Code Assist.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AntigravityDiscoveryResponse {
    /// Model metadata keyed by upstream model ID.
    #[serde(default)]
    models: HashMap<String, AntigravityModelInfo>,
}

/// Metadata for a discovered Antigravity model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AntigravityModelInfo {
    /// Human-readable model name.
    #[serde(default, rename = "displayName")]
    pub(crate) display_name: Option<String>,
    /// Whether the model supports thinking.
    #[serde(default, rename = "supportsThinking")]
    pub(crate) supports_thinking: Option<bool>,
    /// Maximum token count advertised by the model.
    #[serde(default, rename = "maxTokens")]
    pub(crate) max_tokens: Option<u64>,
    /// Maximum output token count.
    #[serde(default, rename = "maxOutputTokens")]
    pub(crate) max_output_tokens: Option<u32>,
    /// Whether the model is disabled.
    #[serde(default)]
    pub(crate) disabled: Option<bool>,
    /// Additional upstream model metadata.
    #[serde(flatten)]
    pub(crate) extra: HashMap<String, serde_json::Value>,
}

/// Derive advertised input types from Antigravity's model metadata.
pub(crate) fn input_modalities(extra: &serde_json::Map<String, Value>) -> Vec<String> {
    let (mut image_mime, mut audio, mut video_mime, mut pdf) = (false, false, false, false);
    if let Some(mimes) = extra.get("supportedMimeTypes").and_then(Value::as_object) {
        for (mime, enabled) in mimes {
            if enabled.as_bool() != Some(true) {
                continue;
            }
            image_mime |= mime.starts_with("image/");
            audio |= mime.starts_with("audio/");
            video_mime |= mime.starts_with("video/")
                && !mime.starts_with("video/videoframe/")
                && !mime.starts_with("video/audio/")
                && !mime.starts_with("video/text/")
                && mime != "video/jpeg2000";
            pdf |= mime == "application/pdf";
        }
    }
    let image = extra
        .get("supportsImages")
        .and_then(Value::as_bool)
        .unwrap_or(image_mime);
    let video = extra
        .get("supportsVideo")
        .and_then(Value::as_bool)
        .unwrap_or(video_mime);
    let mut result = vec!["text".to_owned()];
    for (enabled, modality) in [
        (image, "image"),
        (audio, "audio"),
        (video, "video"),
        (pdf, "pdf"),
    ] {
        if enabled {
            result.push(modality.to_owned());
        }
    }
    result
}

/// A family is recognized only when discovery supplies multiple sibling tiers.
/// A lone suffixed ID may be unrelated even if its base ID exists.
pub(crate) struct ModelFamily {
    /// Canonical family or standalone model ID.
    pub(crate) id: String,
    /// Discovered upstream IDs and their metadata.
    pub(crate) variants: Vec<(String, AntigravityModelInfo)>,
    /// Whether multiple reasoning tiers form this family.
    pub(crate) grouped: bool,
}

/// Split a recognized reasoning-tier suffix from a model ID.
fn tier(id: &str) -> Option<(&str, &str)> {
    [("-low", "low"), ("-medium", "medium"), ("-high", "high")]
        .into_iter()
        .find_map(|(suffix, effort)| {
            id.strip_suffix(suffix)
                .filter(|base| !base.is_empty())
                .map(|base| (base, effort))
        })
}

/// Group discovered sibling tiers into model families.
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
    /// Check whether a model or its grouped family is permitted.
    fn allowed(&self, id: &str, permissions: &crate::server::settings::Permissions) -> bool {
        permissions.allows_model(id) || (self.grouped && permissions.allows_model(&self.id))
    }

    /// Build a model listing entry from permitted variants, if any.
    pub(crate) fn entry(
        &self,
        permissions: &crate::server::settings::Permissions,
    ) -> Option<(crate::openai::Model, &AntigravityModelInfo)> {
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

    /// Resolve only discovered IDs, applying authorization to the actual upstream
    /// model as well as the family grant. The default is stable and accessible.
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

/// Failure to resolve an authorized upstream model.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// No selected model is authorized.
    Forbidden,
    /// The requested reasoning effort is unavailable.
    Unsupported,
}

impl AntigravityClient {
    /// Fetch enabled, user-facing models from the available upstream endpoints.
    pub(crate) async fn fetch_models(
        &self,
        creds: &Credentials,
    ) -> Result<Vec<(String, AntigravityModelInfo)>, UpstreamError> {
        let token = super::token_digest(&creds.access_token);
        {
            let mut cache = self.model_cache.lock().await;
            if let Some(entry) = cache.as_ref()
                && entry.token == token
                && entry.project == creds.project_id
                && entry.fetched.elapsed() < std::time::Duration::from_secs(300)
            {
                return Ok(entry.models.clone());
            }
            *cache = None;
        }
        #[cfg(test)]
        let endpoints = self.endpoints();
        #[cfg(not(test))]
        let endpoints = Self::endpoints();
        let mut last_err = UpstreamError::from("No upstream endpoint available".to_string());

        for endpoint in &endpoints {
            let url = format!("{endpoint}/v1internal:fetchAvailableModels");
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", creds.access_token))
                .header("Content-Type", "application/json")
                .header("User-Agent", ANTIGRAVITY_USER_AGENT)
                .json(&json!({"project": creds.project_id}))
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
                    *self.model_cache.lock().await = Some(super::ModelCache {
                        token,
                        project: creds.project_id.clone(),
                        fetched: std::time::Instant::now(),
                        models: models.clone(),
                    });
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
