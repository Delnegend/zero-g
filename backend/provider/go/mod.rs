use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, header};
use serde_json::Value;

use crate::provider::openai;
use crate::provider::quota::{QuotaSnapshot, QuotaWindow};
use crate::provider::registry::Provider;
use crate::server::{AppState, settings::SettingsStore};
use std::time::Duration;

/// Default `OpenCode` Go inference API base URL.
const DEFAULT_BASE: &str = "https://opencode.ai/inference/go/openai/v1";
/// `OpenCode` Go model discovery API base URL.
const MODELS_BASE: &str = "https://opencode.ai/zen/go/v1";
/// Convert an `OpenCode` Go usage response into a quota snapshot.
pub(crate) fn parse_go(value: &Value) -> Result<QuotaSnapshot, String> {
    let usage = value
        .get("usage")
        .ok_or("Invalid OpenCode Go usage response")?;
    let mut windows = Vec::new();
    for (field, label) in [
        ("rolling", "5 hours"),
        ("weekly", "7 days"),
        ("monthly", "Monthly"),
    ] {
        let window = usage
            .get(field)
            .ok_or("Incomplete OpenCode Go usage response")?;
        let used = window
            .get("percent")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .ok_or("Invalid OpenCode Go usage percentage")?;
        let reset_at = window
            .get("resetsAt")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp());
        windows.push(QuotaWindow {
            label: label.to_string(),
            remaining_percent: Some(100.0 - used.clamp(0.0, 100.0)),
            reset_at,
        });
    }
    Ok(QuotaSnapshot::Quota { windows })
}

/// Fetch usage for an `OpenCode` Go credential.
pub(crate) async fn go_usage(
    store: &SettingsStore,
    credential: &str,
) -> Result<QuotaSnapshot, String> {
    let key = store
        .secret_get(&format!("credential.{credential}.api_key"))
        .await?
        .ok_or("OpenCode Go API key is not connected")?;
    if key.is_empty() {
        return Err("OpenCode Go requires an API key".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Unable to create OpenCode Go client")?;
    let response = client
        .get("https://opencode.ai/zen/go/v1/usage")
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Unable to contact OpenCode Go")?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenCode Go returned HTTP {}",
            response.status().as_u16()
        ));
    }
    parse_go(
        &response
            .json::<Value>()
            .await
            .map_err(|_| "Invalid OpenCode Go usage response")?,
    )
}

/// Select the configured or default base URL for inference or model discovery.
pub(crate) fn base(provider: &Provider, models: bool) -> &str {
    if provider.base_url.is_empty() || provider.base_url.trim_end_matches('/') == DEFAULT_BASE {
        if models { MODELS_BASE } else { DEFAULT_BASE }
    } else {
        &provider.base_url
    }
}

/// Send an authenticated request to an `OpenCode` Go upstream.
pub(crate) async fn send_upstream(
    base: &str,
    key: &str,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, &'static str> {
    use std::sync::LazyLock;
    static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
    });
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize OpenCode Go provider")?;
    let mut request = client
        .request(method, url)
        .header(
            header::USER_AGENT,
            concat!("zero-g/", env!("CARGO_PKG_VERSION")),
        )
        .bearer_auth(key);
    for name in [
        "accept",
        "content-type",
        "x-opencode-session",
        "session_id",
        "session-id",
        "x-session-id",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    request
        .send()
        .await
        .map_err(|_| "Failed to contact OpenCode Go provider")
}

/// Build the authenticated `OpenCode` Go proxy routes.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    openai::routes::go_router(settings)
}

/// `OpenCode` Go provider backend.
pub(crate) struct Backend;

impl super::openai::ModelEndpoint for Backend {
    async fn request(provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str> {
        send_upstream(
            base(provider, true),
            key,
            "models",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }
}

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "go",
        label: None,
        oauth: false,
        api_key: true,
        fixed_id: Some("go"),
        fixed_name: Some("OpenCode Go"),
        keyless: false,
        default_base_url: Some(DEFAULT_BASE),
        legacy: super::common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };
    fn router(settings: &SettingsStore) -> Router<AppState> {
        router(settings)
    }
    async fn discover(
        state: &AppState,
        provider: &Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        openai::discover::<Self>(&state.settings, provider).await
    }
}

impl super::common::QuotaProvider for Backend {
    async fn quota(
        store: &SettingsStore,
        _provider: &str,
        credential: &str,
    ) -> Result<QuotaSnapshot, String> {
        go_usage(store, credential).await
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
