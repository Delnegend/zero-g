use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response, Sse, sse::Event as SseEvent};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Decoded Cursor output, independent of its wire protocol.
#[derive(Debug)]
pub(super) enum Event {
    /// An assistant text fragment.
    Text(String),
    /// A reasoning text fragment.
    Thinking(String),
    /// A completed function call.
    Tool {
        /// Caller-visible call identifier.
        id: String,
        /// Function name.
        name: String,
        /// JSON-encoded arguments.
        arguments: String,
    },
    /// Token counts reported by Cursor.
    Usage {
        /// Input token count.
        input: i64,
        /// Output token count.
        output: i64,
    },
    /// Successful termination of the upstream turn.
    Done,
}

/// Maintains response state and converts Cursor events into `OpenAI` events.
struct Translator {
    /// Identifier shared by the response and its chunks.
    id: String,
    /// Model requested by the client.
    model: String,
    /// Response creation time in seconds.
    created: u64,
    /// Whether to emit Responses API objects rather than chat completions.
    responses: bool,
    /// Whether chat streams should include a final usage chunk.
    include_usage: bool,
    /// Accumulated assistant text.
    text: String,
    /// Accumulated assistant reasoning.
    thinking: String,
    /// Completed chat function calls.
    tools: Vec<Value>,
    /// Completed Responses API output items in order.
    output: Vec<Value>,
    /// Open message output index, identifier, and accumulated text.
    message: Option<(usize, String, String)>,
    /// Open reasoning output index, identifier, and accumulated text.
    reasoning: Option<(usize, String, String)>,
    /// Most recent input and output token counts.
    usage: Option<(i64, i64)>,
    /// Whether an assistant role was emitted in a chat chunk.
    role_sent: bool,
}

impl Translator {
    /// Create a fresh response with a unique identifier.
    fn new(model: String, responses: bool, include_usage: bool) -> Self {
        Self {
            id: format!(
                "{}{}",
                if responses { "resp_" } else { "chatcmpl-" },
                uuid::Uuid::new_v4().simple()
            ),
            model,
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            responses,
            include_usage,
            text: String::new(),
            thinking: String::new(),
            tools: Vec::new(),
            output: Vec::new(),
            message: None,
            reasoning: None,
            usage: None,
            role_sent: false,
        }
    }

    /// Encode a named Responses API event.
    fn named(kind: &str, mut value: Value) -> SseEvent {
        if let Some(object) = value.as_object_mut() {
            object.insert("type".to_string(), json!(kind));
        }
        SseEvent::default().event(kind).data(value.to_string())
    }

    /// Emit the initial response event when applicable.
    fn initial(&self) -> Vec<SseEvent> {
        if self.responses {
            vec![Self::named(
                "response.created",
                json!({"response": {"id": self.id, "object": "response", "created_at": self.created, "status": "in_progress", "model": self.model, "output": []}}),
            )]
        } else {
            Vec::new()
        }
    }

