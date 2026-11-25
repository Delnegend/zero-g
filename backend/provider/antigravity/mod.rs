//! Google Antigravity quota display. Zero-G never calls Google for this: the official `agy`
//! CLI reports its own quota and `zero-g-agy-integration` pushes it to `/v1/agy/quota`.

/// Pushed quota snapshots: parsing, storage and display.
pub(crate) mod quota;

/// Provider integration for Antigravity account quota.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "agy",
        label: None,
        oauth: false,
        api_key: false,
        fixed_id: Some("agy"),
        fixed_name: Some("Google Antigravity"),
        keyless: false,
        default_base_url: None,
    };
    fn router(
        _settings: &crate::server::settings::SettingsStore,
    ) -> axum::Router<crate::server::AppState> {
        axum::Router::new()
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn discover(
        _state: &crate::server::AppState,
        _provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        Ok(Vec::new())
    }
}

impl super::common::QuotaProvider for Backend {
    async fn quota(
        store: &crate::server::settings::SettingsStore,
        _provider: &str,
        credential: &str,
    ) -> Result<super::quota::QuotaSnapshot, String> {
        quota::stored(store, credential).await
    }
}
