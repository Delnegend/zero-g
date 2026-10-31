use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::Value;

use crate::openai::{ChatCompletionRequest, Model, ModelListResponse, StreamOptions};
use crate::provider::antigravity::AntigravityModelInfo;
use crate::provider::antigravity::chat::ChatStreamSerializer;
use crate::provider::antigravity::models::{ResolveError, normalize_models};
use crate::provider::antigravity::responses::ResponsesStreamSerializer;
use crate::provider::antigravity::{AntigravityClient, AuthManager};
use crate::provider::common::errors::UpstreamError;
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::session::RequestSession;
use crate::provider::registry::Provider;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

/// Builds the virtual-key-protected Antigravity API routes.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            false,
            crate::openai::validate_translated_request,
        ))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "agy"),
            settings::virtual_key_auth,
        ))
}

/// Returns the response for a model disallowed by the virtual key.
fn model_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(crate::server::error_body(
            "Model is not allowed for this virtual key",
            "permission_error",
            Some("model_not_allowed"),
        )),
    )
        .into_response()
}

/// Validates the supported `OpenAI` tool selection shapes.
fn validate_tool_choice(
    choice: Option<&Value>,
    tools: Option<&[crate::openai::Tool]>,
) -> Option<Response> {
    let choice = choice?;
    let available = tools.unwrap_or_default();
    let valid = match choice.as_str() {
        Some("auto" | "none") => true,
        Some("required") => !available.is_empty(),
        Some(_) => false,
        None => {
            let name = choice
                .pointer("/function/name")
                .or_else(|| choice.get("name"))
                .and_then(Value::as_str);
            choice.get("type").and_then(Value::as_str) == Some("function")
                && name.is_some_and(|name| available.iter().any(|tool| tool.function.name == name))
        }
    };
    (!valid).then(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(crate::server::error_body(
                "Invalid tool_choice or unavailable function",
                "invalid_request_error",
                Some("invalid_tool_choice"),
            )),
        )
            .into_response()
    })
}

/// Rejects media that cannot be forwarded without loss.
fn validate_request_media(messages: &[crate::openai::ChatMessage]) -> Option<Response> {
    super::payload::validate_media(messages)
        .err()
        .map(|message| {
            (
                StatusCode::BAD_REQUEST,
                Json(crate::server::error_body(
                    &message,
                    "invalid_request_error",
                    Some("invalid_media"),
                )),
            )
                .into_response()
        })
}

/// Failure while selecting credentials or contacting the upstream service.
#[derive(Debug)]
enum PoolError {
    /// A local configuration or credential failure.
    Internal(String),
    /// An upstream request failure.
    Upstream(UpstreamError),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(message) => f.write_str(message),
            Self::Upstream(error) => write!(f, "{error}"),
        }
    }
}

/// Tries credentials in order, advancing on retryable upstream failures.
async fn try_candidates<C, T, F, Fut>(candidates: Vec<C>, mut attempt: F) -> Result<T, PoolError>
where
    F: FnMut(C) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    let mut candidates = candidates.into_iter().peekable();
    while let Some(candidate) = candidates.next() {
        match attempt(candidate).await {
            Err(PoolError::Upstream(error)) if error.retryable() && candidates.peek().is_some() => {
            }
            result => return result,
        }
    }
    Err(PoolError::Internal(
        "Google Antigravity is not configured. Authenticate on the dashboard Providers page."
            .into(),
    ))
}

/// Loads pooled credentials and runs an operation with each eligible candidate.
async fn with_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id)
        .await
        .map_err(PoolError::Internal)?;
    try_candidates(credentials, |credential| {
        let store = state.settings.clone();
        let provider_id = provider.id.clone();
        let operation = &operation;
        async move {
            let auth = AuthManager::load(store, &provider_id, &credential.id)
                .await
                .map_err(PoolError::Internal)?
                .ok_or_else(|| {
                    PoolError::Internal("Antigravity credentials no longer exist".into())
                })?;
            let creds =
                auth.get_credentials()
                    .await
                    .map_err(|error| match error.upstream_status {
                        Some(
                            status @ (StatusCode::UNAUTHORIZED
                            | StatusCode::FORBIDDEN
                            | StatusCode::TOO_MANY_REQUESTS),
                        ) => PoolError::Upstream(UpstreamError {
                            status: Some(status),
                            message: error.message,
                        }),
                        _ => PoolError::Internal(error.message),
                    })?;
            let result = operation(creds).await.map_err(PoolError::Upstream)?;
            if let Some(identity) = identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            Ok(result)
        }
    })
    .await
}