    /// Encode a chat completion chunk.
    fn chunk(&mut self, mut delta: Value, finish: Option<&str>) -> SseEvent {
        if !self.role_sent {
            if let Some(object) = delta.as_object_mut() {
                object.insert("role".to_string(), json!("assistant"));
            }
            self.role_sent = true;
        }
        let mut chunk = json!({"id": self.id, "object": "chat.completion.chunk", "created": self.created, "model": self.model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        if self.include_usage
            && let Some(object) = chunk.as_object_mut()
        {
            object.insert("usage".to_string(), Value::Null);
        }
        SseEvent::default().data(chunk.to_string())
    }

    /// Finish an open assistant message and append it to the output.
    fn close_message(&mut self, events: &mut Vec<SseEvent>) {
        if let Some((index, id, text)) = self.message.take() {
            let part = json!({"type": "output_text", "text": text, "annotations": []});
            let item = json!({"id": id, "type": "message", "status": "completed", "role": "assistant", "content": [part]});
            events.push(Self::named(
                "response.output_text.done",
                json!({"item_id": id, "output_index": index, "content_index": 0, "text": text}),
            ));
            events.push(Self::named(
                "response.content_part.done",
                json!({"item_id": id, "output_index": index, "content_index": 0, "part": part}),
            ));
            events.push(Self::named(
                "response.output_item.done",
                json!({"output_index": index, "item": item}),
            ));
            self.output.push(item);
        }
    }

    /// Finish an open reasoning item and append it to the output.
    fn close_reasoning(&mut self, events: &mut Vec<SseEvent>) {
        if let Some((index, id, text)) = self.reasoning.take() {
            let item = json!({"id": id, "type": "reasoning", "summary": [], "content": [{"type": "reasoning_text", "text": text}]});
            events.push(Self::named(
                "response.reasoning_text.done",
                json!({"item_id": id, "output_index": index, "content_index": 0, "text": text}),
            ));
            events.push(Self::named(
                "response.output_item.done",
                json!({"output_index": index, "item": item}),
            ));
            self.output.push(item);
        }
    }

    /// Translate one decoded upstream event.
    fn handle(&mut self, event: Event) -> Vec<SseEvent> {
        let mut events = Vec::new();
        match event {
            Event::Text(text) => {
                self.text.push_str(&text);
                if self.responses {
                    self.close_reasoning(&mut events);
                    if self.message.is_none() {
                        let index = self.output.len();
                        let id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                        events.push(Self::named("response.output_item.added", json!({"output_index": index, "item": {"id": id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}})));
                        events.push(Self::named("response.content_part.added", json!({"item_id": id, "output_index": index, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}})));
                        self.message = Some((index, id, String::new()));
                    }
                    if let Some((index, id, accumulated)) = &mut self.message {
                        accumulated.push_str(&text);
                        events.push(Self::named("response.output_text.delta", json!({"item_id": id, "output_index": index, "content_index": 0, "delta": text})));
                    }
                } else {
                    events.push(self.chunk(json!({"content": text}), None));
                }
            }
            Event::Thinking(text) => {
                self.thinking.push_str(&text);
                if self.responses {
                    self.close_message(&mut events);
                    if self.reasoning.is_none() {
                        let index = self.output.len();
                        let id = format!("rs_{}", uuid::Uuid::new_v4().simple());
                        events.push(Self::named("response.output_item.added", json!({"output_index": index, "item": {"id": id, "type": "reasoning", "summary": [], "content": []}})));
                        self.reasoning = Some((index, id, String::new()));
                    }
                    if let Some((index, id, accumulated)) = &mut self.reasoning {
                        accumulated.push_str(&text);
                        events.push(Self::named("response.reasoning_text.delta", json!({"item_id": id, "output_index": index, "content_index": 0, "delta": text})));
                    }
                } else {
                    events.push(self.chunk(json!({"reasoning_content": text}), None));
                }
            }
            Event::Tool {
                id,
                name,
                arguments,
            } => {
                let index = self.tools.len();
                self.tools.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}}));
                if self.responses {
                    self.close_message(&mut events);
                    self.close_reasoning(&mut events);
                    let output_index = self.output.len();
                    let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
                    let added = json!({"id": item_id, "type": "function_call", "status": "in_progress", "call_id": id, "name": name, "arguments": ""});
                    let item = json!({"id": item_id, "type": "function_call", "status": "completed", "call_id": id, "name": name, "arguments": arguments});
                    events.push(Self::named(
                        "response.output_item.added",
                        json!({"output_index": output_index, "item": added}),
                    ));
                    events.push(Self::named("response.function_call_arguments.delta", json!({"item_id": item_id, "output_index": output_index, "delta": arguments})));
                    events.push(Self::named("response.function_call_arguments.done", json!({"item_id": item_id, "output_index": output_index, "arguments": arguments})));
                    events.push(Self::named(
                        "response.output_item.done",
                        json!({"output_index": output_index, "item": item}),
                    ));
                    self.output.push(item);
                } else {
                    events.push(self.chunk(json!({"tool_calls": [{"index": index, "id": id, "type": "function", "function": {"name": name, "arguments": arguments}}]}), None));
                }
            }
            Event::Usage { input, output } => self.usage = Some((input, output)),
            Event::Done => {}
        }
        events
    }

    /// Render the latest token counts in the selected API shape.
    fn usage(&self) -> Value {
        self.usage.map_or(Value::Null, |(input, output)| {
            if self.responses {
                json!({"input_tokens": input, "output_tokens": output, "total_tokens": input.saturating_add(output)})
            } else {
                json!({"prompt_tokens": input, "completion_tokens": output, "total_tokens": input.saturating_add(output)})
            }
        })
    }

    /// Render the completed nonstreaming response.
    fn result(&self) -> Value {
        if self.responses {
            json!({"id": self.id, "object": "response", "created_at": self.created, "status": "completed", "model": self.model, "output": self.output, "output_text": self.text, "usage": self.usage()})
        } else {
            let mut message = json!({"role": "assistant", "content": if self.text.is_empty() && !self.tools.is_empty() { Value::Null } else { json!(self.text) }});
            if let Some(object) = message.as_object_mut() {
                if !self.thinking.is_empty() {
                    object.insert("reasoning_content".to_string(), json!(self.thinking));
                }
                if !self.tools.is_empty() {
                    object.insert("tool_calls".to_string(), json!(self.tools));
                }
            }
            json!({"id": self.id, "object": "chat.completion", "created": self.created, "model": self.model, "choices": [{"index": 0, "message": message, "finish_reason": if self.tools.is_empty() { "stop" } else { "tool_calls" }}], "usage": self.usage()})
        }
    }

    /// Emit completion events after a successful upstream turn.
    fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.responses {
            self.close_reasoning(&mut events);
            self.close_message(&mut events);
            events.push(Self::named(
                "response.completed",
                json!({"response": self.result()}),
            ));
        } else {
            let reason = if self.tools.is_empty() {
                "stop"
            } else {
                "tool_calls"
            };
            events.push(self.chunk(json!({}), Some(reason)));
            if self.include_usage && self.usage.is_some() {
                events.push(SseEvent::default().data(
                    json!({"id": self.id, "object": "chat.completion.chunk", "created": self.created, "model": self.model, "choices": [], "usage": self.usage()}).to_string(),
                ));
            }
        }
        events.push(SseEvent::default().data("[DONE]"));
        events
    }

    /// Emit a terminal upstream failure.
    fn failure(&self, error: &str) -> Vec<SseEvent> {
        let detail = json!({"message": error, "type": "upstream_error", "code": "bad_gateway"});
        if self.responses {
            vec![
                Self::named(
                    "response.failed",
                    json!({"response": {"id": self.id, "object": "response", "created_at": self.created, "model": self.model, "status": "failed", "error": detail}}),
                ),
                SseEvent::default().data("[DONE]"),
            ]
        } else {
            vec![
                SseEvent::default().data(json!({"error": detail}).to_string()),
                SseEvent::default().data("[DONE]"),
            ]
        }
    }
}

