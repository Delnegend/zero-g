use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::antigravity::AntigravityClient;
use crate::antigravity::AntigravityEvent;
use crate::antigravity::spawn_event_stream;
use crate::openai::ChatCompletionRequest;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};
use crate::server::{AppState, check_auth};

pub(crate) async fn responses_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ResponseCreateRequest>,
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
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": { "message": e } })),
            )
                .into_response(),
        }
    }
}

async fn handle_responses_streaming(
    state: AppState,
    creds: crate::auth::Credentials,
    chat_req: ChatCompletionRequest,
    original_model: String,
) -> Response {
    let wire_model = AntigravityClient::map_model_id(&chat_req.model);
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
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": { "message": e } })),
            )
                .into_response();
        }
    };

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

    let resp_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());

    tokio::spawn(async move {
        let created_event = json!({
            "type": "response.created",
            "response": {
                "id": resp_id,
                "object": "response",
                "created_at": created,
                "status": "in_progress",
                "model": original_model
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.created")
                .data(created_event.to_string())))
            .await;

        let mut byte_stream = spawn_event_stream(upstream_resp.bytes_stream());

        let mut item_started = false;
        let mut message_index: Option<usize> = None;
        let mut next_output_index: usize = 0;
        let mut full_text = String::new();
        let mut usage = ResponseUsage::default();
        let mut output_items: Vec<(usize, ResponseOutputItem)> = Vec::new();

        while let Some(event) = byte_stream.recv().await {
            match event {
                Ok(AntigravityEvent::Text(text)) => {
                    if !item_started {
                        item_started = true;
                        let index = next_output_index;
                        next_output_index += 1;
                        message_index = Some(index);

                        let added_event = json!({
                            "type": "response.output_item.added",
                            "output_index": index,
                            "item": {
                                "id": msg_id,
                                "type": "message",
                                "status": "in_progress",
                                "role": "assistant",
                                "content": []
                            }
                        });
                        let _ = tx
                            .send(Ok(Event::default()
                                .event("response.output_item.added")
                                .data(added_event.to_string())))
                            .await;

                        let part_event = json!({
                            "type": "response.content_part.added",
                            "output_index": index,
                            "content_index": 0,
                            "part": {
                                "type": "output_text",
                                "text": ""
                            }
                        });
                        let _ = tx
                            .send(Ok(Event::default()
                                .event("response.content_part.added")
                                .data(part_event.to_string())))
                            .await;
                    }

                    let index = message_index.unwrap_or(0);
                    full_text.push_str(&text);
                    let delta_event = json!({
                        "type": "response.output_text.delta",
                        "output_index": index,
                        "content_index": 0,
                        "delta": text
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.output_text.delta")
                            .data(delta_event.to_string())))
                        .await;
                }
                Ok(AntigravityEvent::Reasoning(text)) => {
                    let reasoning_event = json!({
                        "type": "response.reasoning_text.delta",
                        "output_index": 0,
                        "content_index": 0,
                        "delta": text
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.reasoning_text.delta")
                            .data(reasoning_event.to_string())))
                        .await;
                }
                Ok(AntigravityEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    let index = next_output_index;
                    next_output_index += 1;
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
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.output_item.added")
                            .data(added_event.to_string())))
                        .await;

                    let done_args = json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": index,
                        "name": name,
                        "arguments": arguments
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.function_call_arguments.done")
                            .data(done_args.to_string())))
                        .await;

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
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.output_item.done")
                            .data(done_item.to_string())))
                        .await;

                    output_items.push((
                        index,
                        ResponseOutputItem::FunctionCall {
                            id: fc_item_id.clone(),
                            status: "completed".to_string(),
                            name: name.clone(),
                            call_id: id.clone(),
                            arguments: arguments.clone(),
                        },
                    ));
                }
                Ok(AntigravityEvent::Finish { .. }) => {}
                Ok(AntigravityEvent::Usage(value)) => {
                    usage = ResponseUsage::from(value);
                }
                Err(error) => {
                    error!("Error reading upstream bytes: {error}");
                    break;
                }
            }
        }

        if item_started {
            let index = message_index.unwrap_or(0);
            let done_text_event = json!({
                "type": "response.output_text.done",
                "output_index": index,
                "content_index": 0,
                "text": full_text
            });
            let _ = tx
                .send(Ok(Event::default()
                    .event("response.output_text.done")
                    .data(done_text_event.to_string())))
                .await;

            let done_part_event = json!({
                "type": "response.content_part.done",
                "output_index": index,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": full_text
                }
            });
            let _ = tx
                .send(Ok(Event::default()
                    .event("response.content_part.done")
                    .data(done_part_event.to_string())))
                .await;

            let msg_item = ResponseOutputItem::Message {
                id: msg_id,
                status: "completed".to_string(),
                role: "assistant".to_string(),
                content: vec![ResponseContentPart::OutputText {
                    text: full_text.clone(),
                }],
            };
            let done_item_event = json!({
                "type": "response.output_item.done",
                "output_index": index,
                "item": msg_item
            });
            let _ = tx
                .send(Ok(Event::default()
                    .event("response.output_item.done")
                    .data(done_item_event.to_string())))
                .await;

            output_items.push((index, msg_item));
        }

        output_items.sort_by_key(|(index, _)| *index);
        let final_items: Vec<ResponseOutputItem> =
            output_items.into_iter().map(|(_, item)| item).collect();

        let final_response = ResponseObject {
            id: resp_id,
            object: "response".to_string(),
            created_at: created,
            model: original_model,
            status: "completed".to_string(),
            output: final_items,
            output_text: full_text,
            usage: Some(usage),
        };

        let completed_event = json!({
            "type": "response.completed",
            "response": final_response
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.completed")
                .data(completed_event.to_string())))
            .await;

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let sse_stream = ReceiverStream::new(rx);
    Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