/// Starts an upstream chat stream using the credential pool.
async fn pooled_stream(
    state: &AppState,
    provider: &Provider,
    req: &ChatCompletionRequest,
    session_id: &str,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, PoolError> {
    with_pool(state, provider, identity, |creds| async move {
        let models = state.providers.client.fetch_models(&creds).await?;
        let info = models
            .iter()
            .find(|(id, _)| id == &req.model)
            .map(|(_, info)| info);
        let payload = AntigravityClient::build_request_payload(
            req,
            &creds.project_id,
            &req.model,
            session_id,
            info,
        );
        state
            .providers
            .client
            .stream_request(&creds, &payload, req.model.contains("claude"))
            .await
    })
    .await
}

/// Fetches upstream models using the credential pool.
async fn pool_models(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    with_pool(state, provider, identity, |creds| async move {
        state.providers.client.fetch_models(&creds).await
    })
    .await
}

/// Fetches upstream models, converting pool failures to strings.
pub(crate) async fn fetch_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
    pool_models(state, provider, None)
        .await
        .map_err(|error| error.to_string())
}

/// Converts a pool failure into an API error response.
fn pool_error_response(error: PoolError) -> Response {
    let (status, kind, code, message) = match error {
        PoolError::Internal(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal_error",
            message,
        ),
        PoolError::Upstream(error) => (
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "bad_gateway",
            error.message,
        ),
    };
    (
        status,
        Json(crate::server::error_body(&message, kind, Some(code))),
    )
        .into_response()
}

/// Lists discovered models permitted by the virtual key.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    let live_models = match pool_models(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(models) => models,
        Err(error) => return pool_error_response(error),
    };

    let data = normalize_models(live_models)
        .iter()
        .filter_map(|family| family.entry(&permissions).map(|(entry, _)| entry))
        .collect();
    Json(ModelListResponse {
        object: "list".to_string(),
        data,
    })
    .into_response()
}

/// Upstream reports the context window as `maxTokens`, separate from
/// `maxOutputTokens`; callers that only see `max_output_tokens` fall back to
/// their own defaults, so both are carried through.
pub(crate) fn to_model_entry(id: String, info: AntigravityModelInfo) -> Model {
    let reasoning = info.supports_thinking.unwrap_or(false);
    let effort_tier = [("-low", "low"), ("-medium", "medium"), ("-high", "high")]
        .into_iter()
        .find(|(suffix, _)| id.ends_with(suffix))
        .map(|(_, effort)| effort.to_string());
    let supported_efforts = effort_tier.as_ref().map_or_else(
        || reasoning.then(|| vec!["low".to_string(), "medium".to_string(), "high".to_string()]),
        |effort| Some(vec![effort.clone()]),
    );

    let mut extra: serde_json::Map<String, serde_json::Value> = info.extra.into_iter().collect();
    for key in [
        "id",
        "object",
        "created",
        "owned_by",
        "name",
        "display_name",
        "reasoning",
        "default_reasoning_effort",
        "supported_reasoning_efforts",
        "context_window",
        "max_output_tokens",
    ] {
        extra.remove(key);
    }
    extra.insert("displayName".into(), serde_json::json!(info.display_name));
    extra.insert(
        "supportsThinking".into(),
        serde_json::json!(info.supports_thinking),
    );
    extra.insert("maxTokens".into(), serde_json::json!(info.max_tokens));
    extra.insert(
        "maxOutputTokens".into(),
        serde_json::json!(info.max_output_tokens),
    );
    extra.insert("disabled".into(), serde_json::json!(info.disabled));
    Model {
        id,
        object: Some("model".to_string()),
        created: Some(1_700_000_000),
        owned_by: Some("google-antigravity".to_string()),
        name: info.display_name.clone(),
        display_name: info.display_name,
        reasoning: if reasoning { Some(true) } else { None },
        default_reasoning_effort: effort_tier,
        supported_reasoning_efforts: supported_efforts,
        context_window: info.max_tokens,
        max_output_tokens: info.max_output_tokens,
        extra,
    }
}

/// Resolves a requested model and reasoning effort against permitted models.
fn resolve_model(
    models: Vec<(String, AntigravityModelInfo)>,
    model: &str,
    effort: Option<&str>,
    permissions: &Permissions,
) -> Result<String, ResolveError> {
    for family in normalize_models(models) {
        if family.id == model {
            return family.resolve(effort, permissions);
        }
        if family.grouped && family.variants.iter().any(|(id, _)| id == model) {
            return if permissions.allows_model(model) || permissions.allows_model(&family.id) {
                Ok(model.to_owned())
            } else {
                Err(ResolveError::Forbidden)
            };
        }
    }
    // Preserve direct access to upstream models not present in discovery.
    if permissions.allows_model(model) {
        Ok(model.to_owned())
    } else {
        Err(ResolveError::Forbidden)
    }
}

