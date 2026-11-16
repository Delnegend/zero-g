use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::body::Bytes;
use axum::response::{IntoResponse, Json, Response, sse::Sse};
use tokio_stream::wrappers::ReceiverStream;

use super::protocol::usage_from_stats;
use super::upstream::fetch_chatjimmy;
use crate::openai::{AssistantMessage, ChatCompletionRequest, ChatCompletionResponse, Choice};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::registry;
use crate::server::AppState;

use axum::response::sse::Event;

use crate::openai::{ChatCompletionChunk, ChunkChoice, ChunkDelta, Usage};

/// Splits generated text into natural phrase-sized chunks for smooth streaming.
pub(super) fn split_stream_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_inclusive(|c: char| c.is_whitespace()) {
        current.push_str(word);
        if current.len() >= 30 {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Encodes a typed `ChatCompletionChunk` into an SSE event.
pub(super) fn chunk_event(
    id: &str,
    created: u64,
    model: &str,
    delta: ChunkDelta,
    finish_reason: Option<&str>,
    usage: Option<Usage>,
) -> Option<Event> {
    let is_pure_usage = usage.is_some()
        && delta.role.is_none()
        && delta.content.is_none()
        && finish_reason.is_none();
    let chunk = ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: if is_pure_usage {
            vec![]
        } else {
            vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason: finish_reason.map(str::to_owned),
            }]
        },
        usage,
    };
    serde_json::to_string(&chunk)
        .ok()
        .map(|data| Event::default().data(data))
}

/// Produce either a buffered completion or its simulated SSE stream.
#[allow(clippy::too_many_lines)]
pub(super) async fn respond(
    state: AppState,
    provider: registry::Provider,
    identity: Option<Extension<RequestIdentity>>,
    req: ChatCompletionRequest,
    payload_bytes: Bytes,
) -> Response {
    let is_stream = req.stream.unwrap_or(false);
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if is_stream {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
        let model = req.model.clone();
        let settings = state.settings.clone();
        let provider_clone = provider.clone();
        let id_for_task = id.clone();
        let identity_ref = identity.map(|Extension(id)| id);

        tokio::spawn(async move {
            let first = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: Some(String::new()),
                    ..Default::default()
                },
                None,
                None,
            );
            if let Some(event) = first
                && tx.send(Ok(event)).await.is_err()
            {
                return;
            }

            let result = fetch_chatjimmy(
                &settings,
                &provider_clone,
                payload_bytes,
                identity_ref.as_ref(),
            )
            .await;

            let (content, token_stats) = match result {
                Ok(data) => data,
                Err(err) => {
                    let err_data = serde_json::json!({
                        "error": {
                            "message": err.message(),
                            "type": err.kind(),
                            "code": err.code()
                        }
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("error")
                            .data(err_data.to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            };

            let usage = token_stats.map(usage_from_stats);

            for chunk_text in split_stream_chunks(&content) {
                let chunk = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta {
                        content: Some(chunk_text),
                        ..Default::default()
                    },
                    None,
                    None,
                );
                if let Some(event) = chunk
                    && tx.send(Ok(event)).await.is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let stop = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta::default(),
                Some("stop"),
                None,
            );
            if let Some(event) = stop {
                let _ = tx.send(Ok(event)).await;
            }

            if let Some(usage) = usage {
                let usage_event = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta::default(),
                    None,
                    Some(usage),
                );
                if let Some(event) = usage_event {
                    let _ = tx.send(Ok(event)).await;
                }
            }

            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        });

        Sse::new(ReceiverStream::new(rx)).into_response()
    } else {
        let (content, token_stats) = match fetch_chatjimmy(
            &state.settings,
            &provider,
            payload_bytes,
            identity.as_ref().map(|Extension(id)| id),
        )
        .await
        {
            Ok(data) => data,
            Err(err) => return err.into_response(),
        };

        let response = ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created: now,
            model: req.model,
            choices: vec![Choice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    reasoning_content: None,
                    tool_calls: None,
                    provider_parts: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: token_stats.map(usage_from_stats),
        };
        Json(response).into_response()
    }
}
