use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::openai::{ChatCompletionRequest, Model};
use crate::provider::common::proxy::{check_model, error, filter_models};
use crate::provider::common::session::RequestSession;
use crate::provider::{openai, registry::Provider};
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};

/// Translation between chat completions and the upstream Responses API.
#[path = "translate.rs"]
mod translate;

/// Shared redirect-free client for OAuth requests.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
});

/// Install virtual-key-protected routes for one Grok billing pool.
pub(super) fn router(settings: &SettingsStore, kind: &'static str) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .layer(Extension(kind))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), kind),
            settings::virtual_key_auth,
        ))
}

/// Return the fixed upstream host for the selected billing pool.
fn base(kind: &str) -> Option<&'static str> {
    match kind {
        "grok" => Some("https://api.x.ai/v1"),
        "grok-build" => Some("https://cli-chat-proxy.grok.com/v1"),
        _ => None,
    }
}

/// Construct scoped conversation identity headers.
fn conversation_headers(session: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = header::HeaderValue::from_str(session) {
        headers.insert("x-grok-conv-id", value);
    }
    headers
}

/// Construct the Grok Build CLI fingerprint and request identity headers.
fn build_headers(session: &str, model: &str, turns: usize) -> HeaderMap {
    let mut headers = conversation_headers(session);
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_static("grok-shell/0.2.101 (linux; x64)"),
    );
    headers.insert(
        "x-xai-token-auth",
        header::HeaderValue::from_static("xai-grok-cli"),
    );
    headers.insert(
        "x-grok-client-identifier",
        header::HeaderValue::from_static("grok-shell"),
    );
    headers.insert(
        "x-grok-client-mode",
        header::HeaderValue::from_static("interactive"),
    );
    headers.insert(
        "x-grok-client-version",
        header::HeaderValue::from_static("0.2.101"),
    );
    headers.insert(
        "x-authenticateresponse",
        header::HeaderValue::from_static("authenticate-response"),
    );
    if let Ok(value) = header::HeaderValue::from_str(session) {
        headers.insert("x-grok-session-id", value);
    }
    if let Ok(value) = header::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()) {
        headers.insert("x-grok-req-id", value);
    }
    if let Ok(value) = header::HeaderValue::from_str(&turns.max(1).to_string()) {
        headers.insert("x-grok-turn-idx", value);
    }
    if let Ok(value) = header::HeaderValue::from_str(model) {
        headers.insert("x-grok-model-override", value);
    }
    headers
}

/// Count observed user turns in Responses input.
fn turns(value: &Value) -> usize {
    value
        .get("input")
        .and_then(Value::as_array)
        .map_or(1, |items| {
            items
                .iter()
                .filter(|item| item.get("role").and_then(Value::as_str) == Some("user"))
                .count()
                .max(1)
        })
}

/// Bound session memory while retaining the highest observed turn for active sessions.
#[derive(Default)]
struct TurnCache {
    /// Highest observed turn and last use for each scoped session.
    sessions: HashMap<String, (usize, Instant)>,
}

impl TurnCache {
    /// Record an observed turn, evicting the least recently used session when full.
    fn observe(&mut self, session: &str, observed: usize) -> usize {
        if !self.sessions.contains_key(session)
            && self.sessions.len() >= 4096
            && let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| id.clone())
        {
            self.sessions.remove(&oldest);
        }
        let entry = self
            .sessions
            .entry(session.to_owned())
            .or_insert_with(|| (1, Instant::now()));
        entry.0 = entry.0.max(observed);
        entry.1 = Instant::now();
        entry.0
    }
}

/// Bounded process-local history of Build session turns.
static TURNS: LazyLock<Mutex<TurnCache>> = LazyLock::new(|| Mutex::new(TurnCache::default()));

