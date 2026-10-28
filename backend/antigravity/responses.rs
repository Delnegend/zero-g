use axum::response::sse::Event;
use serde_json::json;

use crate::antigravity::AntigravityEvent;
use crate::responses::{ResponseContentPart, ResponseObject, ResponseOutputItem, ResponseUsage};

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ResponsesStreamSerializer {
    pub(crate) resp_id: String,
    pub(crate) msg_id: String,
    pub(crate) created: u64,
    pub(crate) original_model: String,
    pub(crate) message_in_progress: bool,
    pub(crate) message_index: Option<usize>,
    pub(crate) next_output_index: usize,
    pub(crate) full_text: String,
    pub(crate) usage: ResponseUsage,
    pub(crate) output_items: Vec<(usize, ResponseOutputItem)>,
}

impl crate::server::sse::SseSerializer for ResponsesStreamSerializer {
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

    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event> {
        let mut events = Vec::new();
        match event {
            AntigravityEvent::Text(text) => {
                if !self.message_in_progress {
                    self.message_in_progress = true;
                    let index = self.next_output_index;
                    self.next_output_index = self.next_output_index.saturating_add(1);
                    self.message_index = Some(index);

                    let added_event = json!({
                        "type": "response.output_item.added",
                        "output_index": index,
                        "item": {
                            "id": self.msg_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        }
                    });
                    events.push(
                        Event::default()
                            .event("response.output_item.added")
                            .data(added_event.to_string()),
                    );

                    let part_event = json!({
                        "type": "response.content_part.added",
                        "output_index": index,
                        "content_index": 0,
                        "part": {
                            "type": "output_text",
                            "text": ""
                        }
                    });
                    events.push(
                        Event::default()
                            .event("response.content_part.added")
                            .data(part_event.to_string()),
                    );
                }

                let index = self.message_index.unwrap_or(0);
                self.full_text.push_str(&text);
                let delta_event = json!({
                    "type": "response.output_text.delta",
                    "output_index": index,
                    "content_index": 0,
                    "delta": text,
                    "item_id": self.msg_id
                });
                events.push(
                    Event::default()
                        .event("response.output_text.delta")
                        .data(delta_event.to_string()),
                );
            }
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
            } => {
                self.tool_call_events(id, name, arguments, &mut events);
            }
            AntigravityEvent::Finish { .. } => {}
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
            status: "completed".to_string(),
            output: final_items,
            output_text: self.full_text.clone(),
            usage: Some(self.usage.clone()),
        };

        let completed_event = json!({
            "type": "response.completed",
            "response": final_response
        });
        events.push(
            Event::default()
                .event("response.completed")
                .data(completed_event.to_string()),
        );
        events
    }
}

impl ResponsesStreamSerializer {
    fn tool_call_events(
        &mut self,
        id: String,
        name: String,
        arguments: String,
        events: &mut Vec<Event>,
    ) {
        self.message_in_progress = false;
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        let fc_item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());

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
                "arguments": arguments
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
            },
        ));
    }
}
