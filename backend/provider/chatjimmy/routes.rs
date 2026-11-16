use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use super::downstream;
use super::protocol::{ChatJimmyMessage, ChatJimmyOptions, ChatJimmyRequest};
use super::{DEFAULT_MODEL, models::discover_models};
use crate::openai::{ChatCompletionRequest, MessageContent};
use crate::provider::common::proxy::{check_model, error, filter_models};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::registry;
use crate::server::{AppState, settings::Permissions};

/// Returns the models list filtered by permissions.
pub(super) async fn models_handler(Extension(permissions): Extension<Permissions>) -> Response {
    let models = discover_models().await.unwrap_or_default();
    let mut value = serde_json::json!({
        "object": "list",
        "data": models,
    });
    if filter_models(&mut value, &permissions) {
        Json(value).into_response()
    } else {
        (
            StatusCode::BAD_GATEWAY,
            Json(crate::server::error_body(
                "Invalid ChatJimmy model list",
                "upstream_error",
                Some("bad_gateway"),
            )),
        )
            .into_response()
    }
}

/// Handles `OpenAI` chat completions translated to `ChatJimmy`.
#[allow(clippy::too_many_lines)]
pub(super) async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<registry::Provider>,
    identity: Option<Extension<RequestIdentity>>,
    body: Bytes,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(crate::server::error_body(
                    &format!("Invalid request: {e}"),
                    "invalid_request_error",
                    Some("invalid_payload"),
                )),
            )
                .into_response();
        }
    };
    if req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        || req
            .tool_choice
            .as_ref()
            .is_some_and(|choice| !choice.is_null() && choice != "none")
        || req.messages.iter().any(|message| {
            message.role == "tool"
                || message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
        })
    {
        return error(
            StatusCode::BAD_REQUEST,
            "ChatJimmy does not support tool calls",
            "invalid_request_error",
            "unsupported_tools",
        )
        .into_response();
    }

    let mut system_prompts = Vec::new();
    let mut chat_messages = Vec::new();
    for msg in &req.messages {
        let text = msg.content.as_ref().map_or_default(MessageContent::as_text);
        if msg.role == "system" || msg.role == "developer" {
            system_prompts.push(text);
        } else {
            chat_messages.push(ChatJimmyMessage {
                role: msg.role.clone(),
                content: text,
            });
        }
    }
    let selected_model = if req.model.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        req.model.clone()
    };
    let payload = ChatJimmyRequest {
        messages: chat_messages,
        chat_options: ChatJimmyOptions {
            selected_model,
            system_prompt: system_prompts.join("\n\n"),
            top_k: 8,
        },
        attachment: None,
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &format!("Failed to serialize request: {e}"),
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };

    downstream::respond(state, provider, identity, req, payload_bytes).await
}
