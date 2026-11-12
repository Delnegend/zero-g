//! ChatJimmy inference provider integration.

use axum::Router;
use axum::routing::{get, post};
use std::sync::LazyLock;
use std::time::Duration;

use crate::openai::Model;
use crate::provider::common::{LegacyPolicy, Provider, ProviderDescriptor};
use crate::provider::registry;
use crate::server::{AppState, settings::SettingsStore};

/// Model discovery and catalog metadata.
mod models;
/// Upstream request and response representations.
mod protocol;
/// HTTP handlers translating chat requests.
mod routes;
/// Streaming response chunk construction.
mod stream;
/// Provider behavior tests.
#[cfg(test)]
mod tests;

pub(crate) use models::catalog_info;
use models::discover_models;
use routes::{chat_handler, models_handler};

/// Default `chatjimmy.ai` base URL.
pub(crate) const DEFAULT_BASE: &str = "https://chatjimmy.ai";
/// Upstream chat endpoint path.
const CHAT_PATH: &str = "/api/chat";
/// Upstream models endpoint path.
const MODELS_PATH: &str = "/api/models";
/// Default single model supported by `ChatJimmy` hardware.
const DEFAULT_MODEL: &str = "llama3.1-8B";

/// Shared client for `ChatJimmy` requests with keepalive and connection pooling.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
});

/// `ChatJimmy` backend.
pub(crate) struct Backend;

impl Provider for Backend {
    const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
        value: "chatjimmy",
        label: None,
        oauth: false,
        api_key: true,
        fixed_id: Some("chatjimmy"),
        fixed_name: Some("ChatJimmy"),
        keyless: true,
        default_base_url: Some(DEFAULT_BASE),
        legacy: LegacyPolicy {
            key: None,
            same_base: false,
        },
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        Router::new()
            .route("/v1/models", get(models_handler))
            .route("/v1/models/", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .route("/v1/chat/completions/", post(chat_handler))
            .layer(axum::middleware::from_fn_with_state(
                (settings.clone(), "chatjimmy"),
                crate::server::settings::virtual_key_auth,
            ))
    }

    async fn discover(
        _state: &AppState,
        _provider: &registry::Provider,
    ) -> Result<Vec<Model>, String> {
        discover_models().await
    }
}
