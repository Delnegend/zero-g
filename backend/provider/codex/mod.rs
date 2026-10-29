/// OAuth credentials and token management for Codex.
pub(crate) mod auth;
/// Codex upstream request and stream handling.
mod client;
/// Dashboard endpoints for the Codex OAuth flow.
pub(crate) mod dashboard_login;
/// Interactive Codex login flow.
pub(crate) mod login;
/// Codex model catalog discovery.
pub(crate) mod models;
/// Translation of requests into Codex payloads.
mod payload;
/// Codex account quota retrieval.
pub(crate) mod quota;
/// HTTP routes for Codex requests.
pub(crate) mod routes;

pub(crate) use auth::CodexAuthManager;
pub(crate) use client::CodexClient;

/// Codex provider integration for routing, discovery, quota, and dashboard login.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "codex",
        label: "Codex (OpenAI)",
        oauth: true,
        api_key: false,
        fixed_id: Some("codex"),
        fixed_name: Some("OpenAI Codex"),
        keyless: false,
        default_base_url: None,
        legacy: super::common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };
    fn router(
        settings: &crate::server::settings::SettingsStore,
    ) -> axum::Router<crate::server::AppState> {
        routes::router(settings)
    }

    async fn discover(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
    ) -> Result<Vec<serde_json::Value>, String> {
        models::fetch_catalog_pool(state, provider)
            .await
            .map(|(_, catalog)| catalog)
    }
}

impl super::common::QuotaProvider for Backend {
    async fn quota(
        store: &crate::server::settings::SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<super::quota::QuotaSnapshot, String> {
        quota::fetch(store, provider, credential).await
    }
}

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> axum::Router {
        dashboard_login::router(state)
    }
}
