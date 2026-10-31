use axum::response::sse::Event;
use serde_json::json;

use crate::provider::antigravity::AntigravityEvent;
use crate::responses::{ResponseContentPart, ResponseObject, ResponseOutputItem, ResponseUsage};

/// Serializes provider events into Responses API stream events.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ResponsesStreamSerializer {
    /// Response identifier.
    pub(crate) resp_id: String,
    /// Identifier for the assistant message item.
    pub(crate) msg_id: String,
    /// Response creation timestamp.
    pub(crate) created: u64,
    /// Model name requested by the caller.
    pub(crate) original_model: String,
    /// Whether an assistant message is currently open.
    pub(crate) message_in_progress: bool,
    /// Output index of the current assistant message.
    pub(crate) message_index: Option<usize>,
    /// Index assigned to the next output item.
    pub(crate) next_output_index: usize,
    /// Accumulated text for the current assistant message.
    pub(crate) full_text: String,
    /// Ordered provider parts for client signature replay.
    pub(crate) provider_parts: Vec<serde_json::Value>,
    /// Most recently received token usage.
    pub(crate) usage: ResponseUsage,
    /// Completed output items paired with their output indices.
    pub(crate) output_items: Vec<(usize, ResponseOutputItem)>,
    /// Whether the upstream terminated without a complete answer.
    pub(crate) incomplete: bool,
}

impl crate::provider::antigravity::sse::SseSerializer for ResponsesStreamSerializer {
    fn initial(&mut self) -> Vec<Event> {
        let created_event = json!({
            "type": "response.created",
            "response": {
                "id": self.resp_id,
                "object": "response",
                "created_at": self.created,
                "status": "in_progress",
                "model": self.original_model
            }
        });
        vec![
            Event::default()
                .event("response.created")
                .data(created_event.to_string()),
        ]
    }

