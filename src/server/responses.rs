use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::antigravity::AntigravityClient;
use crate::antigravity::AntigravityEvent;
use crate::openai::ChatCompletionRequest;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::AppState;

pub(crate) async fn responses_handler(
    State(state): State<AppState>,
    Json(req): Json<ResponseCreateRequest>,
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
    let original_model = req.model.clone();
    let chat_req = req.into_chat_request();

    if is_streaming {
        handle_responses_streaming(state, creds, chat_req, original_model).await
    } else {
        match state.client.complete_non_streaming(&creds, &chat_req).await {
            Ok(resp) => {
                let mut output_items = Vec::new();
                let mut output_text = String::new();

                for choice in &resp.choices {
                    if let Some(content) = &choice.message.content {
                        output_text.push_str(content);
                        output_items.push(ResponseOutputItem::Message {
                            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                            status: "completed".to_string(),
                            role: choice.message.role.clone(),
                            content: vec![ResponseContentPart::OutputText {
                                text: content.clone(),
                            }],
                        });
                    }
                    if let Some(tool_calls) = &choice.message.tool_calls {
                        for tc in tool_calls {
                            output_items.push(ResponseOutputItem::FunctionCall {
                                id: format!("fc_{}", uuid::Uuid::new_v4().simple()),
                                status: "completed".to_string(),
                                name: tc.function.name.clone(),
                                call_id: tc.id.clone(),
                                arguments: tc.function.arguments.clone(),
                            });
                        }
                    }
                }

                let usage = resp.usage.map(ResponseUsage::from);

                let response_obj = ResponseObject {
                    id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
                    object: "response".to_string(),
                    created_at: resp.created,
                    model: original_model,
                    status: "completed".to_string(),
                    output: output_items,
                    output_text,
                    usage,
                };

                Json(response_obj).into_response()
            }
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

async fn handle_responses_streaming(
    state: AppState,
    creds: crate::auth::Credentials,
    chat_req: ChatCompletionRequest,
    original_model: String,
) -> Response {
    let wire_model = chat_req.model.clone();
    let is_claude = wire_model.contains("claude");
    let payload =
        AntigravityClient::build_request_payload(&chat_req, &creds.project_id, &wire_model);

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

    let resp_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());

    let serializer = ResponsesStreamSerializer {
        resp_id,
        msg_id,
        created,
        original_model,
        message_in_progress: false,
        message_index: None,
        next_output_index: 0,
        full_text: String::new(),
        usage: ResponseUsage::default(),
        output_items: Vec::new(),
    };

    crate::server::sse::spawn_sse(serializer, upstream_resp)
}

#[allow(clippy::struct_excessive_bools)]
struct ResponsesStreamSerializer {
    resp_id: String,
    msg_id: String,
    created: u64,
    original_model: String,
    message_in_progress: bool,
    message_index: Option<usize>,
    next_output_index: usize,
    full_text: String,
    usage: ResponseUsage,
    output_items: Vec<(usize, ResponseOutputItem)>,
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
                    self.next_output_index += 1;
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
                self.message_in_progress = false;
                let index = self.next_output_index;
                self.next_output_index += 1;
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
