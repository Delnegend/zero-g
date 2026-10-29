/// Shared upstream error classification.
pub(crate) mod errors;
/// Helpers for OpenAI-compatible proxy responses and permissions.
pub(crate) mod proxy;
/// Scoped identities for upstream session affinity.
pub(crate) mod session;

use crate::provider::{quota, registry};
use crate::server::{AppState, settings::SettingsStore};
use axum::Router;
use serde_json::Value;

/// Operations implemented by each provider backend.
pub(crate) trait Provider {
    /// Static identity and capabilities of this backend.
    const DESCRIPTOR: ProviderDescriptor;
    /// Builds the provider's request router.
    fn router(settings: &SettingsStore) -> Router<AppState>;
    /// Discovers models for a configured provider.
    async fn discover(
        state: &AppState,
        provider: &registry::Provider,
    ) -> Result<Vec<Value>, String>;
}

/// Provider backend capable of reporting credential quota.
pub(crate) trait QuotaProvider {
    /// Fetches quota for the named provider and credential.
    async fn quota(
        store: &SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<quota::QuotaSnapshot, String>;
}

/// Provider backend exposing dashboard-specific routes.
pub(crate) trait DashboardProvider {
    /// Builds dashboard routes bound to application state.
    fn dashboard_router(state: AppState) -> Router;
}

/// Provider identity, authentication options, and migration policy.
#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct ProviderDescriptor {
    /// Stable provider kind identifier.
    pub(crate) value: &'static str,
    /// Display name for the provider kind.
    pub(crate) label: &'static str,
    /// Whether OAuth credentials are supported.
    pub(crate) oauth: bool,
    /// Whether API key credentials are supported.
    pub(crate) api_key: bool,
    /// Fixed provider instance identifier, when applicable.
    pub(crate) fixed_id: Option<&'static str>,
    /// Fixed provider instance name, when applicable.
    pub(crate) fixed_name: Option<&'static str>,
    /// Whether requests can be sent without credentials.
    #[serde(skip)]
    pub(crate) keyless: bool,
    /// Default upstream base URL, if supplied by the backend.
    #[serde(skip)]
    pub(crate) default_base_url: Option<&'static str>,
    /// Rules for matching legacy configuration.
    #[serde(skip)]
    pub(crate) legacy: LegacyPolicy,
}

/// Rules for associating legacy settings with a provider.
#[derive(Clone, Copy)]
pub(crate) struct LegacyPolicy {
    /// Legacy settings key, if one exists.
    pub(crate) key: Option<&'static str>,
    /// Whether matching requires the same base URL.
    pub(crate) same_base: bool,
}
