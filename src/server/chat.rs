use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::antigravity::AntigravityClient;
use crate::openai::{
    ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, ChunkDelta, ChunkFunctionCall,
    ChunkToolCall,
};
use crate::server::{AppState, check_auth};

pub(crate) async fn chat_completions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    if !check_auth(&headers, state.api_key.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "message": "Invalid API key" } })),
        )
            .into_response();
    }

    let creds = match state.auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "message": e } })),
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
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": { "message": e } })),
            )
                .into_response(),
        }
    }
}

async fn handle_streaming(
    state: AppState,
    creds: crate::auth::Credentials,
    req: ChatCompletionRequest,
) -> Response {
    let wire_model = AntigravityClient::map_model_id(&req.model);
    let is_claude = wire_model.contains("claude");
    let payload = AntigravityClient::build_request_payload(&req, &creds.project_id, &wire_model);

    let upstream_resp = match state
        .client
        .stream_request(&creds, &payload, is_claude)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": { "message": e } })),
            )
                .into_response();
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(100);

    let req_model = req.model.clone();
    let stream_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    tokio::spawn(async move {
        let mut byte_stream = upstream_resp.bytes_stream();
        let mut buffer = String::new();
        let mut sent_role = false;

        while let Some(chunk_res) = byte_stream.next().await {
            let bytes = match chunk_res {
                Ok(b) => b,
                Err(e) => {
                    error!("Error reading upstream bytes: {e}");
                    break;
                }
            };
            let chunk_str = String::from_utf8_lossy(&bytes);
            buffer.push_str(&chunk_str);

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim_end().to_string();
                buffer.drain(..=line_end);

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        continue;
                    }
                    if let Ok(parsed) = serde_json::from_str::<Value>(data)
                        && let Some(resp) = parsed.get("response")
                    {
                        if let Some(candidates) = resp.get("candidates").and_then(|c| c.as_array())
                            && let Some(cand) = candidates.first()
                            && let Some(parts) = cand
                                .get("content")
                                .and_then(|c| c.get("parts"))
                                .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    let is_thought = part
                                        .get("thought")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false);

                                    let delta = if is_thought {
                                        ChunkDelta {
                                            role: if sent_role {
                                                None
                                            } else {
                                                sent_role = true;
                                                Some("assistant".to_string())
                                            },
                                            reasoning_content: Some(text.to_string()),
                                            content: None,
                                            tool_calls: None,
                                        }
                                    } else {
                                        ChunkDelta {
                                            role: if sent_role {
                                                None
                                            } else {
                                                sent_role = true;
                                                Some("assistant".to_string())
                                            },
                                            content: Some(text.to_string()),
                                            reasoning_content: None,
                                            tool_calls: None,
                                        }
                                    };

                                    let chunk = ChatCompletionChunk {
                                        id: stream_id.clone(),
                                        object: "chat.completion.chunk".to_string(),
                                        created,
                                        model: req_model.clone(),
                                        choices: vec![ChunkChoice {
                                            index: 0,
                                            delta,
                                            finish_reason: None,
                                        }],
                                        usage: None,
                                    };

                                    if let Ok(json_str) = serde_json::to_string(&chunk) {
                                        let _ = tx.send(Ok(Event::default().data(json_str))).await;
                                    }
                                }

                                if let Some(fc) = part.get("functionCall") {
                                    let name = fc.get("name").and_then(|n| n.as_str());
                                    let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                                    let id = fc.get("id").and_then(|i| i.as_str()).map_or_else(
                                        || format!("call_{}", uuid::Uuid::new_v4().simple()),
                                        std::string::ToString::to_string,
                                    );

                                    let tool_call_delta = ChunkToolCall {
                                        index: 0,
                                        id: Some(id),
                                        r#type: Some("function".to_string()),
                                        function: Some(ChunkFunctionCall {
                                            name: name.map(std::string::ToString::to_string),
                                            arguments: Some(args.to_string()),
                                        }),
                                    };

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
                                                tool_calls: Some(vec![tool_call_delta]),
                                            },
                                            finish_reason: None,
                                        }],
                                        usage: None,
                                    };

                                    if let Ok(json_str) = serde_json::to_string(&chunk) {
                                        let _ = tx.send(Ok(Event::default().data(json_str))).await;
                                    }
                                }
                            }
                        }

                        if let Some(cand) = resp
                            .get("candidates")
                            .and_then(|c| c.as_array())
                            .and_then(|c| c.first())
                            && let Some(fr) = cand.get("finishReason").and_then(|f| f.as_str())
                        {
                            let finish_reason = match fr {
                                "STOP" => "stop".to_string(),
                                "MAX_TOKENS" => "length".to_string(),
                                "SAFETY" => "content_filter".to_string(),
                                other => other.to_lowercase(),
                            };

                            let chunk = ChatCompletionChunk {
                                id: stream_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created,
                                model: req_model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta::default(),
                                    finish_reason: Some(finish_reason),
                                }],
                                usage: None,
                            };

                            if let Ok(json_str) = serde_json::to_string(&chunk) {
                                let _ = tx.send(Ok(Event::default().data(json_str))).await;
                            }
                        }
                    }
                }
            }
        }

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let sse_stream = ReceiverStream::new(rx);
    Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
