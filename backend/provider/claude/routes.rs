use crate::provider::common::session::RequestSession;
use axum::http::StatusCode;
use axum::{
    Extension, Json, Router,
    extract::State,
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    openai::{ChatCompletionRequest, ModelListResponse},
    provider::{
        claude::{auth, client, models, payload, stream::Decoder},
        registry::Provider,
    },
    responses::ResponseCreateRequest,
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Build virtual-key-authenticated Claude routes.
pub(super) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list))
        .route("/v1/models/", get(list))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/chat/completions/", post(chat))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/", post(responses))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "claude"),
            settings::virtual_key_auth,
        ))
}
/// Construct a gateway error response.
fn failure(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(crate::server::error_body(
            message,
            if status.is_client_error() {
                "invalid_request_error"
            } else {
                "upstream_error"
            },
            None,
        )),
    )
        .into_response()
}
/// Return models visible to the virtual key.
async fn list(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
) -> Response {
    match models::discover(&state, &provider).await {
        Ok(mut models) => {
            models.retain(|m| permissions.allows_model(&m.id));
            Json(ModelListResponse {
                object: "list".into(),
                data: models,
            })
            .into_response()
        }
        Err(e) => failure(StatusCode::BAD_GATEWAY, &e),
    }
}
/// Handle chat completion requests.
async fn chat(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    Extension(session): Extension<RequestSession>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    inference(state, provider, permissions, session, req, false).await
}
/// Handle Responses API requests.
async fn responses(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    Extension(session): Extension<RequestSession>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    if let crate::responses::ResponseInput::Items(items) = &req.input
        && items.iter().any(|item| {
            matches!(
                item,
                crate::responses::ResponseInputItem::Tagged(
                    crate::responses::ResponseTaggedItem::Reasoning { .. }
                )
            )
        })
    {
        return failure(
            StatusCode::BAD_REQUEST,
            "Claude reasoning must be replayed in message provider_parts",
        );
    }
    inference(
        state,
        provider,
        permissions,
        session,
        req.into_chat_request(),
        true,
    )
    .await
}