/// Select credentials only from the requested provider; never forward caller credentials.
async fn send(
    state: &AppState,
    provider: &Provider,
    kind: &str,
    query: Option<&str>,
    session: &str,
    body: Option<&Value>,
) -> Result<reqwest::Response, String> {
    if provider.kind != kind {
        return Err("Invalid Grok provider".into());
    }
    let base = base(kind).ok_or("Invalid Grok provider")?;
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize Grok client")?;
    let (method, path) = if body.is_some() {
        (Method::POST, "responses")
    } else {
        (Method::GET, "models")
    };
    let mut url = format!("{base}/{path}");
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id).await?;
    let mut last = None;
    let mut last_error = None;
    for credential in credentials {
        let token =
            match super::auth::access_token(&state.settings, &provider.id, &credential.id, kind)
                .await
            {
                Ok(token) => token,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
        let mut request = client.request(method.clone(), &url).bearer_auth(token);
        if kind == "grok-build" {
            let model = body
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let turn = body.map_or(1, |value| {
                TURNS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .observe(session, turns(value))
            });
            request = request.headers(build_headers(session, model, turn));
        } else {
            request = request.headers(conversation_headers(session));
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Failed to contact Grok upstream")?;
        if !matches!(response.status().as_u16(), 401 | 403 | 429) {
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| last_error.unwrap_or_else(|| "No connected Grok credentials".into()))
}

/// Render an upstream failure in the standard error envelope.
fn upstream_error(message: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(crate::server::error_body(
            message,
            "upstream_error",
            Some("bad_gateway"),
        )),
    )
        .into_response()
}

/// Fetch and validate the upstream model-list envelope.
async fn model_list(
    state: &AppState,
    provider: &Provider,
    kind: &str,
    session: &str,
) -> Result<Value, String> {
    let response = send(state, provider, kind, None, session, None).await?;
    if !response.status().is_success() {
        return Err(format!(
            "Grok model discovery returned HTTP {}",
            response.status()
        ));
    }
    openai::parse_model_list(
        response.bytes_stream(),
        "Failed to read Grok model list",
        "Grok model list is too large",
        "Invalid Grok model list",
    )
    .await
}

/// Discover model metadata from the selected billing pool.
pub(super) async fn discover(
    state: &AppState,
    provider: &Provider,
    kind: &str,
) -> Result<Vec<Model>, String> {
    let session = uuid::Uuid::new_v4().to_string();
    let value = model_list(state, provider, kind, &session).await?;
    Ok(openai::models_from_list(&value))
}

/// Return upstream models filtered by virtual-key permissions.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(kind): Extension<&'static str>,
    Extension(session): Extension<RequestSession>,
) -> Response {
    match model_list(&state, &provider, kind, &session.id).await {
        Ok(mut value) => {
            if filter_models(&mut value, &permissions) {
                Json(value).into_response()
            } else {
                upstream_error("Invalid Grok model list")
            }
        }
        Err(message) => upstream_error(&message),
    }
}

/// Forward a model-authorized Responses request without altering its fields.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(kind): Extension<&'static str>,
    Extension(session): Extension<RequestSession>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if let Err(error) = check_model(&body, &permissions) {
        return error.into_response();
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "Invalid JSON request",
                "invalid_request_error",
                "invalid_json",
            )
            .into_response();
        }
    };
    match send(
        &state,
        &provider,
        kind,
        query.as_deref(),
        &session.id,
        Some(&value),
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(message) => upstream_error(&message),
    }
}

/// Reject chat options that cannot be represented by the shared request type.
fn unsupported_chat_option(value: &Value) -> bool {
    ["response_format", "stop"]
        .iter()
        .any(|key| value.get(*key).is_some_and(|v| !v.is_null()))
        || value
            .get("n")
            .is_some_and(|v| !v.is_null() && v.as_u64() != Some(1))
        || value
            .get("parallel_tool_calls")
            .is_some_and(|v| !v.is_null() && v.as_bool() != Some(true))
        || value
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools.iter().any(|tool| {
                    tool.pointer("/function/strict")
                        .is_some_and(|v| !v.is_null())
                })
            })
        || value
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    let content = message.get("content");
                    let image_detail = content.and_then(Value::as_array).is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.pointer("/image_url/detail")
                                .is_some_and(|v| !v.is_null())
                        })
                    });
                    let invalid_tool_content = message.get("role").and_then(Value::as_str)
                        == Some("tool")
                        && !content.is_some_and(|content| {
                            content.is_string()
                                || content.as_array().is_some_and(|parts| {
                                    parts.iter().all(|part| {
                                        matches!(
                                            part.get("type").and_then(Value::as_str),
                                            Some("text" | "input_text" | "output_text")
                                        ) && part.get("text").is_some_and(Value::is_string)
                                    })
                                })
                        });
                    image_detail || invalid_tool_content
                })
            })
}

/// Translate a model-authorized Chat request to and from Responses.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(kind): Extension<&'static str>,
    Extension(session): Extension<RequestSession>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if let Err(error) = check_model(&body, &permissions) {
        return error.into_response();
    }
    if serde_json::from_slice::<Value>(&body)
        .ok()
        .as_ref()
        .is_some_and(unsupported_chat_option)
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Unsupported chat option",
            "invalid_request_error",
            "unsupported_parameter",
        )
        .into_response();
    }
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "Invalid chat completion request",
                "invalid_request_error",
                "invalid_json",
            )
            .into_response();
        }
    };
    let value = translate::chat_to_responses(&req);
    let response = match send(
        &state,
        &provider,
        kind,
        query.as_deref(),
        &session.id,
        Some(&value),
    )
    .await
    {
        Ok(response) => response,
        Err(message) => return upstream_error(&message),
    };
    if !response.status().is_success() {
        return openai::forward(response);
    }
    if req.stream.unwrap_or(false) {
        let include_usage = req
            .stream_options
            .as_ref()
            .and_then(|options| options.include_usage)
            .unwrap_or(false);
        return translate::stream_to_chat(response, req.model, include_usage);
    }
    match response.json::<Value>().await {
        Ok(value) => match translate::responses_to_chat(&value, &req.model) {
            Ok(value) => Json(value).into_response(),
            Err(message) => upstream_error(&message),
        },
        Err(_) => upstream_error("Invalid Grok response"),
    }
}

