/// Antigravity provider integration.
pub(crate) mod antigravity;
/// Model catalog aggregation.
pub(crate) mod catalog;
/// Claude Code provider integration.
pub(crate) mod claude;
/// Codex provider integration.
pub(crate) mod codex;
/// Shared provider contracts and helpers.
pub(crate) mod common;
/// Provider credential management.
pub(crate) mod credentials;
/// Cursor provider integration.
pub(crate) mod cursor;
/// Go provider integration.
pub(crate) mod go;
/// Grok OAuth and Grok Build integrations.
pub(crate) mod grok;
/// Provider login coordination.
pub(crate) mod login;
/// OpenAI-compatible provider integration.
pub(crate) mod openai;
/// `OpenRouter` provider integration.
pub(crate) mod openrouter;
/// Quota aggregation and responses.
pub(crate) mod quota;
/// Configured provider registry.
pub(crate) mod registry;
/// Zen provider integration.
pub(crate) mod zen;

use self::common::{DashboardProvider, Provider, ProviderDescriptor, QuotaProvider};
use crate::openai::Model;
use crate::server::{AppState, settings::SettingsStore};
use axum::Router;
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

/// Defines the static provider registry and its descriptor mapping together.
macro_rules! providers {
    ($( $(#[$meta:meta])* $variant:ident => $backend:path ),+ $(,)?) => {
        /// Supported provider backend kinds.
        #[derive(Clone, Copy)]
        pub(crate) enum ProviderKind {
            $( $(#[$meta])* $variant, )+
        }

        /// Provider kinds in registration order.
        const ALL: &[ProviderKind] = &[ $( ProviderKind::$variant, )+ ];

        /// Descriptors for every supported provider kind.
        pub(crate) const PROVIDER_KINDS: &[ProviderDescriptor] = &[
            $( <$backend as Provider>::DESCRIPTOR, )+
        ];

        impl ProviderKind {
            /// Returns this backend's static descriptor.
            pub(crate) const fn descriptor(self) -> ProviderDescriptor {
                match self {
                    $( Self::$variant => <$backend as Provider>::DESCRIPTOR, )+
                }
            }
        }
    };
}

providers! {
    /// Google Antigravity backend.
    Antigravity => antigravity::Backend,
    /// `OpenAI` Codex backend.
    Codex => codex::Backend,
    /// Claude Code backend.
    Claude => claude::Backend,
    /// Cursor backend.
    Cursor => cursor::Backend,
    /// xAI OAuth backend.
    Grok => grok::Backend,
    /// Grok Build backend.
    GrokBuild => grok::BuildBackend,
    /// Zen backend.
    Zen => zen::Backend,
    /// Go backend.
    Go => go::Backend,
    /// Generic OpenAI-compatible backend.
    Openai => openai::Backend,
    /// `OpenRouter` backend.
    Openrouter => openrouter::Backend,
}

impl ProviderKind {
    /// Looks up a provider kind by its stable identifier.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        ALL.iter().copied().find(|kind| kind.as_str() == value)
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
    ) -> Result<Vec<Model>, String> {
        match self {
            Self::Antigravity => antigravity::Backend::discover(state, provider).await,
            Self::Codex => codex::Backend::discover(state, provider).await,
            Self::Claude => claude::Backend::discover(state, provider).await,
            Self::Cursor => cursor::Backend::discover(state, provider).await,
            Self::Grok => grok::Backend::discover(state, provider).await,
            Self::GrokBuild => grok::BuildBackend::discover(state, provider).await,
            Self::Zen => zen::Backend::discover(state, provider).await,
            Self::Go => go::Backend::discover(state, provider).await,
            Self::Openai => openai::Backend::discover(state, provider).await,
            Self::Openrouter => openrouter::Backend::discover(state, provider).await,
        }
    }

    /// Discovers only models allowed by the virtual key using backend-specific policy.
    pub(crate) async fn discover_permitted(
        self,
        state: &AppState,
        provider: &registry::Provider,
        permissions: &crate::server::settings::Permissions,
    ) -> Result<Vec<Model>, String> {
        match self {
            Self::Antigravity => {
                antigravity::Backend::discover_permitted(state, provider, permissions).await
            }
            Self::Codex => codex::Backend::discover_permitted(state, provider, permissions).await,
            Self::Claude => claude::Backend::discover_permitted(state, provider, permissions).await,
            Self::Cursor => cursor::Backend::discover_permitted(state, provider, permissions).await,
            Self::Grok => grok::Backend::discover_permitted(state, provider, permissions).await,
            Self::GrokBuild => {
                grok::BuildBackend::discover_permitted(state, provider, permissions).await
            }
            Self::Zen => zen::Backend::discover_permitted(state, provider, permissions).await,
            Self::Go => go::Backend::discover_permitted(state, provider, permissions).await,
            Self::Openai => openai::Backend::discover_permitted(state, provider, permissions).await,
            Self::Openrouter => {
                openrouter::Backend::discover_permitted(state, provider, permissions).await
            }
        }
    }

    /// Returns the backend's quota capability, if supported.
    pub(crate) const fn quota(self) -> Option<QuotaCapability> {
        match self {
            Self::Antigravity => Some(QuotaCapability::Antigravity),
            Self::Codex => Some(QuotaCapability::Codex),
            Self::Zen => Some(QuotaCapability::Zen),
            Self::Go => Some(QuotaCapability::Go),
            Self::Claude => Some(QuotaCapability::Claude),
            Self::Openrouter => Some(QuotaCapability::Openrouter),
            Self::Openai | Self::Cursor | Self::Grok | Self::GrokBuild => None,
        }
    }

    /// Builds the request router for this backend.
    fn router(self, settings: &SettingsStore) -> Router<AppState> {
        match self {
            Self::Antigravity => antigravity::Backend::router(settings),
            Self::Codex => codex::Backend::router(settings),
            Self::Claude => claude::Backend::router(settings),
            Self::Cursor => cursor::Backend::router(settings),
            Self::Grok => grok::Backend::router(settings),
            Self::GrokBuild => grok::BuildBackend::router(settings),
            Self::Zen => zen::Backend::router(settings),
            Self::Go => go::Backend::router(settings),
            Self::Openai => openai::Backend::router(settings),
            Self::Openrouter => openrouter::Backend::router(settings),
        }
    }
}

/// Backends that can retrieve credential quota.
pub(crate) enum QuotaCapability {
    /// Antigravity quota.
    Antigravity,
    /// Codex quota.
    Codex,
    /// Claude subscription quota.
    Claude,
    /// Zen quota.
    Zen,
    /// Go quota.
    Go,
    /// `OpenRouter` account balance.
    Openrouter,
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
            Self::Claude => claude::Backend::quota(store, provider, credential).await,
            Self::Zen => zen::Backend::quota(store, provider, credential).await,
            Self::Go => go::Backend::quota(store, provider, credential).await,
            Self::Openrouter => openrouter::Backend::quota(store, provider, credential).await,
        }
    }
}

/// Nests provider routers under their kind and instance identifier.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    ALL.iter().copied().fold(Router::new(), |router, kind| {
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
    ALL.iter()
        .copied()
        .fold(Router::new(), |router, kind| match kind {
            ProviderKind::Antigravity => {
                router.merge(antigravity::Backend::dashboard_router(state.clone()))
            }
            ProviderKind::Codex => router.merge(codex::Backend::dashboard_router(state.clone())),
            ProviderKind::Claude => router.merge(claude::Backend::dashboard_router(state.clone())),
            ProviderKind::Cursor => router.merge(cursor::Backend::dashboard_router(state.clone())),
            ProviderKind::Grok => router.merge(grok::Backend::dashboard_router(state.clone())),
            ProviderKind::Zen => router.merge(zen::Backend::dashboard_router(state.clone())),
            ProviderKind::Openrouter => {
                router.merge(openrouter::Backend::dashboard_router(state.clone()))
            }
            ProviderKind::Go | ProviderKind::Openai | ProviderKind::GrokBuild => router,
        })
}

/// Provider registration and routing checks.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn provider_kind_listing_preserves_registration_order() {
        let values: Vec<_> = PROVIDER_KINDS.iter().map(|kind| kind.value).collect();
        assert_eq!(
            values,
            [
                "agy",
                "codex",
                "claude",
                "cursor",
                "grok",
                "grok-build",
                "zen",
                "go",
                "openai",
                "openrouter"
            ]
        );
        for value in values {
            assert_eq!(ProviderKind::from_str(value).unwrap().as_str(), value);
        }
    }

    /// Provider paths match complete path segments.
    #[test]
    fn provider_path_matches_kind() {
        assert!(is_provider_path("/openai/example/v1/models"));
        assert!(!is_provider_path("/openaix/example"));
    }
}
