use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::Value;

use crate::openai::{ChatCompletionRequest, ModelListResponse, StreamOptions};
use crate::provider::antigravity::chat::ChatStreamSerializer;
use crate::provider::antigravity::models::normalize_models;
use crate::provider::antigravity::responses::ResponsesStreamSerializer;
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::session::RequestSession;
use crate::provider::registry::Provider;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};
mod model_routing;
mod pool;

#[cfg(test)]
use model_routing::resolve_model;
use model_routing::resolve_request_model;
pub(crate) use model_routing::to_model_entry;
pub(crate) use pool::fetch_models;
#[cfg(test)]
use pool::try_candidates;
use pool::{pool_error_response, pool_models, pooled_stream, with_pool};

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
mod tests;
