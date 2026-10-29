pub(crate) mod auth;
pub(crate) mod chat;
mod client;
pub(crate) mod dashboard_login;
pub(crate) mod login;
pub(crate) mod models;
mod payload;
pub(crate) mod quota;
pub(crate) mod responses;
pub(crate) mod routes;
pub(crate) mod sse;
mod stream;

pub(crate) use auth::{AuthManager, Credentials};
pub(crate) use models::AntigravityModelInfo;
use stream::map_finish_reason;
pub(crate) use stream::{AntigravityEvent, spawn_event_stream};

pub(crate) struct AntigravityClient {
    client: reqwest::Client,
}

impl AntigravityClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "agy",
        label: "Antigravity (Google)",
        oauth: true,
        api_key: false,
        fixed_id: Some("agy"),
        fixed_name: Some("Google Antigravity"),
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
    ) -> Result<Vec<serde_json::Value>, String> {
        let models = routes::fetch_models(state, provider).await?;
        let permissions = crate::server::settings::Permissions {
            providers: Vec::new(),
            models: Vec::new(),
        };
        models::normalize_models(models)
            .iter()
            .filter_map(|family| family.entry(&permissions))
            .map(|(entry, info)| {
                let mut raw = serde_json::to_value(info)
                    .map_err(|_| "Failed to serialize Antigravity model")?;
                let normalized = serde_json::to_value(entry)
                    .map_err(|_| "Failed to serialize Antigravity model")?;
                if let (Some(raw), serde_json::Value::Object(normalized)) =
                    (raw.as_object_mut(), normalized)
                {
                    raw.extend(normalized.into_iter().filter(|(_, value)| !value.is_null()));
                }
                Ok(raw)
            })
            .collect()
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

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> axum::Router {
        dashboard_login::router(state)
    }
}
