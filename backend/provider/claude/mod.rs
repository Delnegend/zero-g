//! Claude Code subscription provider.
/// OAuth credential persistence and refresh.
mod auth;
/// Anthropic HTTP and SSE transport.
mod client;
/// Dashboard login API.
pub(crate) mod dashboard_login;
/// Account model discovery.
mod models;
/// `OpenAI` to Anthropic request translation.
mod payload;
/// Subscription quota discovery.
mod quota;
/// Public gateway routes.
mod routes;
/// Anthropic stream decoding.
mod stream;

/// Claude Code provider backend.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "claude",
        label: "Claude Code",
        oauth: true,
        api_key: false,
        fixed_id: Some("claude"),
        fixed_name: Some("Claude Code"),
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
    ) -> Result<Vec<crate::openai::Model>, String> {
        models::discover(state, provider).await
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
