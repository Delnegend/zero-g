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
