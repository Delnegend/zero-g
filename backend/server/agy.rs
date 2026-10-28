use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use crate::antigravity::AntigravityClient;
use crate::antigravity::AntigravityModelInfo;
use crate::antigravity::chat::ChatStreamSerializer;
use crate::antigravity::responses::ResponsesStreamSerializer;
use crate::openai::{ChatCompletionRequest, ModelEntry, ModelListResponse, StreamOptions};
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;
use crate::server::settings::Permissions;

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

pub(crate) async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
) -> Result<Json<ModelListResponse>, (StatusCode, Json<Value>)> {
    let auth_guard = state.auth.read().await;
    let Some(auth) = auth_guard.as_ref() else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Authenticate on the dashboard Providers page.",
                "server_error",
                Some("antigravity_not_configured"),
            )),
        ));
    };

    let creds = auth.get_credentials().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                &e,
                "server_error",
                Some("internal_error"),
            )),
        )
    })?;
    drop(auth_guard);

    let live_models = state.client.fetch_models(&creds).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(crate::server::error_body(
                &e,
                "upstream_error",
                Some("bad_gateway"),
            )),
        )
    })?;

    let data = live_models
        .into_iter()
        .filter(|(id, _)| permissions.allows_model(id))
        .map(|(id, info)| to_model_entry(id, info))
        .collect();
    Ok(Json(ModelListResponse {
        object: "list".to_string(),
        data,
    }))
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

    let supported_efforts = if let Some(e) = &effort_tier {
        Some(vec![e.clone()])
    } else if reasoning {
        Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ])
    } else {
        None
    };

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

pub(crate) fn map_agy_model_effort(model: &str, effort: Option<&str>) -> String {
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

pub(crate) async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
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

    let auth_guard = state.auth.read().await;
    let Some(auth) = auth_guard.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Authenticate on the dashboard Providers page.",
                "server_error",
                Some("antigravity_not_configured"),
            )),
        )
            .into_response();
    };

    let creds = match auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &e,
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };
    drop(auth_guard);

    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        handle_chat_streaming(state, creds, req).await
    } else {
        match state.client.complete_non_streaming(&creds, &req).await {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => {
                tracing::error!("Upstream request failed: {e}");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(crate::server::error_body(
                        "Upstream request failed",
                        "upstream_error",
                        Some("bad_gateway"),
                    )),
                )
                    .into_response()
            }
        }
    }
}

async fn handle_chat_streaming(
    state: AppState,
    creds: crate::antigravity::Credentials,
    req: ChatCompletionRequest,
) -> Response {
    let wire_model = req.model.clone();
    let is_claude = wire_model.contains("claude");
    let payload = AntigravityClient::build_request_payload(&req, &creds.project_id, &wire_model);

    let upstream_resp = match state
        .client
        .stream_request(&creds, &payload, is_claude)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Upstream request failed: {e}");
            return crate::server::sse::bad_gateway_response();
        }
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

    crate::server::sse::spawn_sse(serializer, upstream_resp)
}

pub(crate) async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
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

    let auth_guard = state.auth.read().await;
    let Some(auth) = auth_guard.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Authenticate on the dashboard Providers page.",
                "server_error",
                Some("antigravity_not_configured"),
            )),
        )
            .into_response();
    };

    let creds = match auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &e,
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };
    drop(auth_guard);

    let is_streaming = req.stream.unwrap_or(false);
    let original_model = req.model.clone();
    let chat_req = req.into_chat_request();

    if is_streaming {
        handle_responses_streaming(state, creds, chat_req, original_model).await
    } else {
        match state.client.complete_non_streaming(&creds, &chat_req).await {
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
            Err(e) => {
                tracing::error!("Upstream request failed: {e}");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(crate::server::error_body(
                        "Upstream request failed",
                        "upstream_error",
                        Some("bad_gateway"),
                    )),
                )
                    .into_response()
            }
        }
    }
}

async fn handle_responses_streaming(
    state: AppState,
    creds: crate::antigravity::Credentials,
    chat_req: ChatCompletionRequest,
    original_model: String,
) -> Response {
    let wire_model = chat_req.model.clone();
    let is_claude = wire_model.contains("claude");
    let payload =
        AntigravityClient::build_request_payload(&chat_req, &creds.project_id, &wire_model);

    let upstream_resp = match state
        .client
        .stream_request(&creds, &payload, is_claude)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Upstream request failed: {e}");
            return crate::server::sse::bad_gateway_response();
        }
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

    crate::server::sse::spawn_sse(serializer, upstream_resp)
}

#[cfg(test)]
mod tests {
    use super::*;

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
