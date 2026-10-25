use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::antigravity::AntigravityEvent;
use crate::antigravity::{map_finish_reason, spawn_event_stream};
use crate::openai::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, ChunkDelta, ChunkFunctionCall,
    ChunkToolCall, StreamOptions,
};
use crate::server::AppState;

pub(crate) async fn chat_completions_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let creds = match state.auth.get_credentials().await {
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
        handle_streaming(state, creds, req).await
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

async fn handle_streaming(
    state: AppState,
    creds: crate::auth::Credentials,
    req: ChatCompletionRequest,
) -> Response {
    let wire_model = crate::antigravity::AntigravityClient::map_model_id(&req.model);
    let is_claude = wire_model.contains("claude");
    let payload = crate::antigravity::AntigravityClient::build_request_payload(
        &req,
        &creds.project_id,
        &wire_model,
    );

    let upstream_resp = match state
        .client
        .stream_request(&creds, &payload, is_claude)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Upstream request failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(crate::server::error_body(
                    "Upstream request failed",
                    "upstream_error",
                    Some("bad_gateway"),
                )),
            )
                .into_response();
        }
    };

    let include_usage = req
        .stream_options
        .as_ref()
        .and_then(|options: &StreamOptions| options.include_usage)
        .unwrap_or(false);

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let req_model = req.model.clone();
    let stream_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());

    tokio::spawn(async move {
        let mut events = spawn_event_stream(upstream_resp.bytes_stream());

        let mut sent_role = false;
        let mut has_tools = false;
        let mut finish_sent = false;
        let mut had_error = false;
        let mut tool_call_index: u32 = 0;
        let mut usage = crate::openai::Usage::default();
        let mut saw_usage = false;
        while let Some(event) = events.recv().await {
            match event {
                Ok(AntigravityEvent::Text(text)) => {
                    let chunk = ChatCompletionChunk {
                        id: stream_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: req_model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: if sent_role {
                                    None
                                } else {
                                    sent_role = true;
                                    Some("assistant".to_string())
                                },
                                content: Some(text),
                                reasoning_content: None,
                                tool_calls: None,
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    send_chunk(&tx, &chunk).await;
                }
                Ok(AntigravityEvent::Reasoning(text)) => {
                    let chunk = ChatCompletionChunk {
                        id: stream_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: req_model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: if sent_role {
                                    None
                                } else {
                                    sent_role = true;
                                    Some("assistant".to_string())
                                },
                                content: None,
                                reasoning_content: Some(text),
                                tool_calls: None,
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    send_chunk(&tx, &chunk).await;
                }
                Ok(AntigravityEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    has_tools = true;
                    let index = tool_call_index;
                    tool_call_index = tool_call_index.saturating_add(1);

                    let chunk = ChatCompletionChunk {
                        id: stream_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: req_model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: if sent_role {
                                    None
                                } else {
                                    sent_role = true;
                                    Some("assistant".to_string())
                                },
                                content: None,
                                reasoning_content: None,
                                tool_calls: Some(vec![ChunkToolCall {
                                    index,
                                    id: Some(id),
                                    r#type: Some("function".to_string()),
                                    function: Some(ChunkFunctionCall {
                                        name: Some(name),
                                        arguments: Some(arguments),
                                    }),
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    send_chunk(&tx, &chunk).await;
                }
                Ok(AntigravityEvent::Finish { reason }) => {
                    finish_sent = true;
                    let chunk = ChatCompletionChunk {
                        id: stream_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: req_model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta::default(),
                            finish_reason: Some(map_finish_reason(&reason, has_tools)),
                        }],
                        usage: None,
                    };
                    send_chunk(&tx, &chunk).await;
                }
                Ok(AntigravityEvent::Usage(value)) => {
                    usage = value;
                    saw_usage = true;
                }
                Err(error) => {
                    error!("Error reading upstream bytes: {error}");
                    had_error = true;
                    break;
                }
            }
        }

        if had_error {
            if let Ok(json_str) = serde_json::to_string(&crate::server::error_body(
                "Upstream request failed",
                "upstream_error",
                Some("bad_gateway"),
            )) {
                let _ = tx.send(Ok(Event::default().data(json_str))).await;
            }
        } else {
            if !finish_sent {
                let chunk = ChatCompletionChunk {
                    id: stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta::default(),
                        finish_reason: Some(if has_tools {
                            "tool_calls".to_string()
                        } else {
                            "stop".to_string()
                        }),
                    }],
                    usage: None,
                };
                send_chunk(&tx, &chunk).await;
            }

            if include_usage && saw_usage {
                let chunk = ChatCompletionChunk {
                    id: stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: req_model.clone(),
                    choices: vec![],
                    usage: Some(usage),
                };
                send_chunk(&tx, &chunk).await;
            }
        }

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let sse_stream = ReceiverStream::new(rx);
    Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn send_chunk(tx: &mpsc::Sender<Result<Event, Infallible>>, chunk: &ChatCompletionChunk) {
    if let Ok(json_str) = serde_json::to_string(chunk) {
        let _ = tx.send(Ok(Event::default().data(json_str))).await;
    }
}
