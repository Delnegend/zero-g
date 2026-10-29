use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::provider::openai;
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};

const DEFAULT_BASE: &str = "https://opencode.ai/inference/go/openai/v1";
const MODELS_BASE: &str = "https://opencode.ai/zen/go/v1";

pub(crate) fn base(provider: &Provider, models: bool) -> &str {
    if provider.base_url.is_empty() || provider.base_url.trim_end_matches('/') == DEFAULT_BASE {
        if models { MODELS_BASE } else { DEFAULT_BASE }
    } else {
        &provider.base_url
    }
}

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

#[derive(Debug)]
struct ProxyError(StatusCode, &'static str, &'static str, &'static str);

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::server::error_body(self.1, self.2, Some(self.3))),
        )
            .into_response()
    }
}

const fn error(
    status: StatusCode,
    message: &'static str,
    kind: &'static str,
    code: &'static str,
) -> ProxyError {
    ProxyError(status, message, kind, code)
}

const fn settings_error() -> ProxyError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to load OpenAI-compatible provider settings",
        "server_error",
        "internal_error",
    )
}

fn check_model(body: &[u8], permissions: &Permissions) -> Result<(), ProxyError> {
    let model = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "A model is required",
            "invalid_request_error",
            "invalid_model",
        ));
    };
    if !permissions.allows_model(&model) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        ));
    }
    Ok(())
}

fn filter_models(value: &mut Value, permissions: &Permissions) -> bool {
    let Some(data) = value.get_mut("data").and_then(Value::as_array_mut) else {
        return false;
    };
    data.retain(|model| {
        model
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| permissions.allows_model(id))
    });
    true
}

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
