pub(crate) mod errors;
pub(crate) mod proxy;

use crate::provider::{quota, registry};
use crate::server::{AppState, settings::SettingsStore};
use axum::Router;
use serde_json::Value;

pub(crate) trait Provider {
    const DESCRIPTOR: ProviderDescriptor;
    fn router(settings: &SettingsStore) -> Router<AppState>;
    async fn discover(
        state: &AppState,
        provider: &registry::Provider,
    ) -> Result<Vec<Value>, String>;
}

pub(crate) trait QuotaProvider {
    async fn quota(
        store: &SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<quota::QuotaSnapshot, String>;
}

pub(crate) trait DashboardProvider {
    fn dashboard_router(state: AppState) -> Router;
}

#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct ProviderDescriptor {
    pub(crate) value: &'static str,
    pub(crate) label: &'static str,
    pub(crate) oauth: bool,
    pub(crate) api_key: bool,
    pub(crate) fixed_id: Option<&'static str>,
    pub(crate) fixed_name: Option<&'static str>,
    #[serde(skip)]
    pub(crate) keyless: bool,
    #[serde(skip)]
    pub(crate) default_base_url: Option<&'static str>,
    #[serde(skip)]
    pub(crate) legacy: LegacyPolicy,
}

#[derive(Clone, Copy)]
pub(crate) struct LegacyPolicy {
    pub(crate) key: Option<&'static str>,
    pub(crate) same_base: bool,
}
