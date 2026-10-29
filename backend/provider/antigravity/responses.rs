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
    /// Accumulated assistant text.
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
        if !self.message_in_progress
            && matches!(
                &event,
                AntigravityEvent::Reasoning(_)
                    | AntigravityEvent::SignedPart {
                        reasoning: true,
                        ..
                    }
            )
        {
            events.extend(self.handle(AntigravityEvent::Text(String::new())));
            self.provider_parts.pop();
        }
        let event = self.normalize_event(event, &mut events);
        match event {
            AntigravityEvent::SignedPart {
                text, reasoning, ..
            } => {
                let event = if reasoning {
                    AntigravityEvent::Reasoning(text)
                } else {
                    AntigravityEvent::Text(text)
                };
                events.extend(self.handle(event));
            }
            AntigravityEvent::Text(text) => self.text_events(&text, &mut events),
            AntigravityEvent::Reasoning(text) => {
                let reasoning_event = json!({
                    "type": "response.reasoning_text.delta",
                    "output_index": self.message_index.unwrap_or(0),
                    "content_index": 0,
                    "delta": text
                });
                events.push(
                    Event::default()
                        .event("response.reasoning_text.delta")
                        .data(reasoning_event.to_string()),
                );
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
        if self.message_in_progress {
            let index = self.message_index.unwrap_or(0);
            let done_text_event = json!({
                "type": "response.output_text.done",
                "output_index": index,
                "content_index": 0,
                "text": self.full_text
            });
            events.push(
                Event::default()
                    .event("response.output_text.done")
                    .data(done_text_event.to_string()),
            );

            let done_part_event = json!({
                "type": "response.content_part.done",
                "output_index": index,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": self.full_text
                }
            });
            events.push(
                Event::default()
                    .event("response.content_part.done")
                    .data(done_part_event.to_string()),
            );

            let msg_item = ResponseOutputItem::Message {
                id: self.msg_id.clone(),
                status: "completed".to_string(),
                role: "assistant".to_string(),
                content: vec![ResponseContentPart::OutputText {
                    text: self.full_text.clone(),
                }],
                provider_parts: Some(self.provider_parts.clone()),
            };
            let done_item_event = json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": msg_item
            });
            events.push(
                Event::default()
                    .event("response.output_item.done")
                    .data(done_item_event.to_string()),
            );

            self.output_items.push((index, msg_item));
        }

        self.output_items.sort_by_key(|(index, _)| *index);
        let final_items: Vec<ResponseOutputItem> = self
            .output_items
            .clone()
            .into_iter()
            .map(|(_, item)| item)
            .collect();

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
            output_text: self.full_text.clone(),
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
    /// Emits the message opening and text delta for a generated fragment.
    fn text_events(&mut self, text: &str, events: &mut Vec<Event>) {
        if !self.message_in_progress {
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
                "type": "response.content_part.added", "output_index": index,
                "content_index": 0, "part": {"type": "output_text", "text": ""}
            });
            events.push(
                Event::default()
                    .event("response.content_part.added")
                    .data(part_event.to_string()),
            );
        }
        let index = self.message_index.unwrap_or(0);
        self.full_text.push_str(text);
        let delta = json!({
            "type": "response.output_text.delta", "output_index": index,
            "content_index": 0, "delta": text, "item_id": self.msg_id
        });
        events.push(
            Event::default()
                .event("response.output_text.delta")
                .data(delta.to_string()),
        );
    }

    /// Records a replayable provider part and normalizes signed content.
    fn normalize_event(
        &mut self,
        event: AntigravityEvent,
        events: &mut Vec<Event>,
    ) -> AntigravityEvent {
        let (event, part) = match event {
            AntigravityEvent::SignedPart {
                text,
                reasoning,
                signature,
            } => {
                let part =
                    super::provider_part(&self.original_model, &text, reasoning, Some(&signature));
                let event = if reasoning {
                    AntigravityEvent::Reasoning(text)
                } else {
                    AntigravityEvent::Text(text)
                };
                (event, Some(part))
            }
            other => {
                let part = match &other {
                    AntigravityEvent::Text(text) => Some(super::provider_part(
                        &self.original_model,
                        text,
                        false,
                        None,
                    )),
                    AntigravityEvent::Reasoning(text) => {
                        Some(super::provider_part(&self.original_model, text, true, None))
                    }
                    _ => None,
                };
                (other, part)
            }
        };
        if let Some(part) = part {
            self.provider_parts.push(part.clone());
            events.push(
                Event::default().event("response.provider_part.delta").data(
                    json!({"type": "response.provider_part.delta", "part": part}).to_string(),
                ),
            );
        }
        event
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
        self.message_in_progress = false;
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
