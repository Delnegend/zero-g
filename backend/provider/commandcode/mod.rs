/// Go plan access through the CLI's generate endpoint.
pub(crate) mod go;

use axum::Router;
use axum::http::{HeaderMap, Method};
use serde_json::Value;

use crate::provider::{common, openai, registry::Provider};
use crate::server::{AppState, settings::SettingsStore};

/// Command Code's fixed Provider API endpoint.
pub(crate) const BASE_URL: &str = "https://api.commandcode.ai/provider/v1";

/// Command Code Provider API backend.
pub(crate) struct Backend;

/// Whether the gateway's OpenAI-compatible routes can serve this model.
///
/// Claude models answer only on Anthropic's `/messages`, which the gateway does not expose.
fn openai_compatible(entry: &Value) -> bool {
    entry
        .get("supported_endpoints")
        .and_then(Value::as_array)
        .is_none_or(|endpoints| {
            endpoints.iter().any(|endpoint| {
                matches!(endpoint.as_str(), Some("/chat/completions" | "/responses"))
            })
        })
}

impl openai::ModelEndpoint for Backend {
    async fn request(_provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str> {
        openai::send(
            BASE_URL,
            key,
            "models",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }

    fn models_from_list(value: &Value) -> Vec<crate::openai::Model> {
        value
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entry| openai_compatible(entry))
            .filter_map(|entry| {
                let mut model =
                    serde_json::from_value::<crate::openai::Model>(entry.clone()).ok()?;
                if model.context_window.is_none() {
                    model.context_window = entry
                        .get("context_length")
                        .and_then(Value::as_u64)
                        .filter(|&len| len > 0);
                }
                Some(model)
            })
            .filter(|model| !model.id.is_empty())
            .collect()
    }
}

impl common::Provider for Backend {
    const DESCRIPTOR: common::ProviderDescriptor = common::ProviderDescriptor {
        value: "commandcode",
        label: None,
        oauth: false,
        api_key: true,
        fixed_id: Some("commandcode"),
        fixed_name: Some("Command Code"),
        keyless: false,
        default_base_url: Some(BASE_URL),
        legacy: common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        openai::routes::router_for(settings, "commandcode", Some(BASE_URL))
    }

    async fn discover(
        state: &AppState,
        provider: &Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        openai::discover::<Self>(&state.settings, provider).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::provider::openai::ModelEndpoint;
    use serde_json::json;

    /// Messages-only models are hidden because no gateway route can reach them.
    #[test]
    fn discovery_lists_only_openai_compatible_models() {
        let models = Backend::models_from_list(&json!({"data": [
            {"id": "claude-sonnet-4-6", "context_length": 1_000_000,
             "supported_endpoints": ["/messages"]},
            {"id": "deepseek/deepseek-v4-flash", "context_length": 1_000_000,
             "supported_endpoints": ["/chat/completions", "/responses"]},
            {"id": "z-ai/glm-5.3-flash", "supported_endpoints": ["/chat/completions"]},
            {"id": "unlabelled", "context_length": 0}
        ]}));
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "deepseek/deepseek-v4-flash",
                "z-ai/glm-5.3-flash",
                "unlabelled"
            ]
        );
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[1].context_window, None);
        assert_eq!(models[2].context_window, None);
    }
}