/// Failure fetching or resolving a requested model.
enum RequestModelError {
    /// Model discovery failed.
    Pool(PoolError),
    /// Model resolution or authorization failed.
    Resolve(ResolveError),
}

impl RequestModelError {
    /// Converts the failure into an API error response.
    fn into_response(self) -> Response {
        match self {
            Self::Pool(error) => pool_error_response(error),
            Self::Resolve(ResolveError::Forbidden) => model_forbidden(),
            Self::Resolve(ResolveError::Unsupported) => (
                StatusCode::BAD_REQUEST,
                Json(crate::server::error_body(
                    "Reasoning effort is not supported by this model",
                    "invalid_request_error",
                    Some("unsupported_reasoning_effort"),
                )),
            )
                .into_response(),
        }
    }
}

/// Resolves a request model, fetching discovery data when needed.
async fn resolve_request_model(
    state: &AppState,
    provider: &Provider,
    permissions: &Permissions,
    model: &str,
    effort: Option<&str>,
) -> Result<String, RequestModelError> {
    if ["-low", "-medium", "-high"]
        .iter()
        .any(|suffix| model.ends_with(suffix))
        && permissions.allows_model(model)
    {
        return Ok(model.to_owned());
    }
    let models = pool_models(state, provider, None)
        .await
        .map_err(RequestModelError::Pool)?;
    resolve_model(models, model, effort, permissions).map_err(RequestModelError::Resolve)
}

/// Handles streaming and non-streaming chat completion requests.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
    if let Some(error) = validate_tool_choice(req.tool_choice.as_ref(), req.tools.as_deref()) {
        return error;
    }
    if let Some(error) = validate_request_media(&req.messages) {
        return error;
    }
    let effort = req.reasoning_effort.as_deref().or_else(|| {
        req.reasoning
            .as_ref()
            .and_then(|r| r.get("effort"))
            .and_then(Value::as_str)
    });
    req.model =
        match resolve_request_model(&state, &provider, &permissions, &req.model, effort).await {
            Ok(model) => model,
            Err(error) => return error.into_response(),
        };

    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        handle_chat_streaming(
            &state,
            &provider,
            req,
            &session.id,
            identity.as_ref().map(|Extension(identity)| identity),
        )
        .await
    } else {
        match with_pool(
            &state,
            &provider,
            identity.as_ref().map(|Extension(identity)| identity),
            |creds| {
                let req = &req;
                let client = &state.providers.client;
                let session_id = &session.id;
                async move { client.complete_non_streaming(&creds, req, session_id).await }
            },
        )
        .await
        {
            Ok(resp) => Json(resp).into_response(),
            Err(error) => pool_error_response(error),
        }
    }
}

/// Converts an upstream chat stream into chat-completion SSE events.
async fn handle_chat_streaming(
    state: &AppState,
    provider: &Provider,
    req: ChatCompletionRequest,
    session_id: &str,
    identity: Option<&RequestIdentity>,
) -> Response {
    let upstream_resp = match pooled_stream(state, provider, &req, session_id, identity).await {
        Ok(response) => response,
        Err(error) => return pool_error_response(error),
    };

    let include_usage = req
        .stream_options
        .as_ref()
        .and_then(|options: &StreamOptions| options.include_usage)
        .unwrap_or(false);

    let serializer = ChatStreamSerializer {
        stream_id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        req_model: req.model.clone(),
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
        include_usage,
        sent_role: false,
        has_tools: false,
        finish_sent: false,
        tool_call_index: 0,
        usage: crate::openai::Usage::default(),
        saw_usage: false,
    };

    crate::provider::antigravity::sse::spawn_sse(serializer, upstream_resp)
}

