use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::openai::Usage;

/// Decoded content, completion, and usage events from Antigravity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AntigravityEvent {
    /// A generated text fragment.
    Text(String),
    /// A generated reasoning fragment.
    Reasoning(String),
    /// Signed text or reasoning part to preserve for subsequent requests.
    SignedPart {
        /// Text carried by the part, which may be empty.
        text: String,
        /// Whether this part contains reasoning.
        reasoning: bool,
        /// Opaque upstream signature.
        signature: String,
    },
    /// A generated function call.
    ToolCall {
        /// The upstream or generated call identifier.
        id: String,
        /// The function name.
        name: String,
        /// The function arguments serialized as JSON.
        arguments: String,
        /// Opaque signature attached to the upstream function call.
        thought_signature: Option<String>,
    },
    /// The upstream completion signal.
    Finish {
        /// The upstream finish reason.
        reason: String,
    },
    /// Token usage reported by the upstream service.
    Usage(Usage),
}

/// Maps an upstream finish reason to an OpenAI-compatible reason.
pub(super) fn map_finish_reason(reason: &str, has_tools: bool) -> String {
    match reason {
        "STOP" if has_tools => "tool_calls".to_string(),
        "STOP" => "stop".to_string(),
        "MAX_TOKENS" => "length".to_string(),
        "SAFETY" => "content_filter".to_string(),
        other => other.to_lowercase(),
    }
}

/// Buffers upstream bytes and decodes complete SSE lines.
#[derive(Default)]
struct EventDecoder {
    /// Bytes not yet consumed as complete lines.
    buffer: Vec<u8>,
    /// Whether an invalid or oversized event was encountered.
    error: Option<String>,
}

impl EventDecoder {
    /// Appends bytes and decodes complete lines, clearing oversized buffers.
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<AntigravityEvent> {
        const MAX_BUFFER_BYTES: usize = 1024 * 1024;
        if self.buffer.len().saturating_add(chunk.len()) > MAX_BUFFER_BYTES {
            self.buffer.clear();
            self.error = Some("Upstream event exceeded size limit".to_string());
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);

        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end();
            if let Err(error) = parse_line(line, &mut events) {
                self.error = Some(error);
                break;
            }
        }

        events
    }
}

/// Extracts events from one upstream SSE data line.
fn parse_line(line: &str, events: &mut Vec<AntigravityEvent>) -> Result<(), String> {
    let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
        return Ok(());
    };

    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }

    let parsed = serde_json::from_str::<Value>(data)
        .map_err(|error| format!("Invalid upstream event: {error}"))?;
    if let Some(error) = parsed.get("error") {
        return Err(format!("Upstream error: {error}"));
    }
    let Some(response) = parsed.get("response") else {
        return Err("Upstream event missing response".to_string());
    };

    if let Some(parts) = response
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            let signature = part.get("thoughtSignature").and_then(Value::as_str);
            let text = part.get("text").and_then(Value::as_str);
            let reasoning = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(signature) = signature.filter(|_| part.get("functionCall").is_none()) {
                events.push(AntigravityEvent::SignedPart {
                    text: text.unwrap_or_default().to_string(),
                    reasoning,
                    signature: signature.to_string(),
                });
            } else if let Some(text) = text {
                if reasoning {
                    events.push(AntigravityEvent::Reasoning(text.to_string()));
                } else {
                    events.push(AntigravityEvent::Text(text.to_string()));
                }
            }

            if let Some(function_call) = part.get("functionCall") {
                let name = function_call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = function_call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string();
                let id = function_call.get("id").and_then(Value::as_str).map_or_else(
                    || format!("call_{}", uuid::Uuid::new_v4().simple()),
                    std::string::ToString::to_string,
                );

                events.push(AntigravityEvent::ToolCall {
                    id,
                    name,
                    arguments,
                    thought_signature: part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
    }

    if let Some(reason) = response
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str)
    {
        events.push(AntigravityEvent::Finish {
            reason: reason.to_string(),
        });
    }

    if let Some(metadata) = response.get("usageMetadata") {
        let mut usage = Usage::default();
        if let Some(prompt_tokens) = metadata.get("promptTokenCount").and_then(Value::as_u64) {
            usage.prompt_tokens = u32::try_from(prompt_tokens).unwrap_or(u32::MAX);
        }
        if let Some(completion_tokens) =
            metadata.get("candidatesTokenCount").and_then(Value::as_u64)
        {
            usage.completion_tokens = u32::try_from(completion_tokens).unwrap_or(u32::MAX);
        }
        if let Some(total_tokens) = metadata.get("totalTokenCount").and_then(Value::as_u64) {
            usage.total_tokens = u32::try_from(total_tokens).unwrap_or(u32::MAX);
        }
        events.push(AntigravityEvent::Usage(usage));
    }
    Ok(())
}

