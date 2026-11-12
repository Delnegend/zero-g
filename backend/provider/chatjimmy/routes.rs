use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{
    IntoResponse, Json, Response,
    sse::{Event, Sse},
};
use tokio_stream::wrappers::ReceiverStream;

use super::protocol::{
    ChatJimmyMessage, ChatJimmyOptions, ChatJimmyRequest, ChatJimmyStats, parse_upstream,
    usage_from_stats,
};
use super::stream::{chunk_event, split_stream_chunks};
use super::{CHAT_PATH, CLIENT, DEFAULT_BASE, DEFAULT_MODEL, models::discover_models};
use crate::openai::{
    AssistantMessage, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkDelta,
    MessageContent,
};
use crate::provider::common::proxy::{
    ProxyError, check_model, error, filter_models, retry_api_keys,
};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::registry;
use crate::server::{
    AppState,
    settings::{Permissions, SettingsStore},
};

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

/// Sends a request upstream to `ChatJimmy` and parses the response.
async fn fetch_chatjimmy(
    settings: &SettingsStore,
    provider: &registry::Provider,
    payload_bytes: Bytes,
    identity: Option<&RequestIdentity>,
) -> Result<(String, Option<ChatJimmyStats>), ProxyError> {
    let base_url = if provider.base_url.is_empty() {
        DEFAULT_BASE
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!("{base_url}{CHAT_PATH}");

    let response = retry_api_keys(
        settings,
        provider,
        "No connected ChatJimmy credentials",
        identity,
        |_key| {
            let url = url.clone();
            let payload = payload_bytes.clone();
            async move {
                let client = CLIENT
                    .as_ref()
                    .map_err(|_| "Failed to initialize ChatJimmy client")?;
                client
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::USER_AGENT,
                        concat!("zero-g/", env!("CARGO_PKG_VERSION")),
                    )
                    .body(payload)
                    .send()
                    .await
                    .map_err(|_| "Failed to contact ChatJimmy upstream")
            }
        },
    )
    .await?;

    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "ChatJimmy upstream returned error status",
            "upstream_error",
            "bad_gateway",
        ));
    }

    let Ok(raw_text) = response.text().await else {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Failed to read ChatJimmy response",
            "upstream_error",
            "bad_gateway",
        ));
    };

    if raw_text.trim().is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "ChatJimmy returned empty response (input likely exceeded the ~6,000 token limit)",
            "invalid_request_error",
            "context_length_exceeded",
        ));
    }

    Ok(parse_upstream(&raw_text))
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
        let text = msg
            .content
            .as_ref()
            .map(MessageContent::as_text)
            .unwrap_or_default();
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

    let is_stream = req.stream.unwrap_or(false);
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if is_stream {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
        let model = req.model.clone();
        let settings = state.settings.clone();
        let provider_clone = provider.clone();
        let id_for_task = id.clone();
        let identity_ref = identity.map(|Extension(id)| id);

        tokio::spawn(async move {
            let first = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: Some(String::new()),
                    ..Default::default()
                },
                None,
                None,
            );
            if let Some(event) = first
                && tx.send(Ok(event)).await.is_err()
            {
                return;
            }

            let result = fetch_chatjimmy(
                &settings,
                &provider_clone,
                payload_bytes,
                identity_ref.as_ref(),
            )
            .await;

            let (content, token_stats) = match result {
                Ok(data) => data,
                Err(err) => {
                    let err_data = serde_json::json!({
                        "error": {
                            "message": err.message(),
                            "type": err.kind(),
                            "code": err.code()
                        }
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("error")
                            .data(err_data.to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            };

            let usage = token_stats.map(usage_from_stats);

            for chunk_text in split_stream_chunks(&content) {
                let chunk = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta {
                        content: Some(chunk_text),
                        ..Default::default()
                    },
                    None,
                    None,
                );
                if let Some(event) = chunk
                    && tx.send(Ok(event)).await.is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let stop = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta::default(),
                Some("stop"),
                None,
            );
            if let Some(event) = stop {
                let _ = tx.send(Ok(event)).await;
            }

            if let Some(usage) = usage {
                let usage_event = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta::default(),
                    None,
                    Some(usage),
                );
                if let Some(event) = usage_event {
                    let _ = tx.send(Ok(event)).await;
                }
            }

            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        });

        Sse::new(ReceiverStream::new(rx)).into_response()
    } else {
        let (content, token_stats) = match fetch_chatjimmy(
            &state.settings,
            &provider,
            payload_bytes,
            identity.as_ref().map(|Extension(id)| id),
        )
        .await
        {
            Ok(data) => data,
            Err(err) => return err.into_response(),
        };

        let response = ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created: now,
            model: req.model,
            choices: vec![Choice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    reasoning_content: None,
                    tool_calls: None,
                    provider_parts: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: token_stats.map(usage_from_stats),
        };
        Json(response).into_response()
    }
}
