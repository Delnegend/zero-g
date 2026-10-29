pub(crate) mod antigravity;
pub(crate) mod catalog;
pub(crate) mod codex;
pub(crate) mod common;
pub(crate) mod credentials;
pub(crate) mod go;
pub(crate) mod login;
pub(crate) mod openai;
pub(crate) mod quota;
pub(crate) mod registry;
pub(crate) mod zen;

use self::common::{DashboardProvider, Provider, ProviderDescriptor, QuotaProvider};
use crate::server::{AppState, settings::SettingsStore};
use axum::Router;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Providers {
    pub(crate) client: Arc<antigravity::AntigravityClient>,
    pub(crate) codex_client: Arc<codex::CodexClient>,
}

impl Providers {
    pub(crate) fn new() -> Self {
        Self {
            client: Arc::new(antigravity::AntigravityClient::new()),
            codex_client: Arc::new(codex::CodexClient::new()),
        }
    }
}

pub(crate) const PROVIDER_KINDS: &[ProviderDescriptor] = &[
    antigravity::Backend::DESCRIPTOR,
    codex::Backend::DESCRIPTOR,
    zen::Backend::DESCRIPTOR,
    go::Backend::DESCRIPTOR,
    openai::Backend::DESCRIPTOR,
];

#[derive(Clone, Copy)]
pub(crate) enum ProviderKind {
    Antigravity,
    Codex,
    Zen,
    Go,
    Openai,
}

const ALL: [ProviderKind; 5] = [
    ProviderKind::Antigravity,
    ProviderKind::Codex,
    ProviderKind::Zen,
    ProviderKind::Go,
    ProviderKind::Openai,
];

impl ProviderKind {
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        ALL.into_iter().find(|kind| kind.as_str() == value)
    }
    pub(crate) const fn descriptor(self) -> ProviderDescriptor {
        match self {
            Self::Antigravity => antigravity::Backend::DESCRIPTOR,
            Self::Codex => codex::Backend::DESCRIPTOR,
            Self::Zen => zen::Backend::DESCRIPTOR,
            Self::Go => go::Backend::DESCRIPTOR,
            Self::Openai => openai::Backend::DESCRIPTOR,
        }
    }
    pub(crate) const fn as_str(self) -> &'static str {
        self.descriptor().value
    }
    pub(crate) const fn oauth(self) -> bool {
        self.descriptor().oauth
    }
    pub(crate) const fn api_key(self) -> bool {
        self.descriptor().api_key
    }
    pub(crate) const fn keyless(self) -> bool {
        self.descriptor().keyless
    }
    pub(crate) const fn default_base_url(self) -> Option<&'static str> {
        self.descriptor().default_base_url
    }

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

    pub(crate) const fn quota(self) -> Option<QuotaCapability> {
        match self {
            Self::Antigravity => Some(QuotaCapability::Antigravity),
            Self::Codex => Some(QuotaCapability::Codex),
            Self::Zen => Some(QuotaCapability::Zen),
            Self::Go => Some(QuotaCapability::Go),
            Self::Openai => None,
        }
    }

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

pub(crate) enum QuotaCapability {
    Antigravity,
    Codex,
    Zen,
    Go,
}
impl QuotaCapability {
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

pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    ALL.into_iter().fold(Router::new(), |router, kind| {
        router.nest(&format!("/{}/{{id}}", kind.as_str()), kind.router(settings))
    })
}

pub(crate) fn is_provider_path(path: &str) -> bool {
    path.strip_prefix('/')
        .and_then(|path| path.split('/').next())
        .and_then(ProviderKind::from_str)
        .is_some()
}

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

pub(crate) async fn start() -> Option<tokio::process::Child> {
    zen::start().await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
