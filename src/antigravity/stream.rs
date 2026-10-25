use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::openai::Usage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AntigravityEvent {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    Finish {
        reason: String,
    },
    Usage(Usage),
}

pub(crate) fn map_finish_reason(reason: &str, has_tools: bool) -> String {
    match reason {
        "STOP" if has_tools => "tool_calls".to_string(),
        "STOP" => "stop".to_string(),
        "MAX_TOKENS" => "length".to_string(),
        "SAFETY" => "content_filter".to_string(),
        other => other.to_lowercase(),
    }
}

#[derive(Default)]
pub(crate) struct EventDecoder {
    buffer: Vec<u8>,
}

impl EventDecoder {
    pub(crate) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<AntigravityEvent> {
        self.buffer.extend_from_slice(chunk);

        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end();
            parse_line(line, &mut events);
        }

        events
    }
}

fn parse_line(line: &str, events: &mut Vec<AntigravityEvent>) {
    let Some(data) = line.strip_prefix("data: ") else {
        return;
    };

    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
        return;
    };
    let Some(response) = parsed.get("response") else {
        return;
    };

    if let Some(parts) = response
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
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
            usage.prompt_tokens = prompt_tokens as u32;
        }
        if let Some(completion_tokens) =
            metadata.get("candidatesTokenCount").and_then(Value::as_u64)
        {
            usage.completion_tokens = completion_tokens as u32;
        }
        if let Some(total_tokens) = metadata.get("totalTokenCount").and_then(Value::as_u64) {
            usage.total_tokens = total_tokens as u32;
        }
        events.push(AntigravityEvent::Usage(usage));
    }
}

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

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    for event in decoder.push_chunk(bytes.as_ref()) {
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(format!("Stream read error: {error}"))).await;
                    return;
                }
            }
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
    fn ignores_malformed_and_done() {
        let mut decoder = EventDecoder::default();
        let events = decoder.push_chunk(b"data: {not json}\ndata: [DONE]\n");

        assert_eq!(events, Vec::<AntigravityEvent>::new());
    }
}
