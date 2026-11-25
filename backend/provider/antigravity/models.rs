//! The models the official `agy` CLI lists, kept so the catalog page can show them.
//!
//! They are for reference only: Zero-G does not route Antigravity requests, and the CLI reports
//! just an id and a display name, so nothing else about a model is known.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::openai::Model;
use crate::server::settings::SettingsStore;

/// Most models accepted in one report.
pub(crate) const MAX_MODELS: usize = 500;

/// One model as `agy models` prints it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ModelEntry {
    /// Model id, such as `gemini-4-neon-high`.
    pub(crate) id: String,
    /// Display name, such as `Gemini 4 Neon (High)`.
    pub(crate) name: String,
}

impl ModelEntry {
    /// Whether both fields are short, printable text.
    pub(crate) fn is_valid(&self) -> bool {
        [&self.id, &self.name].iter().all(|text| {
            !text.trim().is_empty() && text.len() <= 200 && !text.contains(char::is_control)
        })
    }

    /// The catalog record for this model.
    fn to_model(&self) -> Option<Model> {
        serde_json::from_value(json!({
            "id": self.id,
            "object": "model",
            "owned_by": "antigravity",
            "display_name": self.name,
        }))
        .ok()
    }
}

/// Setting that holds a credential's model list.
pub(crate) fn setting_name(credential_id: &str) -> String {
    format!("agy_models.{credential_id}")
}

/// A credential's stored model list; empty when none was reported or it cannot be read.
async fn stored(store: &SettingsStore, credential: &str) -> Vec<ModelEntry> {
    store
        .setting_get(&setting_name(credential))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// The models reported by any of the given credentials, each id once, in report order.
pub(crate) async fn catalog(store: &SettingsStore, credentials: &[String]) -> Vec<Model> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for credential in credentials {
        for entry in stored(store, credential).await {
            if seen.insert(entry.id.clone())
                && let Some(model) = entry.to_model()
            {
                models.push(model);
            }
        }
    }
    models
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::ModelEntry;

    #[test]
    fn entries_must_be_short_printable_text() {
        let entry = |id: &str, name: &str| ModelEntry {
            id: id.into(),
            name: name.into(),
        };
        assert!(entry("gemini-4-neon-high", "Gemini 4 Neon (High)").is_valid());
        assert!(!entry("", "Name").is_valid());
        assert!(!entry("id", "  ").is_valid());
        assert!(!entry("id", "bad\nname").is_valid());
        assert!(!entry(&"x".repeat(201), "Name").is_valid());
        let model = entry("m", "M").to_model().unwrap();
        assert_eq!(model.display_name.as_deref(), Some("M"));
        assert_eq!(model.id, "m");
    }
}
