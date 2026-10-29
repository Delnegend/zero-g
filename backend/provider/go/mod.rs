use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::provider::common::proxy::{
    ProxyError, check_model, error, filter_models, settings_error,
};
use crate::provider::openai;
use crate::provider::quota::{QuotaSnapshot, QuotaWindow};
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};
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
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "go"),
            settings::virtual_key_auth,
        ))
}

/// Resolve credentials and send an `OpenCode` Go proxy request.
async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, ProxyError> {
    let credentials = crate::provider::credentials::list(settings, &provider.id)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    for credential in credentials {
        let key = settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = send_upstream(
            base(provider, path == "models"),
            &key,
            path,
            query,
            method.clone(),
            headers,
            body.clone(),
        )
        .await
        .map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if !matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No connected OpenCode Go credentials",
            "upstream_error",
            "no_credentials",
        )
    })
}

/// Fetch and filter models allowed by the virtual key.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let response = match send(
        &state.settings,
        &provider,
        "models",
        query.as_deref(),
        Method::GET,
        &headers,
        None,
    )
    .await
    {
        Ok(response) => response,
        Err(response) => return response.into_response(),
    };
    if !response.status().is_success() || permissions.models.is_empty() {
        return openai::forward(response);
    }
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(mut value) = response.json::<Value>().await else {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    };
    if !filter_models(&mut value, &permissions) {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    }
    let mut result = Response::new(Body::from(value.to_string()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

/// Forward an inference request after checking model permissions.
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    match send(
        &state.settings,
        &provider,
        path,
        query.as_deref(),
        Method::POST,
        &headers,
        Some(body),
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(response) => response.into_response(),
    }
}

/// Handle chat completion requests.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "chat/completions",
    )
    .await
}

/// Handle responses API requests.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "responses",
    )
    .await
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
        label: "OpenCode Go",
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
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    #[test]
    fn endpoints_and_model_permissions() {
        let mut provider = Provider {
            id: "go".into(),
            name: "Go".into(),
            kind: "go".into(),
            base_url: String::new(),
        };
        assert_eq!(base(&provider, false), DEFAULT_BASE);
        assert_eq!(base(&provider, true), MODELS_BASE);
        provider.base_url = "https://example.com/v1".into();
        assert_eq!(base(&provider, true), "https://example.com/v1");
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        assert!(check_model(br#"{"model":"allowed"}"#, &permissions).is_ok());
        assert_eq!(
            check_model(br#"{"model":"other"}"#, &permissions)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut models = json!({"data":[{"id":"allowed","extra":1},{"id":"other"}]});
        assert!(filter_models(&mut models, &permissions));
        assert_eq!(models, json!({"data":[{"id":"allowed","extra":1}]}));
    }

    #[tokio::test]
    async fn forwards_auth_protocol_and_real_session_without_impersonation() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let app = Router::new().route(
            "/{*path}",
            axum::routing::any(move |req: axum::extract::Request| {
                let tx = tx.clone();
                async move {
                    let (parts, body) = req.into_parts();
                    let body = axum::body::to_bytes(body, 1024).await.unwrap();
                    tx.send((parts, body)).unwrap();
                    (StatusCode::OK, "ok")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for path in ["chat/completions", "responses", "models"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                "Bearer client-secret".parse().unwrap(),
            );
            headers.insert(header::USER_AGENT, "opencode/fake".parse().unwrap());
            headers.insert("x-opencode-session", "conversation-123".parse().unwrap());
            let method = if path == "models" {
                Method::GET
            } else {
                Method::POST
            };
            let response = send_upstream(
                &base,
                "upstream-secret",
                path,
                Some("a=1"),
                method,
                &headers,
                Some(Bytes::from_static(br#"{"model":"m"}"#)),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let (parts, body) = rx.recv().await.unwrap();
            assert_eq!(parts.uri.to_string(), format!("/v1/{path}?a=1"));
            assert_eq!(
                parts.headers[header::AUTHORIZATION],
                "Bearer upstream-secret"
            );
            assert!(
                parts.headers[header::USER_AGENT]
                    .to_str()
                    .unwrap()
                    .starts_with("zero-g/")
            );
            assert_eq!(parts.headers["x-opencode-session"], "conversation-123");
            assert_eq!(body.as_ref(), br#"{"model":"m"}"#);
        }
        server.abort();
    }

    #[tokio::test]
    async fn credential_failover_uses_next_key_for_auth_and_rate_limits() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let store = SettingsStore::new(
            crate::database::Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "go".to_string(),
            name: "Go".to_string(),
            kind: "go".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        for id in ["first", "second"] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: "go".to_string(),
                name: id.to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
            let name = format!("credential.{id}.api_key");
            Secret::upsert_by_name(name.as_str())
                .value(store.encrypt(&name, id).unwrap())
                .exec(&mut (*store.database.db).clone())
                .await
                .unwrap();
        }
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let (tx, mut rx) = mpsc::unbounded_channel();
            let app = Router::new().route(
                "/{*path}",
                axum::routing::any({
                    let count = count.clone();
                    move |headers: HeaderMap| {
                        let count = count.clone();
                        let tx = tx.clone();
                        async move {
                            tx.send(headers[header::AUTHORIZATION].clone()).unwrap();
                            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                                status
                            } else {
                                StatusCode::OK
                            }
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider = Provider {
                id: "go".into(),
                name: "Go".into(),
                kind: "go".into(),
                base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            };
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let response = send(
                &store,
                &provider,
                "responses",
                None,
                Method::POST,
                &HeaderMap::new(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_ne!(rx.recv().await.unwrap(), rx.recv().await.unwrap());
            assert_eq!(count.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
}
