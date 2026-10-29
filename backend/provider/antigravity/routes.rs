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
use crate::provider::antigravity::client::UpstreamError;
use crate::provider::antigravity::responses::ResponsesStreamSerializer;
use crate::provider::antigravity::{AntigravityClient, AuthManager};
use crate::provider::registry::Provider;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

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

#[derive(Debug)]
enum PoolError {
    Internal(String),
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

async fn pooled_stream(
    state: &AppState,
    provider: &Provider,
    req: &ChatCompletionRequest,
) -> Result<reqwest::Response, PoolError> {
    with_pool(state, provider, |creds| async move {
        let payload = AntigravityClient::build_request_payload(req, &creds.project_id, &req.model);
        state
            .client
            .stream_request(&creds, &payload, req.model.contains("claude"))
            .await
    })
    .await
}

async fn pool_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, PoolError> {
    with_pool(state, provider, |creds| async move {
        state.client.fetch_models(&creds).await
    })
    .await
}

pub(crate) async fn fetch_models(
    state: &AppState,
    provider: &Provider,
) -> Result<Vec<(String, AntigravityModelInfo)>, String> {
    pool_models(state, provider)
        .await
        .map_err(|error| error.to_string())
}

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

async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
) -> Response {
    let live_models = match pool_models(&state, &provider).await {
        Ok(models) => models,
        Err(error) => return pool_error_response(error),
    };

    let data = live_models
        .into_iter()
        .filter(|(id, _)| permissions.allows_model(id))
        .map(|(id, info)| to_model_entry(id, info))
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
    let effort_tier = if id.ends_with("-low") {
        Some("low".to_string())
    } else if id.ends_with("-medium") {
        Some("medium".to_string())
    } else if id.ends_with("-high") {
        Some("high".to_string())
    } else {
        None
    };

    let supported_efforts = effort_tier.as_ref().map_or_else(
        || reasoning.then(|| vec!["low".to_string(), "medium".to_string(), "high".to_string()]),
        |e| Some(vec![e.clone()]),
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

fn map_agy_model_effort(model: &str, effort: Option<&str>) -> String {
    let Some(effort) = effort else {
        return model.to_string();
    };
    if model.ends_with("-low") || model.ends_with("-medium") || model.ends_with("-high") {
        return model.to_string();
    }
    match effort.to_lowercase().as_str() {
        "low" | "minimal" => format!("{model}-low"),
        "medium" => format!("{model}-medium"),
        "high" | "xhigh" | "max" | "ultra" => format!("{model}-high"),
        _ => model.to_string(),
    }
}

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
    req.model = map_agy_model_effort(&req.model, effort);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }

    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        handle_chat_streaming(&state, &provider, req).await
    } else {
        match with_pool(&state, &provider, |creds| {
            let req = &req;
            let client = &state.client;
            async move { client.complete_non_streaming(&creds, req).await }
        })
        .await
        {
            Ok(resp) => Json(resp).into_response(),
            Err(error) => pool_error_response(error),
        }
    }
}

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
    req.model = map_agy_model_effort(&req.model, effort);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }

    let is_streaming = req.stream.unwrap_or(false);

    let original_model = req.model.clone();
    let chat_req = req.into_chat_request();

    if is_streaming {
        handle_responses_streaming(&state, &provider, chat_req, original_model).await
    } else {
        match with_pool(&state, &provider, |creds| {
            let chat_req = &chat_req;
            let client = &state.client;
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

    #[test]
    fn policy_checks_mapped_effort() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["model-low".into()],
        };
        assert!(permissions.allows_model(&map_agy_model_effort("model", Some("low"))));
        assert!(!permissions.allows_model(&map_agy_model_effort("model", Some("high"))));
        assert!(!permissions.allows_model(&map_agy_model_effort("model-high", Some("low"))));
    }

    #[test]
    fn test_to_model_entry_reports_upstream_context_window() {
        let info = AntigravityModelInfo {
            display_name: Some("Claude Sonnet 5.5 (High)".to_string()),
            supports_thinking: Some(true),
            max_tokens: Some(1_000_000),
            max_output_tokens: Some(128_000),
            disabled: None,
            extra: std::collections::HashMap::new(),
        };
        let entry = to_model_entry("claude-sonnet-5-5-high".to_string(), info);
        assert_eq!(entry.context_window, Some(1_000_000));
        assert_eq!(entry.max_output_tokens, Some(128_000));
        assert_eq!(entry.default_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            entry.supported_reasoning_efforts,
            Some(vec!["high".to_string()])
        );
    }

    #[test]
    fn test_map_agy_model_effort() {
        assert_eq!(
            map_agy_model_effort("gemini-3.8-flash", Some("high")),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            map_agy_model_effort("gemini-3.8-flash", Some("low")),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            map_agy_model_effort("claude-sonnet-5-5-high", Some("low")),
            "claude-sonnet-5-5-high"
        );
        assert_eq!(
            map_agy_model_effort("gemini-2.5-flash", None),
            "gemini-2.5-flash"
        );
    }
}
