use axum::response::{
    IntoResponse, Response,
    sse::{Event, Sse},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::stream::Decoder;
/// Dispatches a single upstream event with bounded buffering.
pub(super) async fn consume(
    response: reqwest::Response,
    mut decoder: Decoder,
    tx: Option<mpsc::Sender<Result<Value, String>>>,
) -> Result<Decoder, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Claude stream failed: {e}"))?;
        if buffer.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("Claude stream event too large".into());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&bytes)
                .map_err(|_| "Invalid UTF-8 in Claude stream")?
                .trim_end();
            if let Some(data) = line.strip_prefix("data:") {
                let event: Value = serde_json::from_str(data.trim())
                    .map_err(|e| format!("Invalid Claude stream event: {e}"))?;
                for output in decoder.event(&event)? {
                    if let Some(tx) = &tx {
                        tx.send(Ok(output)).await.map_err(|_| "Downstream closed")?;
                    }
                }
            }
        }
    }
    if !decoder.finished {
        return Err("Claude stream ended before message_stop".into());
    }
    Ok(decoder)
}

/// Convert a decoded completion to a chat completion object.
pub(super) fn chat_object(done: &Decoder) -> Value {
    json!({"id":done.id,"object":"chat.completion","created":done.created,"model":done.model,"choices":[{"index":0,"message":done.message(),"finish_reason":done.stop_reason}],"usage":done.usage()})
}

/// Convert a decoded completion to a Responses object.
pub(super) fn response_object(done: &Decoder) -> Value {
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
pub(super) fn stream_response(
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
        let job = tokio::spawn(consume(upstream, decoder, Some(tx)));
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
