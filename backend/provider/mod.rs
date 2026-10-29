/// Antigravity provider integration.
pub(crate) mod antigravity;
/// Model catalog aggregation.
pub(crate) mod catalog;
/// Codex provider integration.
pub(crate) mod codex;
/// Shared provider contracts and helpers.
pub(crate) mod common;
/// Provider credential management.
pub(crate) mod credentials;
/// Go provider integration.
pub(crate) mod go;
/// Provider login coordination.
pub(crate) mod login;
/// OpenAI-compatible provider integration.
pub(crate) mod openai;
/// Quota aggregation and responses.
pub(crate) mod quota;
/// Configured provider registry.
pub(crate) mod registry;
/// Zen provider integration.
pub(crate) mod zen;

use self::common::{DashboardProvider, Provider, ProviderDescriptor, QuotaProvider};
use crate::server::{AppState, settings::SettingsStore};
use axum::Router;
use serde_json::Value;
use std::sync::Arc;

/// Shared clients for providers that maintain client state.
#[derive(Clone)]
pub(crate) struct Providers {
    /// Antigravity API client.
    pub(crate) client: Arc<antigravity::AntigravityClient>,
    /// Codex API client.
    pub(crate) codex_client: Arc<codex::CodexClient>,
}

impl Providers {
    /// Creates the shared provider clients.
    pub(crate) fn new() -> Self {
        Self {
            client: Arc::new(antigravity::AntigravityClient::new()),
            codex_client: Arc::new(codex::CodexClient::new()),
        }
    }
}

/// Descriptors for every supported provider kind.
pub(crate) const PROVIDER_KINDS: &[ProviderDescriptor] = &[
    antigravity::Backend::DESCRIPTOR,
    codex::Backend::DESCRIPTOR,
    zen::Backend::DESCRIPTOR,
    go::Backend::DESCRIPTOR,
    openai::Backend::DESCRIPTOR,
];

/// Supported provider backend kinds.
#[derive(Clone, Copy)]
pub(crate) enum ProviderKind {
    /// Google Antigravity backend.
    Antigravity,
    /// `OpenAI` Codex backend.
    Codex,
    /// Zen backend.
    Zen,
    /// Go backend.
    Go,
    /// Generic OpenAI-compatible backend.
    Openai,
}

/// Provider kinds in registration order.
const ALL: [ProviderKind; 5] = [
    ProviderKind::Antigravity,
    ProviderKind::Codex,
    ProviderKind::Zen,
    ProviderKind::Go,
    ProviderKind::Openai,
];

impl ProviderKind {
    /// Looks up a provider kind by its stable identifier.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        ALL.into_iter().find(|kind| kind.as_str() == value)
    }
    /// Returns this backend's static descriptor.
    pub(crate) const fn descriptor(self) -> ProviderDescriptor {
        match self {
            Self::Antigravity => antigravity::Backend::DESCRIPTOR,
            Self::Codex => codex::Backend::DESCRIPTOR,
            Self::Zen => zen::Backend::DESCRIPTOR,
            Self::Go => go::Backend::DESCRIPTOR,
            Self::Openai => openai::Backend::DESCRIPTOR,
        }
    }
    /// Returns the stable provider kind identifier.
    pub(crate) const fn as_str(self) -> &'static str {
        self.descriptor().value
    }
    /// Returns whether OAuth credentials are supported.
    pub(crate) const fn oauth(self) -> bool {
        self.descriptor().oauth
    }
    /// Returns whether API key credentials are supported.
    pub(crate) const fn api_key(self) -> bool {
        self.descriptor().api_key
    }
    /// Returns whether credentials are optional.
    pub(crate) const fn keyless(self) -> bool {
        self.descriptor().keyless
    }
    /// Returns the default upstream URL, if any.
    pub(crate) const fn default_base_url(self) -> Option<&'static str> {
        self.descriptor().default_base_url
    }

    /// Discovers models using the selected backend.
    pub(crate) async fn discover(
        self,
        state: &AppState,
        provider: &registry::Provider,
    ) -> Result<Vec<Value>, String> {
        match self {
            Self::Antigravity => antigravity::Backend::discover(state, provider).await,
            Self::Codex => codex::Backend::discover(state, provider).await,
            Self::Zen => zen::Backend::discover(state, provider).await,
            Self::Go => go::Backend::discover(state, provider).await,
            Self::Openai => openai::Backend::discover(state, provider).await,
        }
    }

    /// Returns the backend's quota capability, if supported.
    pub(crate) const fn quota(self) -> Option<QuotaCapability> {
        match self {
            Self::Antigravity => Some(QuotaCapability::Antigravity),
            Self::Codex => Some(QuotaCapability::Codex),
            Self::Zen => Some(QuotaCapability::Zen),
            Self::Go => Some(QuotaCapability::Go),
            Self::Openai => None,
        }
    }

    /// Builds the request router for this backend.
    fn router(self, settings: &SettingsStore) -> Router<AppState> {
        match self {
            Self::Antigravity => antigravity::Backend::router(settings),
            Self::Codex => codex::Backend::router(settings),
            Self::Zen => zen::Backend::router(settings),
            Self::Go => go::Backend::router(settings),
            Self::Openai => openai::Backend::router(settings),
        }
    }
}