/// Checks for pool isolation and scoped request identity.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build requests carry scoped identity, model, turns, and unique request IDs.
    #[test]
    fn build_identity_uses_scoped_session_and_user_turns() {
        let body = json!({"input": [{"role":"user"}, {"role":"assistant"}, {"role":"user"}]});
        let headers = build_headers("scoped-session", "grok-example", turns(&body));
        assert_eq!(headers["x-grok-session-id"], "scoped-session");
        assert_eq!(headers["x-grok-conv-id"], "scoped-session");
        assert_eq!(headers["x-grok-turn-idx"], "2");
        assert_eq!(headers["x-grok-model-override"], "grok-example");
        assert_eq!(headers["x-xai-token-auth"], "xai-grok-cli");
        assert_ne!(
            headers["x-grok-req-id"],
            build_headers("scoped-session", "grok-example", 2)["x-grok-req-id"]
        );
    }

    /// API requests carry conversation identity without Build-only headers.
    #[test]
    fn api_headers_include_only_scoped_conversation_identity() {
        let headers = conversation_headers("scoped-session");
        assert_eq!(headers["x-grok-conv-id"], "scoped-session");
        assert!(!headers.contains_key("x-grok-session-id"));
        assert!(!headers.contains_key("x-xai-token-auth"));
    }

    /// Cached turn indices are monotonic and isolated by session.
    #[test]
    fn session_turns_never_decrease_while_cached() {
        let mut cache = TurnCache::default();
        assert_eq!(cache.observe("first", 3), 3);
        assert_eq!(cache.observe("first", 1), 3);
        assert_eq!(cache.observe("second", 0), 1);
        assert_eq!(cache.observe("first", 4), 4);
    }

    /// Unsupported chat options are rejected rather than silently dropped.
    #[test]
    fn rejects_unrepresentable_chat_options() {
        assert!(unsupported_chat_option(
            &json!({"response_format": {"type": "json_object"}})
        ));
        assert!(unsupported_chat_option(&json!({"stop": ["end"]})));
        assert!(unsupported_chat_option(
            &json!({"tools": [{"function": {"strict": true}}]})
        ));
        assert!(!unsupported_chat_option(
            &json!({"response_format": null, "tools": [{"function": {"name": "f"}}]})
        ));
        assert!(!unsupported_chat_option(
            &json!({"n": 1, "parallel_tool_calls": true})
        ));
        assert!(!unsupported_chat_option(
            &json!({"n": null, "parallel_tool_calls": null})
        ));
        assert!(unsupported_chat_option(&json!({"n": 0})));
        assert!(unsupported_chat_option(
            &json!({"parallel_tool_calls": false})
        ));
    }

    /// Chat requests with unrepresentable options receive a client-visible error.
    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn chat_handler_rejects_unsupported_options() {
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let settings = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings,
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        for option in [
            json!({"response_format": {"type": "json_object"}}),
            json!({"stop": ["end"]}),
            json!({"tools": [{"type": "function", "function": {"name": "f", "strict": true}}]}),
            json!({"parallel_tool_calls": false}),
            json!({"n": 2}),
            json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/image", "detail": "high"}}]}]}),
            json!({"messages": [{"role": "tool", "tool_call_id": "call", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/image"}}]}]}),
        ] {
            let mut body = json!({"model": "grok-example", "messages": []});
            body.as_object_mut()
                .unwrap()
                .extend(option.as_object().unwrap().clone());
            let response = chat_handler(
                State(state.clone()),
                Extension(Permissions {
                    providers: vec![],
                    models: vec![],
                }),
                Extension(Provider {
                    id: "grok".into(),
                    name: "Grok".into(),
                    kind: "grok".into(),
                    base_url: String::new(),
                }),
                Extension("grok"),
                Extension(RequestSession {
                    id: "scoped-session".into(),
                }),
                RawQuery(None),
                Bytes::from(body.to_string()),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                value.pointer("/error/code").and_then(Value::as_str),
                Some("unsupported_parameter")
            );
        }
    }

    /// Each billing pool uses its own fixed upstream host.
    #[test]
    fn billing_pools_have_distinct_fixed_hosts() {
        assert_eq!(base("grok"), Some("https://api.x.ai/v1"));
        assert_eq!(
            base("grok-build"),
            Some("https://cli-chat-proxy.grok.com/v1")
        );
        assert_eq!(base("other"), None);
    }
}
