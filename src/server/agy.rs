use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use crate::antigravity::AntigravityClient;
use crate::antigravity::chat::ChatStreamSerializer;
use crate::antigravity::responses::ResponsesStreamSerializer;
use crate::openai::{ChatCompletionRequest, ModelEntry, ModelListResponse, StreamOptions};
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;

pub(crate) async fn models_handler(
    State(state): State<AppState>,
) -> Result<Json<ModelListResponse>, (StatusCode, Json<Value>)> {
    let Some(auth) = &state.auth else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Run `zero-g login agy` to authenticate.",
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
        .map(|(id, _)| ModelEntry {
            id,
            object: "model".to_string(),
            created: 0,
            owned_by: "google-antigravity".to_string(),
        })
        .collect();

    Ok(Json(ModelListResponse {
        object: "list".to_string(),
        data,
    }))
}

pub(crate) async fn chat_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let Some(auth) = &state.auth else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Run `zero-g login agy` to authenticate.",
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
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    let Some(auth) = &state.auth else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "Google Antigravity is not configured. Run `zero-g login agy` to authenticate.",
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
