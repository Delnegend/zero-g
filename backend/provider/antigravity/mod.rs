//! Google Antigravity quota display. Zero-G never calls Google for this: the official `agy`
//! CLI reports its own quota and model list, and `zero-g-agy-integration` pushes them to
//! `/v1/agy/quota`.

/// Models the CLI lists, shown in the catalog for reference.
pub(crate) mod models;
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

    async fn discover(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        let credentials = crate::storage::ProviderCredential::filter_by_provider_id(&provider.id)
            .exec(&mut (*state.database.db).clone())
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|credential| credential.id)
            .collect::<Vec<_>>();
        Ok(models::catalog(&state.settings, &credentials).await)
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
