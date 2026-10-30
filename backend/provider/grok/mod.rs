//! xAI OAuth and Grok Build CLI provider integrations.
/// OAuth token storage and refresh.
mod auth;
/// Dashboard device authorization endpoints.
pub(crate) mod dashboard_login;
/// Provider inference and discovery routes.
mod routes;

use crate::{
    openai::Model,
    provider::{
        common::{DashboardProvider, LegacyPolicy, Provider, ProviderDescriptor},
        registry,
    },
    server::{AppState, settings::SettingsStore},
};
use axum::Router;

/// xAI OAuth API billing pool.
pub(crate) struct Backend;
/// Grok Build CLI billing pool.
pub(crate) struct BuildBackend;

impl Provider for Backend {
    const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
        value: "grok",
        label: "Grok (xAI)",
        oauth: true,
        api_key: false,
        fixed_id: Some("grok"),
        fixed_name: Some("Grok"),
        keyless: false,
        default_base_url: Some("https://api.x.ai/v1"),
        legacy: LegacyPolicy {
            key: None,
            same_base: false,
        },
    };
    fn router(settings: &SettingsStore) -> Router<AppState> {
        routes::router(settings, "grok")
    }
    async fn discover(
        state: &AppState,
        provider: &registry::Provider,
    ) -> Result<Vec<Model>, String> {
        routes::discover(state, provider, "grok").await
    }
}
impl Provider for BuildBackend {
    const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
        value: "grok-build",
        label: "Grok Build",
        oauth: true,
        api_key: false,
        fixed_id: Some("grok-build"),
        fixed_name: Some("Grok Build"),
        keyless: false,
        default_base_url: Some("https://cli-chat-proxy.grok.com/v1"),
        legacy: LegacyPolicy {
            key: None,
            same_base: false,
        },
    };
    fn router(settings: &SettingsStore) -> Router<AppState> {
        routes::router(settings, "grok-build")
    }
    async fn discover(
        state: &AppState,
        provider: &registry::Provider,
    ) -> Result<Vec<Model>, String> {
        routes::discover(state, provider, "grok-build").await
    }
}
impl DashboardProvider for Backend {
    fn dashboard_router(state: AppState) -> Router {
        dashboard_login::router(state)
    }
}