/// Handles streaming and non-streaming Responses API requests.
// Keep the response assembly and both streaming paths together.
#[allow(clippy::too_many_lines)]
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    if let Some(error) = validate_tool_choice(req.tool_choice.as_ref(), req.tools.as_deref()) {
        return error;
    }
    let mut chat_req = req.into_chat_request();
    if let Some(error) = validate_request_media(&chat_req.messages) {
        return error;
    }
    let effort = chat_req.reasoning_effort.as_deref().or_else(|| {
        chat_req
            .reasoning
            .as_ref()
            .and_then(|r| r.get("effort"))
            .and_then(Value::as_str)
    });
    chat_req.model =
        match resolve_request_model(&state, &provider, &permissions, &chat_req.model, effort).await
        {
            Ok(model) => model,
            Err(error) => return error.into_response(),
        };

    let is_streaming = chat_req.stream.unwrap_or(false);
    let original_model = chat_req.model.clone();

    if is_streaming {
        handle_responses_streaming(
            &state,
            &provider,
            chat_req,
            original_model,
            &session.id,
            identity.as_ref().map(|Extension(identity)| identity),
        )
        .await
    } else {
        match with_pool(
            &state,
            &provider,
            identity.as_ref().map(|Extension(identity)| identity),
            |creds| {
                let chat_req = &chat_req;
                let client = &state.providers.client;
                let session_id = &session.id;
                async move {
                    client
                        .complete_non_streaming(&creds, chat_req, session_id)
                        .await
                }
            },
        )
        .await
        {
            Ok(resp) => {
                let mut output_items = Vec::new();
                let mut output_text = String::new();

                for choice in &resp.choices {
                    if choice.message.content.is_some() || choice.message.provider_parts.is_some() {
                        let content = choice.message.content.as_deref().unwrap_or_default();
                        output_text.push_str(content);
                        output_items.push(ResponseOutputItem::Message {
                            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                            status: "completed".to_string(),
                            role: choice.message.role.clone(),
                            content: vec![ResponseContentPart::OutputText {
                                text: content.to_owned(),
                            }],
                            provider_parts: choice.message.provider_parts.clone(),
                        });
                    }
                    if let Some(tool_calls) = &choice.message.tool_calls {
                        for tc in tool_calls {
                            output_items.push(ResponseOutputItem::FunctionCall {
                                id: format!("fc_{}", uuid::Uuid::new_v4().simple()),
                                status: "completed".to_string(),
                                name: tc.function.name.clone(),
                                call_id: tc.id.clone(),
                                arguments: tc.function.arguments.clone(),
                                thought_signature: tc.thought_signature.clone(),
                            });
                        }
                    }
                }

                let usage = resp.usage.map(ResponseUsage::from);

                let response_obj = ResponseObject {
                    id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
                    object: "response".to_string(),
                    created_at: resp.created,
                    model: original_model,
                    status: if resp.choices.iter().any(|choice| {
                        choice.finish_reason.as_deref() != Some("stop")
                            && choice.finish_reason.as_deref() != Some("tool_calls")
                    }) {
                        "incomplete"
                    } else {
                        "completed"
                    }
                    .to_string(),
                    output: output_items,
                    output_text,
                    usage,
                };

                Json(response_obj).into_response()
            }
            Err(error) => pool_error_response(error),
        }
    }
}