/// Translate decoded events into an OpenAI-compatible HTTP response.
pub(super) async fn respond(
    mut rx: mpsc::Receiver<Result<Event, String>>,
    model: String,
    streaming: bool,
    responses: bool,
    include_usage: bool,
) -> Response {
    let mut translator = Translator::new(model, responses, include_usage);
    if streaming {
        let (tx, output) = mpsc::channel::<Result<SseEvent, Infallible>>(32);
        tokio::spawn(async move {
            for event in translator.initial() {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            while let Some(next) = rx.recv().await {
                let (events, finished) = match next {
                    Ok(Event::Done) => (translator.finish(), true),
                    Ok(event) => (translator.handle(event), false),
                    Err(error) => (translator.failure(&error), true),
                };
                for event in events {
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                if finished {
                    return;
                }
            }
            for event in translator.failure("Cursor stream ended before completion") {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });
        Sse::new(ReceiverStream::new(output)).into_response()
    } else {
        while let Some(next) = rx.recv().await {
            match next {
                Ok(Event::Done) => {
                    if responses {
                        translator.close_reasoning(&mut Vec::new());
                        translator.close_message(&mut Vec::new());
                    }
                    return Json(translator.result()).into_response();
                }
                Ok(event) => { translator.handle(event); }
                Err(error) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": {"message": error, "type": "upstream_error", "code": "bad_gateway"}}))).into_response(),
            }
        }
        (StatusCode::BAD_GATEWAY, Json(json!({"error": {"message": "Cursor stream ended before completion", "type": "upstream_error", "code": "bad_gateway"}}))).into_response()
    }
}

/// Tests of client-visible translation behavior.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// Chat completions retain content, function arguments, and usage.
    #[tokio::test]
    async fn nonstream_preserves_text_tools_and_usage() {
        let (tx, rx) = mpsc::channel(8);
        for event in [
            Event::Text("hello".into()),
            Event::Tool {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{\"x\":1}".into(),
            },
            Event::Usage {
                input: 3,
                output: 4,
            },
            Event::Done,
        ] {
            assert!(tx.send(Ok(event)).await.is_ok());
        }
        let response = respond(rx, "model".into(), false, false, false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let Ok(bytes) = to_bytes(response.into_body(), usize::MAX).await else {
            panic!("body")
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            panic!("json")
        };
        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"x\":1}"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(value["usage"]["total_tokens"], 7);
    }

    /// Responses preserve the order of reasoning, messages, and function calls.
    #[tokio::test]
    async fn responses_keep_ordered_reasoning_text_and_calls() {
        let (tx, rx) = mpsc::channel(8);
        for event in [
            Event::Thinking("think".into()),
            Event::Text("answer".into()),
            Event::Tool {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{}".into(),
            },
            Event::Text("after".into()),
            Event::Done,
        ] {
            assert!(tx.send(Ok(event)).await.is_ok());
        }
        let response = respond(rx, "model".into(), false, true, false).await;
        let Ok(bytes) = to_bytes(response.into_body(), usize::MAX).await else {
            panic!("body")
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            panic!("json")
        };
        assert_eq!(value["output"][0]["content"][0]["text"], "think");
        assert_eq!(value["output"][1]["content"][0]["text"], "answer");
        assert_eq!(value["output"][2]["call_id"], "call_1");
        assert_eq!(value["output"][3]["content"][0]["text"], "after");
    }

    /// Streaming chat exposes function calls and the correct finish reason.
    #[tokio::test]
    async fn chat_stream_emits_tool_and_finish() {
        let (tx, rx) = mpsc::channel(4);
        assert!(
            tx.send(Ok(Event::Tool {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{}".into()
            }))
            .await
            .is_ok()
        );
        assert!(tx.send(Ok(Event::Done)).await.is_ok());
        let response = respond(rx, "model".into(), true, false, false).await;
        let Ok(bytes) = to_bytes(response.into_body(), usize::MAX).await else {
            panic!("body")
        };
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("\"id\":\"call_1\""));
        assert!(body.contains("\"finish_reason\":\"tool_calls\""));
        assert!(body.contains("[DONE]"));
    }

    /// Requested usage follows the finish chunk and precedes the stream terminator.
    #[tokio::test]
    async fn chat_stream_includes_requested_usage() {
        let (tx, rx) = mpsc::channel(8);
        for event in [
            Event::Text("hello".into()),
            Event::Tool {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{}".into(),
            },
            Event::Usage {
                input: 3,
                output: 4,
            },
            Event::Usage {
                input: 12,
                output: 5,
            },
            Event::Done,
        ] {
            assert!(tx.send(Ok(event)).await.is_ok());
        }
        let response = respond(rx, "model".into(), true, false, true).await;
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let frames: Vec<_> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        assert_eq!(frames.len(), 5);
        let text: Value = serde_json::from_str(frames[0]).unwrap();
        let tool: Value = serde_json::from_str(frames[1]).unwrap();
        let finish: Value = serde_json::from_str(frames[2]).unwrap();
        let usage: Value = serde_json::from_str(frames[3]).unwrap();
        assert_eq!(text["choices"][0]["delta"]["content"], "hello");
        assert_eq!(tool["choices"][0]["delta"]["tool_calls"][0]["id"], "call_1");
        assert_eq!(finish["choices"][0]["finish_reason"], "tool_calls");
        for chunk in [&text, &tool, &finish] {
            assert_eq!(chunk.get("usage"), Some(&Value::Null));
        }
        assert_eq!(usage["choices"], json!([]));
        assert_eq!(
            usage["usage"],
            json!({"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17})
        );
        assert_eq!(usage["id"], text["id"]);
        assert_eq!(usage["model"], text["model"]);
        assert_eq!(usage["created"], text["created"]);
        assert_eq!(usage["object"], "chat.completion.chunk");
        assert_eq!(frames[4], "[DONE]");
    }

    /// Ordinary streams and streams without upstream counts have no usage chunk.
    #[tokio::test]
    async fn chat_stream_omits_unrequested_or_unavailable_usage() {
        for (include_usage, has_usage) in [(false, true), (true, false)] {
            let (tx, rx) = mpsc::channel(4);
            assert!(tx.send(Ok(Event::Text("hello".into()))).await.is_ok());
            if has_usage {
                assert!(
                    tx.send(Ok(Event::Usage {
                        input: 3,
                        output: 4
                    }))
                    .await
                    .is_ok()
                );
            }
            assert!(tx.send(Ok(Event::Done)).await.is_ok());
            let response = respond(rx, "model".into(), true, false, include_usage).await;
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            let frames: Vec<_> = body
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect();
            assert_eq!(frames.len(), 3);
            let text: Value = serde_json::from_str(frames[0]).unwrap();
            let finish: Value = serde_json::from_str(frames[1]).unwrap();
            assert_eq!(text["choices"][0]["delta"]["content"], "hello");
            assert_eq!(finish["choices"][0]["finish_reason"], "stop");
            let expected = include_usage.then_some(&Value::Null);
            assert_eq!(text.get("usage"), expected);
            assert_eq!(finish.get("usage"), expected);
            assert_eq!(frames[2], "[DONE]");
        }
    }

    /// Responses streaming keeps its own usage format, regardless of chat options.
    #[tokio::test]
    async fn responses_stream_usage_is_unchanged() {
        let (tx, rx) = mpsc::channel(4);
        assert!(tx.send(Ok(Event::Text("hello".into()))).await.is_ok());
        assert!(
            tx.send(Ok(Event::Usage {
                input: 3,
                output: 4
            }))
            .await
            .is_ok()
        );
        assert!(tx.send(Ok(Event::Done)).await.is_ok());
        let response = respond(rx, "model".into(), true, true, true).await;
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let frames: Vec<_> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        let completed: Value = frames
            .iter()
            .filter_map(|frame| serde_json::from_str::<Value>(frame).ok())
            .find(|value| value["type"] == "response.completed")
            .unwrap();
        assert_eq!(
            completed["response"]["usage"],
            json!({"input_tokens": 3, "output_tokens": 4, "total_tokens": 7})
        );
        assert!(!body.contains("chat.completion.chunk"));
        assert_eq!(frames.last(), Some(&"[DONE]"));
    }

    /// Streaming Responses report an error after partial output.
    #[tokio::test]
    async fn response_stream_reports_partial_failure() {
        let (tx, rx) = mpsc::channel(4);
        assert!(tx.send(Ok(Event::Text("partial".into()))).await.is_ok());
        assert!(tx.send(Err("broken".into())).await.is_ok());
        let response = respond(rx, "model".into(), true, true, false).await;
        let Ok(bytes) = to_bytes(response.into_body(), usize::MAX).await else {
            panic!("body")
        };
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("response.output_text.delta"));
        assert!(body.contains("response.failed"));
        assert!(body.contains("broken"));
        assert!(!body.contains("response.completed"));
    }

    /// An upstream disconnect without completion fails the request.
    #[tokio::test]
    async fn incomplete_nonstream_is_an_error() {
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let response = respond(rx, "model".into(), false, false, false).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
