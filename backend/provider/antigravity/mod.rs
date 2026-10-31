/// OAuth credentials and token refresh.
pub(crate) mod auth;
/// `OpenAI` chat completion response conversion.
pub(crate) mod chat;
/// Upstream HTTP client operations.
mod client;
/// Dashboard OAuth login endpoints.
pub(crate) mod dashboard_login;
/// Interactive OAuth login flow.
pub(crate) mod login;
/// Upstream model discovery and normalization.
pub(crate) mod models;
/// Conversion of requests to upstream payloads.
mod payload;
/// Account quota discovery and normalization.
pub(crate) mod quota;
/// `OpenAI` responses API conversion.
pub(crate) mod responses;
/// Provider API routes.
pub(crate) mod routes;
/// Server-sent event response handling.
pub(crate) mod sse;
/// Upstream streaming event decoding.
mod stream;

pub(crate) use auth::{AuthManager, Credentials};
pub(crate) use models::AntigravityModelInfo;
use stream::map_finish_reason;
pub(crate) use stream::{AntigravityEvent, spawn_event_stream};

/// Encodes a signature with its provider and wire-model scope.
fn scoped_signature(model: &str, signature: &str) -> String {
    serde_json::json!(["antigravity", model, signature]).to_string()
}

/// Encodes an ordered text or reasoning part for client replay.
fn provider_part(
    model: &str,
    text: &str,
    reasoning: bool,
    signature: Option<&str>,
) -> serde_json::Value {
    let mut part = serde_json::json!({
        "type": if reasoning { "reasoning" } else { "text" },
        "text": text
    });
    if let (Some(object), Some(signature)) = (part.as_object_mut(), signature) {
        object.insert(
            "thought_signature".to_string(),
            scoped_signature(model, signature).into(),
        );
    }
    part
}

/// Accepts only a signature issued for the same provider and wire model.
fn replay_signature(model: &str, signature: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(signature).ok()?;
    let items = value.as_array()?;
    if items.len() == 3
        && items.first()?.as_str()? == "antigravity"
        && items.get(1)?.as_str()? == model
    {
        Some(items.get(2)?.as_str()?.to_owned())
    } else {
        None
    }
}

/// HTTP client for the Antigravity upstream API.
pub(crate) struct AntigravityClient {
    /// Shared connection pool used for upstream requests.
    client: reqwest::Client,
    /// Most recent credential-bound discovery result, held for a short interval.
    model_cache: tokio::sync::Mutex<Option<ModelCache>>,
    /// Isolated endpoint override used only by behavioral tests.
    #[cfg(test)]
    test_endpoint: Option<String>,
}

/// A bounded discovery cache scoped to the exact upstream credentials.
struct ModelCache {
    /// SHA-256 digest of the access token used for discovery.
    token: [u8; 32],
    /// Project associated with the credentials.
    project: String,
    /// Instant at which discovery succeeded.
    fetched: std::time::Instant,
    /// Enabled discovered models.
    models: Vec<(String, AntigravityModelInfo)>,
}

/// Computes the cache identity without retaining a bearer token.
fn token_digest(token: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(token.as_bytes()).into()
}

impl AntigravityClient {
    /// Creates a client with a fresh HTTP connection pool.
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            model_cache: tokio::sync::Mutex::new(None),
            #[cfg(test)]
            test_endpoint: None,
        }
    }

    /// Returns the local test endpoint when configured.
    #[cfg(test)]
    fn endpoints(&self) -> [&str; 2] {
        if let Some(endpoint) = self.test_endpoint.as_deref() {
            return [endpoint, endpoint];
        }
        [auth::CLOUD_CODE_ASSIST_ENDPOINT, models::SANDBOX_ENDPOINT]
    }

    /// Returns the fixed production endpoints.
    #[cfg(not(test))]
    const fn endpoints() -> [&'static str; 2] {
        [auth::CLOUD_CODE_ASSIST_ENDPOINT, models::SANDBOX_ENDPOINT]
    }
}

/// Provider integration for Antigravity.
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
    ) -> Result<Vec<crate::openai::Model>, String> {
        <Self as super::common::Provider>::discover_permitted(
            state,
            provider,
            &crate::server::settings::Permissions {
                providers: Vec::new(),
                models: Vec::new(),
            },
        )
        .await
    }

    async fn discover_permitted(
        state: &crate::server::AppState,
        provider: &super::registry::Provider,
        permissions: &crate::server::settings::Permissions,
    ) -> Result<Vec<crate::openai::Model>, String> {
        let models = routes::fetch_models(state, provider).await?;
        Ok(permitted_models(models, permissions))
    }
}

/// Normalize and filter discovered families while retaining provider metadata.
fn permitted_models(
    models: Vec<(String, AntigravityModelInfo)>,
    permissions: &crate::server::settings::Permissions,
) -> Vec<crate::openai::Model> {
    models::normalize_models(models)
        .iter()
        .filter_map(|family| family.entry(permissions).map(|(entry, _)| entry))
        .collect()
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
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod discovery_permission_tests {
    use super::*;
    use crate::server::settings::Permissions;
    use serde_json::json;

    #[test]
    fn discovery_exposes_only_permitted_family_efforts() {
        let info: AntigravityModelInfo = serde_json::from_value(json!({
            "supportsThinking": true,
            "capabilities": {"vision": true}
        }))
        .unwrap();
        let models = vec![
            ("model-low".to_owned(), info.clone()),
            ("model-high".to_owned(), info),
        ];
        let permissions = Permissions {
            providers: vec![],
            models: vec!["model-high".into()],
        };
        let entries = permitted_models(models, &permissions);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "model");
        assert_eq!(
            entries[0].supported_reasoning_efforts,
            Some(vec!["high".into()])
        );
        assert_eq!(entries[0].default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(entries[0].extra["capabilities"], json!({"vision": true}));
    }
}
