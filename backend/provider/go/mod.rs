use axum::body::Bytes;
use axum::extract::{RawQuery, State};
#[cfg(test)]
use axum::http::StatusCode;
use axum::http::{HeaderMap, Method, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::provider::common::proxy::{ProxyError, check_model, model_response, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
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
#[allow(clippy::too_many_arguments)]
async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, ProxyError> {
    retry_api_keys(
        settings,
        &provider.id,
        "No connected OpenCode Go credentials",
        identity,
        |key| {
            let method = method.clone();
            let body = body.clone();
            async move {
                send_upstream(
                    base(provider, path == "models"),
                    &key,
                    path,
                    query,
                    method,
                    headers,
                    body,
                )
                .await
            }
        },
    )
    .await
}

/// Fetch and filter models allowed by the virtual key.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
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
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(response) => response,
        Err(response) => return response.into_response(),
    };
    model_response(response, &permissions).await
}

/// Forward an inference request after checking model permissions.
#[allow(clippy::too_many_arguments)]
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
    identity: Option<&RequestIdentity>,
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
        identity,
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
    identity: Option<Extension<RequestIdentity>>,
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
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
}

/// Handle responses API requests.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
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
        identity.as_ref().map(|Extension(identity)| identity),
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
    use axum::Json;
    use axum::body::Body;
    use serde_json::json;
    use tokio::sync::mpsc;

    #[test]
    fn endpoint_selection() {
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
    }

    // Keep the authenticated upstream and both response scenarios together.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn restricted_model_listing_filters_and_rejects_invalid_upstream() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use tower::ServiceExt;

        let app = Router::new().route(
            "/v1/models",
            axum::routing::get(|headers: HeaderMap, RawQuery(query): RawQuery| async move {
                if headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    != Some("Bearer upstream-secret")
                {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error":"invalid credential"})),
                    );
                }
                let body = if query.as_deref() == Some("invalid=1") {
                    json!({"data":"not an array"})
                } else {
                    json!({"data":[{"id":"allowed","extra":1},{"id":"other"}]})
                };
                (StatusCode::OK, Json(body))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "go".to_string(),
            name: "Go".to_string(),
            kind: "go".to_string(),
            base_url: base_url.clone(),
            deleted: false
        })
        .exec(&mut (*database.db).clone())
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "key".to_string(),
            provider_id: "go".to_string(),
            name: "key".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*database.db).clone())
        .await
        .unwrap();
        let secret_name = "credential.key.api_key";
        Secret::upsert_by_name(secret_name)
            .value(store.encrypt(secret_name, "upstream-secret").unwrap())
            .exec(&mut (*database.db).clone())
            .await
            .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings: store,
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let provider = Provider {
            id: "go".into(),
            name: "Go".into(),
            kind: "go".into(),
            base_url,
        };
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        let router = Router::new()
            .route("/v1/models", axum::routing::get(models_handler))
            .layer(Extension(provider))
            .layer(Extension(permissions))
            .with_state(state);
        for (uri, expected) in [
            ("/v1/models", StatusCode::OK),
            ("/v1/models?invalid=1", StatusCode::BAD_GATEWAY),
        ] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap(),
                    json!({"data":[{"id":"allowed","extra":1}]})
                );
            }
        }
        server.abort();
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
            let identity = RequestIdentity::default();
            let response = send(
                &store,
                &provider,
                "responses",
                None,
                Method::POST,
                &HeaderMap::new(),
                None,
                Some(&identity),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let first = rx.recv().await.unwrap();
            let second = rx.recv().await.unwrap();
            assert_ne!(first, second);
            let expected = second.to_str().unwrap().trim_start_matches("Bearer ");
            let snapshot = identity.snapshot();
            assert_eq!(snapshot.credential_id.as_deref(), Some(expected));
            assert_eq!(snapshot.credential_name.as_deref(), Some(expected));
            assert_eq!(count.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
}
