//! Cursor subscription provider.

/// Cursor OAuth token storage and refresh.
mod auth;
/// Dashboard PKCE authorization endpoints.
pub(crate) mod dashboard_login;
/// Client-facing inference endpoints and response handling.
mod downstream;
/// Account-scoped Cursor model discovery.
mod models;
/// Conversion of conversation history to Cursor records.
mod replay;
/// Cursor's interleaved agent RPC conversation.
mod upstream;

/// Cursor's agent wire protocol, generated from the vendored upstream schema.
#[allow(
    dead_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::missing_docs_in_private_items,
    missing_docs
)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/agent.v1.rs"));
}

/// Cursor backend integration.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "cursor",
        label: None,
        oauth: true,
        api_key: false,
        fixed_id: Some("cursor"),
        fixed_name: Some("Cursor"),
        keyless: false,
        default_base_url: None,
    };

    fn router(
        settings: &crate::server::settings::SettingsStore,
    ) -> axum::Router<crate::server::AppState> {
        downstream::router(settings)
    }

    async fn discover(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        models::fetch_catalog_pool(state, provider, None).await
    }
}

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> axum::Router {
        dashboard_login::router(state)
    }
}