/// Converts an upstream chat stream into Responses API SSE events.
async fn handle_responses_streaming(
    state: &AppState,
    provider: &Provider,
    chat_req: ChatCompletionRequest,
    original_model: String,
    session_id: &str,
    identity: Option<&RequestIdentity>,
) -> Response {
    let upstream_resp = match pooled_stream(state, provider, &chat_req, session_id, identity).await
    {
        Ok(response) => response,
        Err(error) => return pool_error_response(error),
    };

    let resp_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());

    let serializer = ResponsesStreamSerializer {
        resp_id,
        msg_id,
        created,
        original_model,
        message_in_progress: false,
        message_index: None,
        next_output_index: 0,
        full_text: String::new(),
        provider_parts: Vec::new(),
        usage: ResponseUsage::default(),
        output_items: Vec::new(),
        incomplete: false,
    };

    crate::provider::antigravity::sse::spawn_sse(serializer, upstream_resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_media_returns_bad_request() {
        let message: crate::openai::ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "user",
            "content": [{"type": "video_url", "video_url": {"url": "https://example.com/video.mp4"}}]
        })).expect("valid chat message");
        let response = validate_request_media(&[message]).expect("external URL must be rejected");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn pool_retries_only_upstream_auth_and_rate_limit_statuses() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = calls.clone();
            let result = try_candidates(vec!["first", "second"], move |id| {
                let seen = seen.clone();
                async move {
                    seen.lock().expect("lock").push(id);
                    if id == "first" {
                        Err(PoolError::Upstream(UpstreamError {
                            status: Some(status),
                            message: "upstream".into(),
                        }))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;
            assert!(result.is_ok());
            assert_eq!(*calls.lock().expect("lock"), vec!["first", "second"]);
        }
    }

    #[tokio::test]
    async fn pool_stops_on_non_retryable_and_after_exhaustion() {
        for status in [
            None,
            Some(StatusCode::BAD_REQUEST),
            Some(StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = calls.clone();
            let result: Result<(), _> = try_candidates(vec!["first", "second"], move |_| {
                seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move {
                    Err(PoolError::Upstream(UpstreamError {
                        status,
                        message: "failed".into(),
                    }))
                }
            })
            .await;
            assert!(result.is_err());
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        }
        let result: Result<(), _> = try_candidates(vec!["first", "second"], |_| async {
            Err(PoolError::Upstream(UpstreamError {
                status: Some(StatusCode::TOO_MANY_REQUESTS),
                message: "last failure".into(),
            }))
        })
        .await;
        assert!(
            matches!(result, Err(PoolError::Upstream(error)) if error.status == Some(StatusCode::TOO_MANY_REQUESTS))
        );
        let empty: Result<(), _> = try_candidates(Vec::<&str>::new(), |_| async { Ok(()) }).await;
        assert!(matches!(empty, Err(PoolError::Internal(_))));
        let internal: Result<(), _> = try_candidates(vec!["first", "second"], |_| async {
            Err(PoolError::Internal("credential error".into()))
        })
        .await;
        assert!(matches!(internal, Err(PoolError::Internal(_))));
    }

    fn models(ids: &[&str]) -> Vec<(String, AntigravityModelInfo)> {
        ids.iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    AntigravityModelInfo {
                        display_name: Some("Model (High)".into()),
                        supports_thinking: Some(true),
                        max_tokens: Some(1_000_000),
                        max_output_tokens: Some(128_000),
                        disabled: None,
                        extra: std::collections::HashMap::from([(
                            "metadata".into(),
                            serde_json::json!(42),
                        )]),
                    },
                )
            })
            .collect()
    }

    fn permissions(ids: &[&str]) -> Permissions {
        Permissions {
            providers: vec![],
            models: ids.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn grouped_discovery_and_permissions_match_routing() {
        let ids = ["model-high", "model-low", "model-medium", "unrelated-low"];
        let groups = normalize_models(models(&ids));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].id, "model");
        assert_eq!(groups[1].id, "unrelated-low");
        let (entry, raw) = groups[0]
            .entry(&permissions(&["model-low"]))
            .expect("visible");
        assert_eq!(entry.name.as_deref(), Some("Model"));
        assert_eq!(entry.display_name.as_deref(), Some("Model"));
        assert_eq!(entry.id, "model");
        assert_eq!(entry.context_window, Some(1_000_000));
        assert_eq!(entry.max_output_tokens, Some(128_000));
        assert_eq!(raw.extra["metadata"], 42);
        assert_eq!(entry.supported_reasoning_efforts, Some(vec!["low".into()]));
        assert_eq!(entry.default_reasoning_effort.as_deref(), Some("low"));
        let all = groups[0]
            .entry(&permissions(&["model"]))
            .expect("visible")
            .0;
        assert_eq!(
            all.supported_reasoning_efforts,
            Some(vec!["low".into(), "medium".into(), "high".into()])
        );
        assert_eq!(all.default_reasoning_effort.as_deref(), Some("medium"));
        let reversed = normalize_models(models(&[
            "unrelated-low",
            "model-medium",
            "model-low",
            "model-high",
        ]));
        let reordered = reversed[0]
            .entry(&permissions(&["model"]))
            .expect("visible")
            .0;
        assert_eq!(
            serde_json::to_value(all).expect("serialize"),
            serde_json::to_value(reordered).expect("serialize")
        );
        assert!(groups[0].entry(&permissions(&["unrelated-low"])).is_none());
        assert_eq!(
            resolve_model(models(&ids), "model", None, &permissions(&["model-low"])),
            Ok("model-low".into())
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model",
                Some("high"),
                &permissions(&["model-low"])
            ),
            Err(ResolveError::Forbidden)
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model-high",
                Some("low"),
                &permissions(&["model-low"])
            ),
            Err(ResolveError::Forbidden)
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model",
                Some("high"),
                &permissions(&["model"])
            ),
            Ok("model-high".into())
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model",
                Some("xhigh"),
                &permissions(&["model"])
            ),
            Ok("model-high".into())
        );
        assert_eq!(
            resolve_model(models(&ids), "model", None, &permissions(&["model"])),
            Ok("model-medium".into())
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model",
                Some("invalid"),
                &permissions(&["model"])
            ),
            Err(ResolveError::Unsupported)
        );
    }

    #[test]
    fn defaults_are_discovered_and_ambiguous_suffixes_stay_separate() {
        let ids = ["model-low", "model-high", "model", "other", "other-high"];
        let groups = normalize_models(models(&ids));
        assert_eq!(
            groups
                .iter()
                .map(|group| group.id.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "other", "other-high"]
        );
        assert_eq!(
            resolve_model(models(&ids), "model", None, &permissions(&["model"])),
            Ok("model".into())
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "model",
                Some("medium"),
                &permissions(&["model"])
            ),
            Err(ResolveError::Unsupported)
        );
        assert_eq!(
            resolve_model(models(&ids), "model", None, &permissions(&["model-high"])),
            Ok("model-high".into())
        );
        assert_eq!(
            resolve_model(
                models(&ids),
                "other-high",
                Some("low"),
                &permissions(&["other-high"])
            ),
            Ok("other-high".into())
        );
        let entry = groups[2].entry(&permissions(&[])).expect("visible").0;
        assert_eq!(entry.supported_reasoning_efforts, Some(vec!["high".into()]));
        assert_eq!(entry.default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(entry.display_name.as_deref(), Some("Model (High)"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn chat_route_preserves_signatures_and_applies_discovered_profile() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use axum::body::Body;
        use axum::http::Request;
        use serde_json::json;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tower::ServiceExt;

        let discovery_count = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let count = discovery_count.clone();
        let fake = Router::new().fallback(axum::routing::post(move |uri: axum::http::Uri, Json(body): Json<Value>| {
            let tx = tx.clone();
            let count = count.clone();
            async move {
                if uri.path().ends_with("fetchAvailableModels") {
                    count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(body["project"], "project");
                    Json(json!({"models": {
                        "model-low": {"supportsThinking": true, "thinkingLevel": 1, "maxOutputTokens": 100},
                        "model-high": {"supportsThinking": true, "thinkingLevel": 3, "maxOutputTokens": 200}
                    }})).into_response()
                } else {
                    if let Some(text) = body.pointer("/request/contents/0/parts/0/text").and_then(Value::as_str) {
                        let usage = match text {
                            "missing-total" => Some(json!({"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":3})),
                            "missing-prompt" => Some(json!({"candidatesTokenCount":5,"thoughtsTokenCount":3,"totalTokenCount":18})),
                            "inconsistent" => Some(json!({"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":3,"totalTokenCount":6})),
                            _ => None,
                        };
                        if let Some(usage) = usage {
                            let event = json!({"response":{"candidates":[{"content":{"parts":[{"text":"answer"}]},"finishReason":"STOP"}],"usageMetadata":usage}});
                            return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], format!("data: {event}\n\n")).into_response();
                        }
                    }
                    let truncated = body.pointer("/request/contents/0/parts/0/text").and_then(Value::as_str) == Some("truncate");
                    tx.send(body).unwrap();
                    if truncated {
                        return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n").into_response();
                    }
                    ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        concat!("data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[",
                        "{\"text\":\"reason\",\"thought\":true,\"thoughtSignature\":\"reason-sig\"},",
                        "{\"text\":\"answer\",\"thoughtSignature\":\"text-sig\"},",
                        "{\"functionCall\":{\"id\":\"call_1\",\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},",
                        "\"thoughtSignature\":\"opaque\"}]},\"finishReason\":\"STOP\"}],",
                        "\"usageMetadata\":{\"promptTokenCount\":10,\"cachedContentTokenCount\":4,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":3,\"totalTokenCount\":18}}}\n\n")).into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "agy".to_string(),
            name: "agy".to_string(),
            kind: "agy".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "credential".to_string(),
            provider_id: "agy".to_string(),
            name: "credential".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        let name = "credential.credential.credential";
        let credential = json!({"access_token":"fake-token","project_id":"project"}).to_string();
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, &credential).unwrap())
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        let mut client = AntigravityClient::new();
        client.test_endpoint = Some(format!("http://{address}"));
        let mut providers = crate::provider::Providers::new();
        providers.client = Arc::new(client);
        let state = AppState {
            providers,
            settings: store,
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_handler))
            .route("/v1/responses", post(responses_handler))
            .layer(Extension(Permissions {
                providers: vec![],
                models: vec![],
            }))
            .layer(Extension(Provider {
                id: "agy".into(),
                name: "agy".into(),
                kind: "agy".into(),
                base_url: String::new(),
            }))
            .layer(Extension(RequestSession {
                id: "scoped-session".into(),
            }))
            .with_state(state);
        let request = |body: Value| {
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let identity = RequestIdentity::default();
        let first = app
            .clone()
            .layer(Extension(identity.clone()))
            .oneshot(request(json!({
                "model":"model", "reasoning_effort":"high", "max_tokens":300,
                "messages":[{"role":"user","content":"hello"}],
                "tools":[{"type":"function","function":{"name":"lookup"}}],
                "tool_choice":{"type":"function","function":{"name":"lookup"}}
            })))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let snapshot = identity.snapshot();
        assert_eq!(snapshot.credential_id.as_deref(), Some("credential"));
        assert_eq!(snapshot.credential_name.as_deref(), Some("credential"));
        let response: Value = serde_json::from_slice(
            &axum::body::to_bytes(first.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["usage"]["prompt_tokens"], 10);
        assert_eq!(response["usage"]["completion_tokens"], 8);
        assert_eq!(response["usage"]["total_tokens"], 18);
        for (text, prompt, total) in [
            ("missing-total", 10, 18),
            ("missing-prompt", 10, 18),
            ("inconsistent", 10, 6),
        ] {
            let result = app
                .clone()
                .oneshot(request(json!({
                    "model":"model-high", "messages":[{"role":"user","content":text}]
                })))
                .await
                .unwrap();
            assert_eq!(result.status(), StatusCode::OK);
            let result: Value = serde_json::from_slice(
                &axum::body::to_bytes(result.into_body(), 1024 * 1024)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["usage"]["prompt_tokens"], prompt);
            assert_eq!(result["usage"]["completion_tokens"], 8);
            assert_eq!(result["usage"]["total_tokens"], total);
        }
        let call = response["choices"][0]["message"]["tool_calls"][0].clone();
        let provider_parts = response["choices"][0]["message"]["provider_parts"].clone();
        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            "reason"
        );
        assert_eq!(response["choices"][0]["message"]["content"], "answer");
        assert!(
            provider_parts[0]["thought_signature"]
                .as_str()
                .unwrap()
                .contains("reason-sig")
        );
        assert!(
            provider_parts[1]["thought_signature"]
                .as_str()
                .unwrap()
                .contains("text-sig")
        );
        assert!(
            call["thought_signature"]
                .as_str()
                .unwrap()
                .contains("opaque")
        );
        let payload = rx.recv().await.unwrap();
        assert_eq!(payload["model"], "model-high");
        assert_eq!(payload["request"]["sessionId"], "scoped-session");
        assert_eq!(
            payload["request"]["generationConfig"]["maxOutputTokens"],
            200
        );
        assert_eq!(
            payload["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "HIGH"
        );
        assert_eq!(
            payload["request"]["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!(["lookup"])
        );
        let stream_identity = RequestIdentity::default();
        let streamed = app
            .clone()
            .layer(Extension(stream_identity.clone()))
            .oneshot(request(json!({
                "model":"model-high", "stream":true, "stream_options":{"include_usage":true},
                "messages":[{"role":"user","content":"hello"}]
            })))
            .await
            .unwrap();
        assert_eq!(streamed.status(), StatusCode::OK);
        let snapshot = stream_identity.snapshot();
        assert_eq!(snapshot.credential_id.as_deref(), Some("credential"));
        assert_eq!(snapshot.credential_name.as_deref(), Some("credential"));
        let bytes = axum::body::to_bytes(streamed.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        let usage = output
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .find_map(|chunk| {
                chunk
                    .get("usage")
                    .filter(|usage| usage.is_object())
                    .cloned()
            })
            .unwrap();
        assert_eq!(usage["prompt_tokens"], 10);
        assert_eq!(usage["completion_tokens"], 8);
        assert_eq!(usage["total_tokens"], 18);
        let stream_parts: Vec<Value> = output
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .filter_map(|chunk| {
                chunk
                    .pointer("/choices/0/delta/provider_parts")
                    .and_then(Value::as_array)
                    .cloned()
            })
            .flatten()
            .collect();
        assert_eq!(Value::Array(stream_parts), provider_parts);
        let _ = rx.recv().await.unwrap();
        let second = app
            .clone()
            .oneshot(request(json!({
                "model":"model", "reasoning_effort":"high",
                "messages":[{"role":"assistant","content":"answer","provider_parts":provider_parts,
                    "tool_calls":[{"id":"unsigned","type":"function","function":{"name":"lookup","arguments":"{}"}},call]}],
                "tools":[{"type":"function","function":{"name":"lookup"}}], "tool_choice":"none"
            })))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let payload = rx.recv().await.unwrap();
        let parts = &payload["request"]["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "reason");
        assert_eq!(parts[0]["thoughtSignature"], "reason-sig");
        assert_eq!(parts[1]["text"], "answer");
        assert_eq!(parts[1]["thoughtSignature"], "text-sig");
        assert_eq!(parts[2]["functionCall"]["id"], "unsigned");
        assert_eq!(
            parts[2]["thoughtSignature"],
            "skip_thought_signature_validator"
        );
        assert_eq!(parts[3]["functionCall"]["id"], "call_1");
        assert_eq!(parts[3]["thoughtSignature"], "opaque");
        assert_eq!(
            payload["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "NONE"
        );
        let parallel = app
            .clone()
            .oneshot(request(json!({
                "model":"model-high",
                "messages":[
                    {"role":"assistant","tool_calls":[
                        {"id":"first","type":"function","function":{"name":"one","arguments":"{}"}},
                        {"id":"second","type":"function","function":{"name":"two","arguments":"{}"}}
                    ]},
                    {"role":"tool","tool_call_id":"second","content":"second result"},
                    {"role":"tool","tool_call_id":"unknown","content":"unknown result"},
                    {"role":"tool","tool_call_id":"first","content":"first result"}
                ]
            })))
            .await
            .unwrap();
        assert_eq!(parallel.status(), StatusCode::OK);
        let payload = rx.recv().await.unwrap();
        let parts = &payload["request"]["contents"][1]["parts"];
        assert_eq!(parts[0]["functionResponse"]["id"], "first");
        assert_eq!(parts[0]["functionResponse"]["name"], "one");
        assert_eq!(
            parts[0]["functionResponse"]["response"]["output"],
            "first result"
        );
        assert_eq!(parts[1]["functionResponse"]["id"], "unknown");
        assert_eq!(parts[2]["functionResponse"]["id"], "second");
        assert_eq!(parts[2]["functionResponse"]["name"], "two");
        assert_eq!(
            parts[2]["functionResponse"]["response"]["output"],
            "second result"
        );
        let response_request = |body: Value| {
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(response_request(json!({
                "model":"model-high", "input":"hello"
            })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let message = value["output"][0].clone();
        assert_eq!(message["provider_parts"], provider_parts);
        let _ = rx.recv().await.unwrap();
        let streamed_response = app
            .clone()
            .oneshot(response_request(json!({
                "model":"model-high", "input":"hello", "stream":true
            })))
            .await
            .unwrap();
        assert_eq!(streamed_response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(streamed_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        let events: Vec<Value> = output
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert!(events.iter().any(
            |event| event["type"] == "response.output_text.delta" && event["delta"] == "answer"
        ));
        let arguments = events
            .iter()
            .find(|event| event["type"] == "response.function_call_arguments.done")
            .unwrap();
        assert_eq!(arguments["name"], "lookup");
        assert_eq!(
            serde_json::from_str::<Value>(arguments["arguments"].as_str().unwrap()).unwrap(),
            json!({"q":"x"})
        );
        let completed = events
            .iter()
            .find(|event| event["type"] == "response.completed")
            .unwrap();
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["output_text"], "answer");
        assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 8);
        assert_eq!(completed["response"]["usage"]["total_tokens"], 18);
        let tool = completed["response"]["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap();
        assert_eq!(tool["call_id"], "call_1");
        assert_eq!(tool["name"], "lookup");
        assert_eq!(
            serde_json::from_str::<Value>(tool["arguments"].as_str().unwrap()).unwrap(),
            json!({"q":"x"})
        );
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "response.failed")
        );
        let _ = rx.recv().await.unwrap();
        let truncated_response = app
            .clone()
            .oneshot(response_request(json!({
                "model":"model-high", "input":"truncate", "stream":true
            })))
            .await
            .unwrap();
        assert_eq!(truncated_response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(truncated_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        let events: Vec<Value> = output
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "response.output_text.delta"
                    && event["delta"] == "partial")
        );
        let failed = events
            .iter()
            .find(|event| event["type"] == "response.failed")
            .unwrap();
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["code"], "bad_gateway");
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "response.completed")
        );
        let _ = rx.recv().await.unwrap();
        let replay = app
            .clone()
            .oneshot(response_request(json!({
                "model":"model-high", "input":[message]
            })))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        let payload = rx.recv().await.unwrap();
        assert_eq!(
            payload["request"]["contents"][0]["parts"][0]["thoughtSignature"],
            "reason-sig"
        );
        assert_eq!(
            payload["request"]["contents"][0]["parts"][1]["thoughtSignature"],
            "text-sig"
        );
        for streaming in [false, true] {
            let response = app
                .clone()
                .oneshot(request(json!({
                    "model":"model-high", "stream":streaming,
                    "messages":[{"role":"user","content":"truncate"}]
                })))
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let output = String::from_utf8(bytes.to_vec()).unwrap();
            if streaming {
                assert_eq!(status, StatusCode::OK);
                assert!(output.contains("upstream_error"));
                assert!(!output.contains("[DONE]"));
            } else {
                assert_eq!(status, StatusCode::BAD_GATEWAY);
                assert!(output.contains("upstream_error"));
            }
            let _ = rx.recv().await.unwrap();
        }
        assert_eq!(discovery_count.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
