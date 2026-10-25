use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Json, Response};

use crate::antigravity::AntigravityEvent;
use crate::antigravity::map_finish_reason;
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

#[allow(clippy::struct_excessive_bools)]
struct ChatStreamSerializer {
    stream_id: String,
    req_model: String,
    created: u64,
    include_usage: bool,
    sent_role: bool,
    has_tools: bool,
    finish_sent: bool,
    tool_call_index: u32,
    usage: crate::openai::Usage,
    saw_usage: bool,
}

impl crate::server::sse::SseSerializer for ChatStreamSerializer {
    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event> {
        match event {
            AntigravityEvent::Text(text) => {
                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: if self.sent_role {
                                None
                            } else {
                                self.sent_role = true;
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
                vec![event_from_chunk(&chunk)]
            }
            AntigravityEvent::Reasoning(text) => {
                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: if self.sent_role {
                                None
                            } else {
                                self.sent_role = true;
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
                vec![event_from_chunk(&chunk)]
            }
            AntigravityEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                self.has_tools = true;
                let index = self.tool_call_index;
                self.tool_call_index = self.tool_call_index.saturating_add(1);

                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: if self.sent_role {
                                None
                            } else {
                                self.sent_role = true;
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
                vec![event_from_chunk(&chunk)]
            }
            AntigravityEvent::Finish { reason } => {
                self.finish_sent = true;
                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta::default(),
                        finish_reason: Some(map_finish_reason(&reason, self.has_tools)),
                    }],
                    usage: None,
                };
                vec![event_from_chunk(&chunk)]
            }
            AntigravityEvent::Usage(value) => {
                self.usage = value;
                self.saw_usage = true;
                Vec::new()
            }
        }
    }

    fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if !self.finish_sent {
            let chunk = ChatCompletionChunk {
                id: self.stream_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created: self.created,
                model: self.req_model.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta::default(),
                    finish_reason: Some(if self.has_tools {
                        "tool_calls".to_string()
                    } else {
                        "stop".to_string()
                    }),
                }],
                usage: None,
            };
            events.push(event_from_chunk(&chunk));
        }

        if self.include_usage && self.saw_usage {
            let chunk = ChatCompletionChunk {
                id: self.stream_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created: self.created,
                model: self.req_model.clone(),
                choices: vec![],
                usage: Some(self.usage.clone()),
            };
            events.push(event_from_chunk(&chunk));
        }

        events
    }
}

fn event_from_chunk(chunk: &ChatCompletionChunk) -> Event {
    let json_str = serde_json::to_string(chunk).unwrap_or_default();
    Event::default().data(json_str)
}
