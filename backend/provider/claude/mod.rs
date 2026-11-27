//! Claude Code subscription provider.
/// OAuth credential persistence and refresh.
mod auth;
/// Dashboard login API.
pub(crate) mod dashboard_login;
/// Account model discovery.
mod models;
pub(crate) use models::{catalog_info, catalog_reference};
/// Account subscription plan.
mod profile;
pub(crate) use profile::plan;
/// Anthropic Messages pass-through for Anthropic-format clients.
pub(crate) mod passthrough;
/// Subscription quota discovery.
mod quota;
/// Banked limit resets.
mod resets;
/// Anthropic HTTP transport and credential selection.
mod upstream;
/// Claude Code version used for Zero-G's own Anthropic calls.
pub(crate) mod version;

/// Claude Code provider backend.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "claude",
        label: None,
        oauth: true,
        api_key: false,
        fixed_id: Some("claude"),
        fixed_name: Some("Claude Code"),
        keyless: false,
        default_base_url: None,
    };
    fn router(
        _settings: &crate::server::settings::SettingsStore,
    ) -> axum::Router<crate::server::AppState> {
        // Claude is served only by the Anthropic pass-through at `/v1/messages`.
        axum::Router::new()
    }
    async fn discover(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        models::discover(state, provider, None).await
    }
}
impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> axum::Router {
        dashboard_login::router(state)
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
