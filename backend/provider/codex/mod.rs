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
/// Banked rate-limit resets.
mod resets;
/// HTTP routes for Codex requests.
pub(crate) mod routes;
/// Client version sent to the `ChatGPT` backend.
pub(crate) mod version;

pub(crate) use auth::CodexAuthManager;
pub(crate) use client::CodexClient;

/// Codex provider integration for routing, discovery, quota, and dashboard login.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "codex",
        label: None,
        oauth: true,
        api_key: false,
        fixed_id: Some("codex"),
        fixed_name: Some("OpenAI Codex"),
        keyless: false,
        default_base_url: None,
    };
    fn router(
        settings: &crate::server::settings::SettingsStore,
    ) -> axum::Router<crate::server::AppState> {
        routes::router(settings)
    }

    async fn discover(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        models::fetch_catalog_pool(state, provider)
            .await
            .map(|response| response.data)
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

impl super::common::ResetProvider for Backend {
    async fn list_resets(
        store: &crate::server::settings::SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<super::resets::ResetCredits, String> {
        resets::list(store, provider, credential).await
    }

    async fn redeem_reset(
        store: &crate::server::settings::SettingsStore,
        provider: &str,
        credential: &str,
        reset: &str,
    ) -> Result<super::resets::RedeemResult, String> {
        resets::redeem(store, provider, credential, reset).await
    }
}

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> axum::Router {
        dashboard_login::router(state)
    }
}