/// Backends that can retrieve credential quota.
pub(crate) enum QuotaCapability {
    /// Antigravity quota.
    Antigravity,
    /// Codex quota.
    Codex,
    /// Zen quota.
    Zen,
    /// Go quota.
    Go,
}
impl QuotaCapability {
    /// Fetches quota for a configured provider credential.
    pub(crate) async fn fetch(
        self,
        store: &SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<quota::QuotaSnapshot, String> {
        match self {
            Self::Antigravity => antigravity::Backend::quota(store, provider, credential).await,
            Self::Codex => codex::Backend::quota(store, provider, credential).await,
            Self::Zen => zen::Backend::quota(store, provider, credential).await,
            Self::Go => go::Backend::quota(store, provider, credential).await,
        }
    }
}

/// Nests provider routers under their kind and instance identifier.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    ALL.into_iter().fold(Router::new(), |router, kind| {
        router.nest(&format!("/{}/{{id}}", kind.as_str()), kind.router(settings))
    })
}

/// Checks whether the first path segment names a supported provider kind.
pub(crate) fn is_provider_path(path: &str) -> bool {
    path.strip_prefix('/')
        .and_then(|path| path.split('/').next())
        .and_then(ProviderKind::from_str)
        .is_some()
}

/// Combines dashboard routes exposed by provider backends.
pub(crate) fn dashboard_router(state: &AppState) -> Router {
    ALL.into_iter()
        .fold(Router::new(), |router, kind| match kind {
            ProviderKind::Antigravity => {
                router.merge(antigravity::Backend::dashboard_router(state.clone()))
            }
            ProviderKind::Codex => router.merge(codex::Backend::dashboard_router(state.clone())),
            ProviderKind::Zen => router.merge(zen::Backend::dashboard_router(state.clone())),
            ProviderKind::Go | ProviderKind::Openai => router,
        })
}

/// Starts the Zen shim when its configuration requires it.
pub(crate) async fn start() -> Option<tokio::process::Child> {
    zen::start().await
}

/// Provider registration and routing checks.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Descriptors agree with kind capabilities and path matching.
    #[test]
    fn registered_capabilities_match_provider_kinds() {
        for descriptor in PROVIDER_KINDS {
            let kind = ProviderKind::from_str(descriptor.value).unwrap();
            assert_eq!(kind.as_str(), descriptor.value);
            assert_eq!(kind.oauth(), descriptor.oauth);
            assert_eq!(kind.api_key(), descriptor.api_key);
            assert_eq!(kind.keyless(), descriptor.keyless);
        }
        assert!(ProviderKind::from_str("unknown").is_none());
        assert!(ProviderKind::Openai.quota().is_none());
        assert!(ProviderKind::Antigravity.quota().is_some());
        assert!(ProviderKind::Codex.quota().is_some());
        assert!(ProviderKind::Zen.quota().is_some());
        assert!(ProviderKind::Go.quota().is_some());
        assert!(is_provider_path("/openai/example/v1/models"));
        assert!(!is_provider_path("/openaix/example"));
    }
}