    fn error(&mut self) -> Event {
        Event::default().event("response.failed").data(
            json!({
                "type": "response.failed",
                "response": {
                    "id": self.resp_id,
                    "object": "response",
                    "created_at": self.created,
                    "model": self.original_model,
                    "status": "failed",
                    "error": { "code": "bad_gateway", "message": "Upstream request failed" }
                }
            })
            .to_string(),
        )
    }

    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event> {
        let mut events = Vec::new();
        match event {
            AntigravityEvent::Text(text) => {
                self.content_events(&text, false, None, &mut events);
            }
            AntigravityEvent::Reasoning(text) => {
                self.content_events(&text, true, None, &mut events);
            }
            AntigravityEvent::SignedPart {
                text,
                reasoning,
                signature,
            } => {
                self.content_events(&text, reasoning, Some(&signature), &mut events);
            }
            AntigravityEvent::ToolCall {
                id,
                name,
                arguments,
                thought_signature,
            } => {
                self.tool_call_events(id, name, arguments, thought_signature, &mut events);
            }
            AntigravityEvent::Finish { reason } => self.incomplete = reason != "STOP",
            AntigravityEvent::Usage(value) => {
                self.usage = ResponseUsage::from(value);
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.finish_message(&mut events);

        self.output_items.sort_by_key(|(index, _)| *index);
        let final_items: Vec<ResponseOutputItem> = std::mem::take(&mut self.output_items)
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        let mut output_text = String::new();
        for item in &final_items {
            if let ResponseOutputItem::Message { content, .. } = item {
                for part in content {
                    let ResponseContentPart::OutputText { text } = part;
                    output_text.push_str(text);
                }
            }
        }

        let final_response = ResponseObject {
            id: self.resp_id.clone(),
            object: "response".to_string(),
            created_at: self.created,
            model: self.original_model.clone(),
            status: if self.incomplete {
                "incomplete"
            } else {
                "completed"
            }
            .to_string(),
            output: final_items,
            output_text,
            usage: Some(self.usage.clone()),
        };

        let event_type = if self.incomplete {
            "response.incomplete"
        } else {
            "response.completed"
        };
        let completed_event = json!({
            "type": event_type,
            "response": final_response
        });
        events.push(
            Event::default()
                .event(event_type)
                .data(completed_event.to_string()),
        );
        events
    }
}

impl ResponsesStreamSerializer {
    /// Opens a message before emitting any of its content or replay parts.
    fn open_message(&mut self, events: &mut Vec<Event>) -> usize {
        if let Some(index) = self.message_index {
            return index;
        }
        if self.msg_id.is_empty() {
            self.msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        }
        self.message_in_progress = true;
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        self.message_index = Some(index);
        let added_event = json!({
            "type": "response.output_item.added", "output_index": index,
            "item": {"id": self.msg_id, "type": "message", "status": "in_progress",
                "role": "assistant", "content": []}
        });
        events.push(
            Event::default()
                .event("response.output_item.added")
                .data(added_event.to_string()),
        );
        let part_event = json!({
            "type": "response.content_part.added", "item_id": self.msg_id, "output_index": index,
            "content_index": 0, "part": {"type": "output_text", "text": ""}
        });
        events.push(
            Event::default()
                .event("response.content_part.added")
                .data(part_event.to_string()),
        );
        index
    }

    /// Emits a content fragment and its corresponding replay part.
    fn content_events(
        &mut self,
        text: &str,
        reasoning: bool,
        signature: Option<&str>,
        events: &mut Vec<Event>,
    ) {
        let index = self.open_message(events);
        let part = super::provider_part(&self.original_model, text, reasoning, signature);
        events.push(
            Event::default()
                .event("response.provider_part.delta")
                .data(json!({"type": "response.provider_part.delta", "part": part}).to_string()),
        );
        self.provider_parts.push(part);
        let delta = if reasoning {
            json!({
                "type": "response.reasoning_text.delta", "output_index": index,
                "content_index": 0, "delta": text, "item_id": self.msg_id
            })
        } else {
            self.full_text.push_str(text);
            json!({
                "type": "response.output_text.delta", "output_index": index,
                "content_index": 0, "delta": text, "item_id": self.msg_id
            })
        };
        events.push(
            Event::default()
                .event(if reasoning {
                    "response.reasoning_text.delta"
                } else {
                    "response.output_text.delta"
                })
                .data(delta.to_string()),
        );
    }

    /// Completes the current message before another output item begins.
    fn finish_message(&mut self, events: &mut Vec<Event>) {
        if !self.message_in_progress {
            return;
        }
        let Some(index) = self.message_index.take() else {
            return;
        };
        let text = std::mem::take(&mut self.full_text);
        let done_text_event = json!({
            "type": "response.output_text.done", "item_id": self.msg_id,
            "output_index": index, "content_index": 0, "text": text
        });
        events.push(
            Event::default()
                .event("response.output_text.done")
                .data(done_text_event.to_string()),
        );
        let done_part_event = json!({
            "type": "response.content_part.done", "item_id": self.msg_id,
            "output_index": index, "content_index": 0,
            "part": {"type": "output_text", "text": text}
        });
        events.push(
            Event::default()
                .event("response.content_part.done")
                .data(done_part_event.to_string()),
        );
        let msg_item = ResponseOutputItem::Message {
            id: std::mem::take(&mut self.msg_id),
            status: "completed".to_string(),
            role: "assistant".to_string(),
            content: vec![ResponseContentPart::OutputText { text }],
            provider_parts: Some(std::mem::take(&mut self.provider_parts)),
        };
        let done_item_event = json!({
            "type": "response.output_item.done", "output_index": index, "item": msg_item
        });
        events.push(
            Event::default()
                .event("response.output_item.done")
                .data(done_item_event.to_string()),
        );
        self.output_items.push((index, msg_item));
        self.message_in_progress = false;
    }

    /// Appends stream events for a function call and records its output item.
    fn tool_call_events(
        &mut self,
        id: String,
        name: String,
        arguments: String,
        thought_signature: Option<String>,
        events: &mut Vec<Event>,
    ) {
        self.finish_message(events);
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        let fc_item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
        let thought_signature = thought_signature
            .map(|signature| super::scoped_signature(&self.original_model, &signature));

        let added_event = json!({
            "type": "response.output_item.added",
            "output_index": index,
            "item": {
                "id": fc_item_id,
                "type": "function_call",
                "status": "in_progress",
                "name": name,
                "call_id": id
            }
        });
        events.push(
            Event::default()
                .event("response.output_item.added")
                .data(added_event.to_string()),
        );

        let done_args = json!({
            "type": "response.function_call_arguments.done",
            "item_id": fc_item_id,
            "output_index": index,
            "name": name,
            "arguments": arguments
        });
        events.push(
            Event::default()
                .event("response.function_call_arguments.done")
                .data(done_args.to_string()),
        );

        let done_item = json!({
            "type": "response.output_item.done",
            "output_index": index,
            "item": {
                "id": fc_item_id,
                "type": "function_call",
                "status": "completed",
                "name": name,
                "call_id": id,
                "arguments": arguments,
                "thought_signature": thought_signature
            }
        });
        events.push(
            Event::default()
                .event("response.output_item.done")
                .data(done_item.to_string()),
        );

        self.output_items.push((
            index,
            ResponseOutputItem::FunctionCall {
                id: fc_item_id,
                status: "completed".to_string(),
                name,
                call_id: id,
                arguments,
                thought_signature,
            },
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::provider::antigravity::sse::SseSerializer;
    use axum::response::{IntoResponse, sse::Sse};
    use serde_json::Value;
    use std::convert::Infallible;

    async fn serialize(input: Vec<AntigravityEvent>) -> Vec<Value> {
        let mut serializer = ResponsesStreamSerializer {
            resp_id: "resp_test".into(),
            msg_id: "msg_first".into(),
            created: 0,
            original_model: "test-model".into(),
            message_in_progress: false,
            message_index: None,
            next_output_index: 0,
            full_text: String::new(),
            provider_parts: Vec::new(),
            usage: ResponseUsage::default(),
            output_items: Vec::new(),
            incomplete: false,
        };
        let mut events = serializer.initial();
        for event in input {
            events.extend(serializer.handle(event));
        }
        events.extend(serializer.handle(AntigravityEvent::Finish {
            reason: "STOP".into(),
        }));
        events.extend(serializer.finish());
        let stream = tokio_stream::iter(events.into_iter().map(Ok::<_, Infallible>));
        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let wire = std::str::from_utf8(&body).unwrap();
        wire.split("\n\n")
            .filter(|frame| !frame.is_empty())
            .map(|frame| {
                let event = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("event:"))
                    .unwrap()
                    .trim();
                let data = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data:"))
                    .unwrap()
                    .trim();
                let value: Value = serde_json::from_str(data).unwrap();
                assert_eq!(value["type"], event);
                value
            })
            .collect()
    }

    fn tool() -> AntigravityEvent {
        AntigravityEvent::ToolCall {
            id: "call_1".into(),
            name: "lookup".into(),
            arguments: r#"{"q":"x"}"#.into(),
            thought_signature: Some("tool-sig".into()),
        }
    }

    fn signed(text: &str, reasoning: bool, signature: &str) -> AntigravityEvent {
        AntigravityEvent::SignedPart {
            text: text.into(),
            reasoning,
            signature: signature.into(),
        }
    }

    fn assert_types(events: &[Value], expected: &[&str]) {
        let types: Vec<&str> = events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, expected);
    }

    #[tokio::test]
    async fn signed_text_before_tool_is_closed_and_preserved() {
        let events = serialize(vec![signed("before", false, "text-sig"), tool()]).await;
        assert_types(
            &events,
            &[
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.provider_part.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ],
        );
        let output = events[11]["response"]["output"].as_array().unwrap();
        assert_eq!(events[11]["response"]["output_text"], "before");
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["id"], "msg_first");
        assert_eq!(output[0]["content"][0]["text"], "before");
        assert_eq!(
            output[0]["provider_parts"],
            json!([{
                "type": "text", "text": "before",
                "thought_signature": r#"["antigravity","test-model","text-sig"]"#
            }])
        );
        assert_eq!(events[3]["part"], output[0]["provider_parts"][0]);
        assert_eq!(events[5]["text"], "before");
        assert_eq!(events[6]["part"]["text"], "before");
        assert_eq!(events[7]["item"], output[0]);
        assert_eq!(events[8]["output_index"], 1);
        assert_eq!(events[10]["item"], output[1]);
        assert_eq!(output[1]["type"], "function_call");
        assert_eq!(output[1]["call_id"], "call_1");
        assert_eq!(output[1]["arguments"], r#"{"q":"x"}"#);
        assert_eq!(
            output[1]["thought_signature"],
            r#"["antigravity","test-model","tool-sig"]"#
        );
    }

    #[tokio::test]
    async fn text_after_tool_opens_distinct_message_with_its_own_replay_parts() {
        let events = serialize(vec![
            AntigravityEvent::Text("before".into()),
            signed("thinking", true, "reasoning-sig"),
            tool(),
            signed("after", false, "after-sig"),
            AntigravityEvent::Text("!".into()),
        ])
        .await;
        assert_types(
            &events,
            &[
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.provider_part.delta",
                "response.output_text.delta",
                "response.provider_part.delta",
                "response.reasoning_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.provider_part.delta",
                "response.output_text.delta",
                "response.provider_part.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ],
        );
        let output = events[22]["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 3);
        assert_eq!(events[22]["response"]["output_text"], "beforeafter!");
        assert_eq!(output[0]["content"][0]["text"], "before");
        assert_eq!(output[2]["content"][0]["text"], "after!");
        assert_eq!(
            output[0]["provider_parts"],
            json!([
                {"type": "text", "text": "before"},
                {"type": "reasoning", "text": "thinking",
                    "thought_signature": r#"["antigravity","test-model","reasoning-sig"]"#}
            ])
        );
        assert_eq!(
            output[2]["provider_parts"],
            json!([
                {"type": "text", "text": "after",
                    "thought_signature": r#"["antigravity","test-model","after-sig"]"#},
                {"type": "text", "text": "!"}
            ])
        );
        assert_eq!(events[9]["item"], output[0]);
        assert_eq!(events[12]["item"], output[1]);
        assert_eq!(events[21]["item"], output[2]);
        assert_eq!(output[1]["type"], "function_call");
        assert_ne!(output[0]["id"], output[2]["id"]);
        for (added, done, index) in [(1, 9, 0), (10, 12, 1), (13, 21, 2)] {
            assert_eq!(events[added]["output_index"], index);
            assert_eq!(events[done]["output_index"], index);
            assert_eq!(events[added]["item"]["id"], events[done]["item"]["id"]);
        }
        assert_eq!(events[4]["item_id"], output[0]["id"]);
        assert_eq!(events[6]["item_id"], output[0]["id"]);
        assert_eq!(events[16]["item_id"], output[2]["id"]);
        assert_eq!(events[18]["item_id"], output[2]["id"]);
        assert_eq!(events[19]["text"], "after!");
        assert_eq!(events[20]["part"]["text"], "after!");
    }
}
