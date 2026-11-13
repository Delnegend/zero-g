//! Command Code `/alpha/generate` stream decoding.

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::provider::common::translation::Event;

/// Largest single stream line accepted from the upstream.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Extract a readable message from a Command Code error value.
pub(super) fn error_message(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.get("message").and_then(Value::as_str))
        .filter(|message| !message.is_empty())
}

/// Decode one stream line; lines without client-visible output yield no events.
fn decode(line: &[u8]) -> Result<Vec<Event>, String> {
    let line = std::str::from_utf8(line)
        .map_err(|_| "Invalid UTF-8 in Command Code stream")?
        .trim();
    let line = line.strip_prefix("data:").map_or(line, str::trim_start);
    if line.is_empty() || line.starts_with(':') || line.starts_with("event:") || line == "[DONE]" {
        return Ok(Vec::new());
    }
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return Ok(Vec::new());
    };
    let text = |field| {
        event
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Ok(match event.get("type").and_then(Value::as_str) {
        Some("text-delta") => text("text").map(Event::Text).into_iter().collect(),
        Some("reasoning-delta") => text("text").map(Event::Thinking).into_iter().collect(),
        Some("tool-call") if event.get("providerExecuted") != Some(&Value::Bool(true)) => {
            let input = event.get("input").or_else(|| event.get("args"));
            let arguments = match input {
                Some(Value::String(raw)) => raw.clone(),
                Some(value) => value.to_string(),
                None => "{}".to_owned(),
            };
            vec![Event::Tool {
                id: text("toolCallId").unwrap_or_default(),
                name: text("toolName").unwrap_or_default(),
                arguments,
            }]
        }
        Some("finish") => {
            if let Some(reason) = event.get("rawFinishReason").and_then(Value::as_str) {
                let reason = reason.to_ascii_lowercase();
                if ["network", "connection", "upstream"]
                    .iter()
                    .any(|kind| reason.starts_with(kind) && reason.ends_with("error"))
                {
                    return Err(format!("Command Code upstream failed: {reason}"));
                }
            }
            let usage = event.get("totalUsage");
            let count = |field| {
                usage
                    .and_then(|usage| usage.get(field))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            };
            vec![
                Event::Usage {
                    input: count("inputTokens"),
                    output: count("outputTokens"),
                },
                Event::Done,
            ]
        }
        Some("error") => {
            return Err(error_message(event.get("error").unwrap_or(&Value::Null))
                .or_else(|| error_message(&event))
                .unwrap_or("Command Code stream error")
                .to_owned());
        }
        Some("abort") => return Err("Command Code aborted the response".into()),
        _ => Vec::new(),
    })
}

/// Forward decoded events until the upstream finishes, fails, or the client leaves.
pub(super) async fn consume(response: reqwest::Response, tx: mpsc::Sender<Result<Event, String>>) {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let failure = loop {
        let chunk = tokio::select! {
            () = tx.closed() => return,
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break "Command Code stream ended before completion".to_owned();
        };
        let Ok(chunk) = chunk else {
            break "Command Code stream failed".to_owned();
        };
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            match decode(&line) {
                Ok(events) => {
                    for event in events {
                        let done = matches!(event, Event::Done);
                        if tx.send(Ok(event)).await.is_err() || done {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
        if buffer.len() > MAX_LINE_BYTES {
            break "Command Code stream event too large".to_owned();
        }
    };
    let _ = tx.send(Err(failure)).await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The CLI's stream parts become text, reasoning, tool, usage, and completion events.
    #[test]
    fn decodes_generate_stream_parts() {
        let text = decode(br#"{"type":"text-delta","id":"0","text":"Hi"}"#).unwrap();
        assert!(matches!(text.as_slice(), [Event::Text(t)] if t == "Hi"));
        let thinking = decode(br#"data: {"type":"reasoning-delta","text":"hmm"}"#).unwrap();
        assert!(matches!(thinking.as_slice(), [Event::Thinking(t)] if t == "hmm"));
        let tool = decode(
            br#"{"type":"tool-call","toolCallId":"c1","toolName":"read","input":{"path":"a"}}"#,
        )
        .unwrap();
        assert!(matches!(
            tool.as_slice(),
            [Event::Tool { id, name, arguments }]
                if id == "c1" && name == "read" && arguments == r#"{"path":"a"}"#
        ));
        let server = decode(br#"{"type":"tool-call","toolCallId":"s","toolName":"web_search","providerExecuted":true}"#).unwrap();
        assert!(server.is_empty());
        let finish = decode(br#"{"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":12,"outputTokens":3}}"#).unwrap();
        assert!(matches!(
            finish.as_slice(),
            [
                Event::Usage {
                    input: 12,
                    output: 3
                },
                Event::Done
            ]
        ));
    }

    /// Upstream failures surface as errors rather than silent completions.
    #[test]
    fn reports_stream_errors() {
        let error =
            decode(br#"{"type":"error","error":{"message":"model_not_in_plan"}}"#).unwrap_err();
        assert_eq!(error, "model_not_in_plan");
        assert!(decode(br#"{"type":"finish","rawFinishReason":"network_error"}"#).is_err());
        assert!(decode(b"not json").unwrap().is_empty());
    }
}