/// Spawns a task that decodes byte chunks into events or a stream error.
pub(crate) fn spawn_event_stream<S, B, E>(
    stream: S,
) -> mpsc::Receiver<Result<AntigravityEvent, String>>
where
    S: futures_util::Stream<Item = Result<B, E>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (tx, rx) = mpsc::channel(100);
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(stream);
        let mut decoder = EventDecoder::default();
        let mut finished = false;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    for event in decoder.push_chunk(bytes.as_ref()) {
                        if matches!(event, AntigravityEvent::Finish { .. }) {
                            finished = true;
                        }
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                    if let Some(error) = decoder.error.take() {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(format!("Stream read error: {error}"))).await;
                    return;
                }
            }
        }
        if !decoder.buffer.is_empty() {
            let _ = tx.send(Err("Truncated upstream event".to_string())).await;
        } else if !finished {
            let _ = tx
                .send(Err(
                    "Upstream stream ended without a finish reason".to_string()
                ))
                .await;
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_text_and_reasoning() {
        let mut decoder = EventDecoder::default();
        let payload = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [
                            {"text": "thinking", "thought": true},
                            {"text": "answer"}
                        ]
                    }
                }]
            }
        });
        let line = format!("data: {payload}\n");

        let events = decoder.push_chunk(line.as_bytes());

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            AntigravityEvent::Reasoning("thinking".to_string())
        );
        assert_eq!(events[1], AntigravityEvent::Text("answer".to_string()));
    }

    #[test]
    fn decodes_tool_call_and_finish() {
        let mut decoder = EventDecoder::default();
        let payload = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "name": "get_weather",
                                "args": {"city": "Paris"}
                            }
                        }]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let line = format!("data: {payload}\n");

        let events = decoder.push_chunk(line.as_bytes());

        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            AntigravityEvent::ToolCall { name, arguments, .. }
                if name == "get_weather" && arguments == "{\"city\":\"Paris\"}"
        ));
        assert_eq!(
            events[1],
            AntigravityEvent::Finish {
                reason: "STOP".to_string(),
            }
        );
    }

    #[test]
    fn decodes_usage() {
        let mut decoder = EventDecoder::default();
        let payload = json!({
            "response": {
                "candidates": [],
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5,
                    "totalTokenCount": 15
                }
            }
        });
        let line = format!("data: {payload}\n");

        let events = decoder.push_chunk(line.as_bytes());

        assert_eq!(
            events,
            vec![AntigravityEvent::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            })]
        );
    }

    #[test]
    fn saturates_oversized_usage_counts() {
        let mut decoder = EventDecoder::default();
        let payload = json!({
            "response": {
                "usageMetadata": {
                    "promptTokenCount": u64::MAX,
                    "candidatesTokenCount": u64::MAX,
                    "totalTokenCount": u64::MAX
                }
            }
        });
        let events = decoder.push_chunk(format!("data: {payload}\n").as_bytes());
        assert_eq!(
            events,
            vec![AntigravityEvent::Usage(Usage {
                prompt_tokens: u32::MAX,
                completion_tokens: u32::MAX,
                total_tokens: u32::MAX,
            })]
        );
    }

    #[test]
    fn reassembles_split_chunks() {
        let mut decoder = EventDecoder::default();
        let line = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}}\n";

        let midpoint = line.len() / 2;
        assert_eq!(
            decoder.push_chunk(&line.as_bytes()[..midpoint]),
            Vec::<AntigravityEvent>::new()
        );
        let events = decoder.push_chunk(&line.as_bytes()[midpoint..]);

        assert_eq!(events, vec![AntigravityEvent::Text("hi".to_string())]);
    }

    #[test]
    fn preserves_utf8_split_across_bytes() {
        let mut decoder = EventDecoder::default();
        let payload = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{"text": "héllo"}]
                    }
                }]
            }
        });
        let line = format!("data: {payload}\n");

        let mut events = Vec::new();
        for byte in line.as_bytes() {
            events.extend(decoder.push_chunk(std::slice::from_ref(byte)));
        }

        assert_eq!(events, vec![AntigravityEvent::Text("héllo".to_string())]);
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(map_finish_reason("STOP", true), "tool_calls");
        assert_eq!(map_finish_reason("STOP", false), "stop");
        assert_eq!(map_finish_reason("MAX_TOKENS", false), "length");
        assert_eq!(map_finish_reason("SAFETY", false), "content_filter");
        assert_eq!(map_finish_reason("OTHER", false), "other");
    }

    #[test]
    fn ignores_malformed_and_done() {
        let mut decoder = EventDecoder::default();
        let events = decoder.push_chunk(b"data: {not json}\ndata: [DONE]\n");

        assert_eq!(events, Vec::<AntigravityEvent>::new());
    }
}
