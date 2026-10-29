use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::Value;

use crate::openai::{ChatCompletionRequest, ModelEntry, ModelListResponse, StreamOptions};
use crate::provider::antigravity::AntigravityModelInfo;
use crate::provider::antigravity::chat::ChatStreamSerializer;
use crate::provider::antigravity::models::{ResolveError, normalize_models};
use crate::provider::antigravity::responses::ResponsesStreamSerializer;
use crate::provider::antigravity::{AntigravityClient, AuthManager};
use crate::provider::common::errors::UpstreamError;
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
async fn try_candidates<T, F, Fut>(ids: Vec<String>, mut attempt: F) -> Result<T, PoolError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    let mut ids = ids.into_iter().peekable();
    while let Some(id) = ids.next() {
        match attempt(id).await {
            Err(PoolError::Upstream(error)) if error.retryable() && ids.peek().is_some() => {}
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
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(crate::provider::antigravity::Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id)
        .await
        .map_err(PoolError::Internal)?;
    let ids = credentials
        .into_iter()
        .map(|credential| credential.id)
        .collect();
    try_candidates(ids, |id| {
        let store = state.settings.clone();
        let provider_id = provider.id.clone();
        let operation = &operation;
        async move {
            let auth = AuthManager::load(store, &provider_id, &id)
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
            operation(creds).await.map_err(PoolError::Upstream)
        }
    })
    .await
}

/// Starts an upstream chat stream using the credential pool.
async fn pooled_stream(
    state: &AppState,
    provider: &Provider,
    req: &ChatCompletionRequest,
) -> Result<reqwest::Response, PoolError> {
    with_pool(state, provider, |creds| async move {
        let payload = AntigravityClient::build_request_payload(req, &creds.project_id, &req.model);
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
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    with_pool(state, provider, |creds| async move {
        state.providers.client.fetch_models(&creds).await
    })
    .await
}

/// Fetches upstream models, converting pool failures to strings.
pub(crate) async fn fetch_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
    pool_models(state, provider)
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
) -> Response {
    let live_models = match pool_models(&state, &provider).await {
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
pub(crate) fn to_model_entry(id: String, info: AntigravityModelInfo) -> ModelEntry {
    let reasoning = info.supports_thinking.unwrap_or(false);
    let effort_tier = [("-low", "low"), ("-medium", "medium"), ("-high", "high")]
        .into_iter()
        .find(|(suffix, _)| id.ends_with(suffix))
        .map(|(_, effort)| effort.to_string());
    let supported_efforts = effort_tier.as_ref().map_or_else(
        || reasoning.then(|| vec!["low".to_string(), "medium".to_string(), "high".to_string()]),
        |effort| Some(vec![effort.clone()]),
    );

    ModelEntry {
        id,
        object: "model".to_string(),
        created: 1_700_000_000,
        owned_by: "google-antigravity".to_string(),
        name: info.display_name.clone(),
        display_name: info.display_name,
        reasoning: if reasoning { Some(true) } else { None },
        default_reasoning_effort: effort_tier,
        supported_reasoning_efforts: supported_efforts,
        context_window: info.max_tokens,
        max_output_tokens: info.max_output_tokens,
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
    let models = pool_models(state, provider)
        .await
        .map_err(RequestModelError::Pool)?;
    resolve_model(models, model, effort, permissions).map_err(RequestModelError::Resolve)
}

/// Handles streaming and non-streaming chat completion requests.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
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
        handle_chat_streaming(&state, &provider, req).await
    } else {
        match with_pool(&state, &provider, |creds| {
            let req = &req;
            let client = &state.providers.client;
            async move { client.complete_non_streaming(&creds, req).await }
        })
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
) -> Response {
    let upstream_resp = match pooled_stream(state, provider, &req).await {
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
    Json(mut req): Json<ResponseCreateRequest>,
) -> Response {
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

    let original_model = req.model.clone();
    let chat_req = req.into_chat_request();

    if is_streaming {
        handle_responses_streaming(&state, &provider, chat_req, original_model).await
    } else {
        match with_pool(&state, &provider, |creds| {
            let chat_req = &chat_req;
            let client = &state.providers.client;
            async move { client.complete_non_streaming(&creds, chat_req).await }
        })
        .await
        {
            Ok(resp) => {
                let mut output_items = Vec::new();
                let mut output_text = String::new();

                for choice in &resp.choices {
                    if let Some(content) = &choice.message.content {
                        output_text.push_str(content);
                        output_items.push(ResponseOutputItem::Message {
                            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                            status: "completed".to_string(),
                            role: choice.message.role.clone(),
                            content: vec![ResponseContentPart::OutputText {
                                text: content.clone(),
                            }],
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
                    status: "completed".to_string(),
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
) -> Response {
    let upstream_resp = match pooled_stream(state, provider, &chat_req).await {
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
        usage: ResponseUsage::default(),
        output_items: Vec::new(),
    };

    crate::provider::antigravity::sse::spawn_sse(serializer, upstream_resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_retries_only_upstream_auth_and_rate_limit_statuses() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = calls.clone();
            let result = try_candidates(vec!["first".into(), "second".into()], move |id| {
                let seen = seen.clone();
                async move {
                    seen.lock().expect("lock").push(id.clone());
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
            let result: Result<(), _> =
                try_candidates(vec!["first".into(), "second".into()], move |_| {
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
        let result: Result<(), _> =
            try_candidates(vec!["first".into(), "second".into()], |_| async {
                Err(PoolError::Upstream(UpstreamError {
                    status: Some(StatusCode::TOO_MANY_REQUESTS),
                    message: "last failure".into(),
                }))
            })
            .await;
        assert!(
            matches!(result, Err(PoolError::Upstream(error)) if error.status == Some(StatusCode::TOO_MANY_REQUESTS))
        );
        let empty: Result<(), _> = try_candidates(Vec::new(), |_| async { Ok(()) }).await;
        assert!(matches!(empty, Err(PoolError::Internal(_))));
        let internal: Result<(), _> =
            try_candidates(vec!["first".into(), "second".into()], |_| async {
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
}