/// Select a credential and invoke the upstream.
async fn inference(
    state: AppState,
    provider: Provider,
    permissions: Permissions,
    session: RequestSession,
    mut req: ChatCompletionRequest,
    responses: bool,
) -> Response {
    if let Some(model) = req.model.strip_prefix("claude/") {
        req.model = model.to_owned();
    }
    if !permissions.allows_model(&req.model) {
        return failure(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
        );
    }
    let mut payload = match payload::build(&req) {
        Ok(v) => v,
        Err(e) => return failure(StatusCode::BAD_REQUEST, &e),
    };
    let credentials = match crate::provider::credentials::list(&state.settings, &provider.id).await
    {
        Ok(v) => v,
        Err(e) => return failure(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let http = match client::http() {
        Ok(client) => client,
        Err(e) => return failure(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let mut last = "No connected Claude credentials".to_owned();
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;
    for credential in credentials {
        let creds = match auth::load(&state.settings, &provider.id, &credential.id).await {
            Ok(v) => v,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let digest = Sha256::digest(format!("zero-g-claude-device:{}", credential.id).as_bytes());
        let device = client::hex(&digest);
        let mut identity = json!({"device_id":device,"session_id":session.id});
        if let Some(account) = &creds.account_id
            && let Some(object) = identity.as_object_mut()
        {
            object.insert("account_uuid".into(), json!(account));
        }
        if let Some(object) = payload.as_object_mut() {
            object.insert("metadata".into(), json!({"user_id":identity.to_string()}));
        }
        let response = match client::send(
            &http,
            "https://api.anthropic.com/v1/messages?beta=true",
            &creds.access_token,
            &payload,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return failure(StatusCode::BAD_GATEWAY, &e),
        };
        if !response.status().is_success() {
            let status = response.status();
            last_status = if status.is_redirection() {
                StatusCode::BAD_GATEWAY
            } else {
                status
            };
            last = format!("Claude upstream returned {status}");
            if matches!(status.as_u16(), 401 | 403 | 429 | 503) {
                continue;
            }
            return failure(last_status, &last);
        }
        let decoder = Decoder::new(req.model.clone());
        if req.stream.unwrap_or(false) {
            return stream_response(
                response,
                decoder,
                responses,
                req.stream_options
                    .is_some_and(|v| v.include_usage.unwrap_or(false)),
            );
        }
        return match client::consume(response, decoder, None).await {
            Ok(done) if responses => Json(response_object(&done)).into_response(),
            Ok(done) => Json(json!({"id":done.id,"object":"chat.completion","created":done.created,"model":done.model,"choices":[{"index":0,"message":done.message(),"finish_reason":done.stop_reason}],"usage":done.usage()})).into_response(),
            Err(e) => failure(StatusCode::BAD_GATEWAY, &e),
        };
    }
    failure(last_status, &last)
}

/// Convert a decoded completion to a Responses object.
fn response_object(done: &Decoder) -> Value {
    let mut output = Vec::new();
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut full_text = String::new();
    for block in done.blocks.values() {
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            push_message(&mut output, &mut parts, &mut text, &done.id);
            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = block
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({}))
                .to_string();
            output.push(json!({"id":id,"type":"function_call","status":"completed","call_id":id,"name":name.strip_prefix('_').unwrap_or(name),"arguments":args}));
        } else {
            if let Some(fragment) = block.get("text").and_then(Value::as_str) {
                text.push_str(fragment);
                full_text.push_str(fragment);
            }
            parts.push(block.clone());
        }
    }
    push_message(&mut output, &mut parts, &mut text, &done.id);
    json!({"id":done.id,"object":"response","created_at":done.created,"model":done.model,"status":if done.stop_reason == "length" {"incomplete"} else {"completed"},"output":output,"output_text":full_text,"usage":{"input_tokens":done.input_tokens,"output_tokens":done.output_tokens,"total_tokens":done.input_tokens.saturating_add(done.output_tokens)}})
}

/// Append one contiguous assistant message, preserving its native blocks.
fn push_message(output: &mut Vec<Value>, parts: &mut Vec<Value>, text: &mut String, id: &str) {
    if parts.is_empty() {
        return;
    }
    let content = if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({"type":"output_text","text":&*text})]
    };
    output.push(json!({"id":format!("msg-{id}-{}", output.len()),"type":"message","status":"completed","role":"assistant","content":content,"provider_parts":&*parts}));
    parts.clear();
    text.clear();
}

/// Stream decoded events to a chat or Responses client.
#[allow(clippy::too_many_lines)]
fn stream_response(
    upstream: reqwest::Response,
    decoder: Decoder,
    responses: bool,
    include_usage: bool,
) -> Response {
    let (events, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    tokio::spawn(async move {
        let id = decoder.id.clone();
        let model = decoder.model.clone();
        let created = decoder.created;
        if responses {
            let data = json!({"type":"response.created","response":{"id":id,"object":"response","created_at":created,"model":model,"status":"in_progress"}});
            if events
                .send(Ok(Event::default()
                    .event("response.created")
                    .data(data.to_string())))
                .await
                .is_err()
            {
                return;
            }
        }
        let (tx, mut incoming) = mpsc::channel(64);
        let job = tokio::spawn(client::consume(upstream, decoder, Some(tx)));
        let mut tool_ids = std::collections::HashMap::new();
        let mut next_index = 0_u64;
        let mut message_index = None;
        let mut tool_indexes = std::collections::HashMap::new();
        while let Some(Ok(chunk)) = incoming.recv().await {
            if responses {
                let delta = chunk.pointer("/choices/0/delta").unwrap_or(&Value::Null);
                let fragment = delta
                    .get("content")
                    .and_then(Value::as_str)
                    .map(|s| ("response.output_text.delta", s))
                    .or_else(|| {
                        delta
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .map(|s| ("response.reasoning_text.delta", s))
                    });
                let has_message_part = delta
                    .pointer("/provider_parts/0/type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "thinking" | "redacted_thinking" | "text"));
                if fragment.is_some() || has_message_part {
                    let index = if let Some(index) = message_index {
                        index
                    } else {
                        let index = next_index;
                        next_index = next_index.saturating_add(1);
                        message_index = Some(index);
                        let item = json!({"type":"response.output_item.added","output_index":index,"item":{"id":format!("msg-{id}-{index}"),"type":"message","status":"in_progress","role":"assistant","content":[]}});
                        if events
                            .send(Ok(Event::default()
                                .event("response.output_item.added")
                                .data(item.to_string())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let part = json!({"type":"response.content_part.added","item_id":format!("msg-{id}-{index}"),"output_index":index,"content_index":0,"part":{"type":"output_text","text":""}});
                        if events
                            .send(Ok(Event::default()
                                .event("response.content_part.added")
                                .data(part.to_string())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        index
                    };
                    if let Some((name, text)) = fragment {
                        let value = json!({"type":name,"item_id":format!("msg-{id}-{index}"),"output_index":index,"content_index":0,"delta":text});
                        if events
                            .send(Ok(Event::default().event(name).data(value.to_string())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let tool = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(call_id) = call.get("id").and_then(Value::as_str) {
                            message_index = None;
                            let index = next_index;
                            next_index = next_index.saturating_add(1);
                            tool_indexes.insert(tool, index);
                            tool_ids.insert(index, call_id.to_owned());
                            let item = json!({"type":"response.output_item.added","output_index":index,"item":{"id":call_id,"type":"function_call","status":"in_progress","call_id":call_id,"name":call.pointer("/function/name"),"arguments":""}});
                            if events
                                .send(Ok(Event::default()
                                    .event("response.output_item.added")
                                    .data(item.to_string())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        let index = tool_indexes.get(&tool).copied().unwrap_or(tool);
                        if let Some(arguments) = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                        {
                            let data = json!({"type":"response.function_call_arguments.delta","item_id":tool_ids.get(&index),"output_index":index,"delta":arguments});
                            if events
                                .send(Ok(Event::default()
                                    .event("response.function_call_arguments.delta")
                                    .data(data.to_string())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            } else if events
                .send(Ok(Event::default().data(chunk.to_string())))
                .await
                .is_err()
            {
                return;
            }
        }
        if let Ok(Ok(done)) = job.await {
            if responses {
                let response = response_object(&done);
                if let Some(items) = response.get("output").and_then(Value::as_array) {
                    for (index, item) in items.iter().enumerate() {
                        if item.get("type").and_then(Value::as_str) == Some("message") {
                            let text = item
                                .pointer("/content/0/text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let item_id = item.get("id");
                            let data = json!({"type":"response.output_text.done","item_id":item_id,"output_index":index,"content_index":0,"text":text});
                            let _ = events
                                .send(Ok(Event::default()
                                    .event("response.output_text.done")
                                    .data(data.to_string())))
                                .await;
                            let data = json!({"type":"response.content_part.done","item_id":item_id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":text}});
                            let _ = events
                                .send(Ok(Event::default()
                                    .event("response.content_part.done")
                                    .data(data.to_string())))
                                .await;
                        }
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let data = json!({"type":"response.function_call_arguments.done","item_id":item.get("id"),"output_index":index,"arguments":item.get("arguments")});
                            let _ = events
                                .send(Ok(Event::default()
                                    .event("response.function_call_arguments.done")
                                    .data(data.to_string())))
                                .await;
                        }
                        let data = json!({"type":"response.output_item.done","output_index":index,"item":item});
                        let _ = events
                            .send(Ok(Event::default()
                                .event("response.output_item.done")
                                .data(data.to_string())))
                            .await;
                    }
                }
                let event = if done.stop_reason == "length" {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                let value = json!({"type":event,"response":response});
                let _ = events
                    .send(Ok(Event::default().event(event).data(value.to_string())))
                    .await;
            } else if include_usage {
                let _ = events.send(Ok(Event::default().data(json!({"id":done.id,"object":"chat.completion.chunk","created":done.created,"model":done.model,"choices":[],"usage":done.usage()}).to_string()))).await;
            }
            let _ = events.send(Ok(Event::default().data("[DONE]"))).await;
        } else {
            let (name, value) = if responses {
                (
                    "response.failed",
                    json!({"type":"response.failed","response":{"id":id,"object":"response","created_at":created,"model":model,"status":"failed","error":{"code":"bad_gateway","message":"Claude upstream stream failed"}}}),
                )
            } else {
                (
                    "error",
                    json!({"error":{"message":"Claude upstream stream failed","type":"upstream_error"}}),
                )
            };
            let _ = events
                .send(Ok(Event::default().event(name).data(value.to_string())))
                .await;
        }
    });
    Sse::new(ReceiverStream::new(rx)).into_response()
}

/// Gateway stream behavior tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// Preserve tool calls and signed reasoning through both stream formats.
    #[tokio::test]
    async fn streams_tool_calls_and_signed_reasoning_from_anthropic() {
        let upstream = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool1\",\"name\":\"_search\",\"input\":{}}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        for responses in [false, true] {
            let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
            let response = stream_response(upstream, Decoder::new("test".into()), responses, true);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let output = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(output.contains("tool1"));
            assert!(output.contains("search"));
            assert!(output.contains("sig"));
            if responses {
                assert!(output.contains("response.function_call_arguments.delta"));
                assert!(output.contains("response.completed"));
            } else {
                assert!(output.contains("[DONE]"));
                assert!(output.contains("tool_calls"));
            }
        }
    }

    /// A completed text block must not produce a reasoning delta.
    #[tokio::test]
    async fn text_only_responses_stream_has_no_reasoning_delta() {
        let upstream = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
        let response = stream_response(upstream, Decoder::new("test".into()), true, false);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("hello"));
        assert!(output.contains("response.completed"));
        assert!(!output.contains("response.reasoning_text.delta"));
    }

    /// Empty tool arguments and cached input counts survive both stream formats.
    #[tokio::test]
    async fn tool_only_stream_has_no_phantom_message() {
        let app = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":5}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"_search\",\"input\":{}}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        for responses in [false, true] {
            let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
            let response = stream_response(upstream, Decoder::new("test".into()), responses, true);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(
                body.contains("\\\"arguments\\\":\\\"{}\\\"")
                    || body.contains("\"arguments\":\"{}\"")
            );
            if responses {
                assert!(!body.contains("\"type\":\"message\""));
                assert!(body.contains("\"input_tokens\":10"));
            } else {
                assert!(body.contains("\"prompt_tokens\":10"));
            }
        }
    }

    /// A Responses output can be replayed without changing native block order.
    #[tokio::test]
    async fn responses_replay_retains_interleaved_blocks() {
        use axum::{body::Bytes, routing::post};
        let app = Router::new()
            .route("/events", get(|| async {
                concat!(
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"before\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"_search\",\"input\":{}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"thought\",\"signature\":\"signed\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n"
                )
            }))
            .route("/messages", post(|body: Bytes| async move {
                let value: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value.pointer("/messages/0/content/0/type").and_then(Value::as_str), Some("text"));
                assert_eq!(value.pointer("/messages/0/content/1/type").and_then(Value::as_str), Some("tool_use"));
                assert_eq!(value.pointer("/messages/0/content/2/signature").and_then(Value::as_str), Some("signed"));
                "ok"
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{address}/events"))
            .await
            .unwrap();
        let done = client::consume(upstream, Decoder::new("test".into()), None)
            .await
            .unwrap();
        let output = response_object(&done);
        let request: ResponseCreateRequest =
            serde_json::from_value(json!({"model":"test","input":output.get("output").unwrap()}))
                .unwrap();
        let payload = payload::build(&request.into_chat_request()).unwrap();
        let result = client::send(
            &client::http().unwrap(),
            &format!("http://{address}/messages"),
            "secret",
            &payload,
        )
        .await
        .unwrap();
        assert!(result.status().is_success());
    }
}
