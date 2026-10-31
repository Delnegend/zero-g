use std::convert::Infallible;

use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::openai::{ChatCompletionRequest, ContentPart, MessageContent};

/// Translate chat history and options without introducing provider-specific defaults.
// JSON object indexing inserts optional fields; this function maps one request.
#[allow(clippy::indexing_slicing, clippy::too_many_lines)]
pub(super) fn chat_to_responses(req: &ChatCompletionRequest) -> Value {
    let mut input = Vec::new();
    for message in &req.messages {
        if message.role == "tool" {
            input.push(json!({"type":"function_call_output", "call_id":message.tool_call_id, "output":message.content.as_ref().map(MessageContent::as_text).unwrap_or_default()}));
            continue;
        }
        if message.role == "assistant"
            && let Some(parts) = &message.provider_parts
        {
            // Replay opaque reasoning; content and calls use standard chat fields.
            input.extend(
                parts
                    .iter()
                    .filter(|p| p.get("type").and_then(Value::as_str) == Some("reasoning"))
                    .cloned(),
            );
        }
        if let Some(content) = &message.content {
            let role = message.role.as_str();
            let kind = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            let parts: Vec<Value> = match content {
                MessageContent::Text(text) => vec![json!({"type":kind,"text":text})],
                MessageContent::Parts(parts) => parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text }
                        | ContentPart::InputText { text }
                        | ContentPart::OutputText { text } => json!({"type":kind,"text":text}),
                        ContentPart::ImageUrl { image_url } => {
                            json!({"type":"input_image","image_url":image_url.url})
                        }
                        _ => serde_json::to_value(part).unwrap_or(Value::Null),
                    })
                    .collect(),
            };
            input.push(json!({"type":"message","role":role,"content":parts}));
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                input.push(json!({"type":"function_call","call_id":call.id,"name":call.function.name,"arguments":call.function.arguments}));
            }
        }
    }
    let mut payload = json!({"model":req.model,"input":input,"stream":req.stream.unwrap_or(false),"include":["reasoning.encrypted_content"]});
    if let Some(v) = req.temperature {
        payload["temperature"] = json!(v);
    }
    if let Some(v) = req.top_p {
        payload["top_p"] = json!(v);
    }
    if let Some(v) = req.max_completion_tokens.or(req.max_tokens) {
        payload["max_output_tokens"] = json!(v);
    }
    if let Some(tools) = &req.tools {
        payload["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let mut value = json!({"type":tool.r#type,"name":tool.function.name});
                    if let Some(description) = &tool.function.description {
                        value["description"] = json!(description);
                    }
                    if let Some(parameters) = &tool.function.parameters {
                        value["parameters"] = parameters.clone();
                    }
                    value
                })
                .collect(),
        );
    }
    if let Some(choice) = &req.tool_choice {
        payload["tool_choice"] = choice
            .pointer("/function/name")
            .and_then(Value::as_str)
            .map_or_else(
                || choice.clone(),
                |name| json!({"type":"function","name":name}),
            );
    }
    let model = req.model.as_str();
    let supports_effort = !model.contains("reasoning")
        && !model.starts_with("grok-build")
        && (["grok-4.3", "grok-4.5", "grok-4.6", "grok-4.7"]
            .iter()
            .any(|prefix| model.starts_with(prefix))
            || model == "grok-4.20-multi-agent-0309");
    if supports_effort {
        let effort = req.reasoning_effort.as_deref().or_else(|| {
            req.reasoning
                .as_ref()
                .and_then(|r| r.get("effort"))
                .and_then(Value::as_str)
        });
        if let Some(effort) = effort {
            let extended = model.starts_with("grok-4.6")
                || model.starts_with("grok-4.7")
                || model.starts_with("grok-4.20-multi-agent");
            let mapped = match effort {
                "minimal" => "low",
                "xhigh" | "max" if !extended => "high",
                _ => effort,
            };
            payload["reasoning"] = json!({"effort":mapped});
        }
    }
    payload
}

/// Translate Responses token accounting into chat usage fields.
// JSON object indexing inserts optional usage details.
#[allow(clippy::indexing_slicing)]
fn usage(response: &Value) -> Option<Value> {
    let u = response.get("usage")?;
    let input = u.get("input_tokens")?.as_u64()?;
    let output = u.get("output_tokens")?.as_u64()?;
    let mut result = json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":u.get("total_tokens").and_then(Value::as_u64).unwrap_or_else(|| input.saturating_add(output))});
    if let Some(details) = u.get("input_tokens_details") {
        result["prompt_tokens_details"] = details.clone();
    }
    if let Some(details) = u.get("output_tokens_details") {
        result["completion_tokens_details"] = details.clone();
    }
    Some(result)
}

/// Derive a chat finish reason from the terminal response status.
fn finish_reason(response: &Value, has_calls: bool) -> Result<&'static str, String> {
    match response.get("status").and_then(Value::as_str) {
        Some("completed") => Ok(if has_calls { "tool_calls" } else { "stop" }),
        Some("incomplete") => match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => Ok("length"),
            Some("content_filter") => Ok("content_filter"),
            _ => Err("Grok response incomplete without a recognized reason".into()),
        },
        _ => Err(format!(
            "Grok response did not complete: {}",
            response.get("error").unwrap_or(response)
        )),
    }
}

/// Convert a completed Responses object to the public chat-completions shape.
// JSON object indexing inserts optional response fields.
#[allow(clippy::indexing_slicing)]
pub(super) fn responses_to_chat(response: &Value, model: &str) -> Result<Value, String> {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Grok response missing id")?;
    let created = response
        .get("created_at")
        .and_then(Value::as_u64)
        .ok_or("Grok response missing created_at")?;
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or("Grok response missing output")?;
    let mut text = String::new();
    let mut refusal = String::new();
    let mut reasoning = String::new();
    let mut parts = Vec::new();
    let mut calls = Vec::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                if let Some(s) = part.get("text").and_then(Value::as_str) {
                                    text.push_str(s);
                                }
                            }
                            Some("refusal") => {
                                if let Some(s) = part.get("refusal").and_then(Value::as_str) {
                                    refusal.push_str(s);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .ok_or("Grok function call missing call_id")?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Grok function call missing name")?;
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or("Grok function call missing arguments")?;
                calls.push(json!({"id":call_id,"type":"function","function":{"name":name,"arguments":arguments}}));
            }
            Some("reasoning") => {
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    for part in summary {
                        if let Some(s) = part.get("text").and_then(Value::as_str) {
                            reasoning.push_str(s);
                        }
                    }
                }
                parts.push(item.clone());
            }
            _ => {}
        }
    }
    let reason = finish_reason(response, !calls.is_empty())?;
    let mut message = json!({"role":"assistant","content":if text.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !refusal.is_empty() {
        message["refusal"] = json!(refusal);
    }
    if !parts.is_empty() {
        message["provider_parts"] = json!(parts);
    }
    let mut result = json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":reason}]});
    if let Some(u) = usage(response) {
        result["usage"] = u;
    }
    Ok(result)
}

/// Maximum size of an upstream SSE line or assembled data payload.
const MAX_SSE_BYTES: usize = 1024 * 1024;

/// Event name and data of a complete upstream SSE frame.
type SseFrame = (Option<String>, String);
/// Complete frames and, on failure, the subsequent decoding error.
type SseDecodeResult = Result<Vec<SseFrame>, (Vec<SseFrame>, &'static str)>;

/// Incrementally decode SSE frames across arbitrary byte boundaries.
#[derive(Default)]
struct Decoder {
    /// Bytes awaiting a complete line.
    buffer: Vec<u8>,
    /// Current SSE event name.
    event: Option<String>,
    /// Joined data lines, including their separating newlines.
    data: Option<String>,
}
impl Decoder {
    /// Return complete event frames from the supplied bytes.
    fn push(&mut self, bytes: &[u8]) -> SseDecodeResult {
        let mut frames = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            let line_part = part.strip_suffix(b"\n");
            let complete = line_part.is_some();
            let line_part = line_part.unwrap_or(part);
            if line_part.len() > MAX_SSE_BYTES.saturating_sub(self.buffer.len()) {
                return Err((frames, "Grok SSE line exceeds size limit"));
            }
            self.buffer.extend_from_slice(line_part);
            if !complete {
                continue;
            }
            let line = String::from_utf8_lossy(&self.buffer);
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                if let Some(data) = self.data.take() {
                    frames.push((self.event.take(), data));
                } else {
                    self.event = None;
                }
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
            } else if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                let len = self
                    .data
                    .as_ref()
                    .map_or(0, |previous| previous.len().saturating_add(1));
                if len > MAX_SSE_BYTES || value.len() > MAX_SSE_BYTES.saturating_sub(len) {
                    return Err((frames, "Grok SSE event data exceeds size limit"));
                }
                if let Some(previous) = &mut self.data {
                    previous.push('\n');
                    previous.push_str(value);
                } else {
                    self.data = Some(value.to_owned());
                }
            }
            self.buffer.clear();
        }
        Ok(frames)
    }
}

/// Tracks a streamed function call and its emitted arguments.
struct Call {
    /// Responses output item identifier.
    item_id: String,
    /// Public function call identifier.
    public_id: String,
    /// Arguments already sent downstream.
    arguments: String,
}
/// Maintains metadata and tool state for chat SSE translation.
struct ChatStream {
    /// Upstream response identifier.
    id: String,
    /// Upstream creation timestamp.
    created: u64,
    /// Model name returned to the caller.
    model: String,
    /// Function calls observed in output order.
    calls: Vec<Call>,
    /// Whether a terminal response was processed.
    finished: bool,
    /// Whether the caller requested a final usage chunk.
    include_usage: bool,
}
impl ChatStream {
    /// Construct one chat completion chunk.
    // Values are serialized into the chunk object.
    #[allow(clippy::needless_pass_by_value)]
    fn chunk(&self, delta: Value, reason: Value) -> Value {
        json!({"id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    }
    /// Match an event to its function call.
    fn call_index(&self, event: &Value) -> Option<usize> {
        let id = event
            .get("item_id")
            .or_else(|| event.pointer("/item/id"))
            .or_else(|| event.get("call_id"))
            .and_then(Value::as_str)?;
        self.calls
            .iter()
            .position(|c| c.item_id == id || c.public_id == id)
    }
    /// Translate one Responses event into zero or more chat chunks.
    // One dispatch table handles the complete Responses event protocol.
    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, event: &Value, name: Option<&str>) -> Result<Vec<Value>, String> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .or(name)
            .unwrap_or("");
        let mut out = Vec::new();
        match kind {
            "response.created" => {
                let response = event
                    .get("response")
                    .ok_or("Grok created event missing response")?;
                response
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Grok response missing id")?
                    .clone_into(&mut self.id);
                self.created = response
                    .get("created_at")
                    .and_then(Value::as_u64)
                    .ok_or("Grok response missing created_at")?;
                out.push(self.chunk(json!({"role":"assistant"}), Value::Null));
            }
            "response.output_text.delta"
            | "response.text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.refusal.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or("Grok delta missing text")?;
                let key = if kind.contains("reasoning") {
                    "reasoning_content"
                } else if kind.contains("refusal") {
                    "refusal"
                } else {
                    "content"
                };
                out.push(self.chunk(json!({(key):delta}), Value::Null));
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or("Grok function call missing item id")?;
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .ok_or("Grok function call missing call_id")?;
                    let function = item
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or("Grok function call missing name")?;
                    let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                    let index = self.calls.len();
                    self.calls.push(Call {
                        item_id: item_id.to_owned(),
                        public_id: call_id.to_owned(),
                        arguments: arguments.to_owned(),
                    });
                    out.push(self.chunk(json!({"tool_calls":[{"index":index,"id":call_id,"type":"function","function":{"name":function,"arguments":arguments}}]}), Value::Null));
                }
            }
            "response.function_call_arguments.delta" => {
                let index = self
                    .call_index(event)
                    .ok_or("Grok emitted an unmatched tool delta")?;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or("Grok tool delta missing arguments")?;
                self.calls
                    .get_mut(index)
                    .ok_or("Grok tool call disappeared")?
                    .arguments
                    .push_str(delta);
                out.push(self.chunk(
                    json!({"tool_calls":[{"index":index,"function":{"arguments":delta}}]}),
                    Value::Null,
                ));
            }
            "response.function_call_arguments.done" | "response.output_item.done" => {
                let item = if kind == "response.output_item.done" {
                    event.get("item").unwrap_or(&Value::Null)
                } else {
                    event
                };
                if kind == "response.function_call_arguments.done"
                    || item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let index = self
                        .call_index(event)
                        .ok_or("Grok emitted an unmatched tool completion")?;
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .ok_or("Grok tool completion missing arguments")?;
                    let call = self
                        .calls
                        .get_mut(index)
                        .ok_or("Grok tool call disappeared")?;
                    let suffix = arguments
                        .strip_prefix(&call.arguments)
                        .ok_or("Grok tool arguments changed after streaming")?
                        .to_owned();
                    arguments.clone_into(&mut call.arguments);
                    if !suffix.is_empty() {
                        out.push(self.chunk(
                            json!({"tool_calls":[{"index":index,"function":{"arguments":suffix}}]}),
                            Value::Null,
                        ));
                    }
                } else if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                    out.push(self.chunk(json!({"provider_parts":[item]}), Value::Null));
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" | "response.done" => {
                let response = event
                    .get("response")
                    .ok_or("Grok terminal event missing response")?;
                let reason = finish_reason(response, !self.calls.is_empty())?;
                out.push(self.chunk(json!({}), json!(reason)));
                if self.include_usage
                    && let Some(u) = usage(response)
                {
                    out.push(json!({"id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,"choices":[],"usage":u}));
                }
                self.finished = true;
            }
            "error" => return Err(format!("Grok stream error: {event}")),
            _ => {}
        }
        if !out.is_empty() && self.id.is_empty() {
            return Err("Grok stream emitted data before response.created".into());
        }
        Ok(out)
    }
}

/// Stream Responses SSE as chat-completions SSE, preserving call identifiers.
pub(super) fn stream_to_chat(
    upstream: reqwest::Response,
    model: String,
    include_usage: bool,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);
    tokio::spawn(async move {
        let mut state = ChatStream {
            id: String::new(),
            created: 0,
            model,
            calls: Vec::new(),
            finished: false,
            include_usage,
        };
        let mut decoder = Decoder::default();
        let mut stream = upstream.bytes_stream();
        let mut error = None;
        'reading: while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(e) => {
                    error = Some(format!("Grok stream read error: {e}"));
                    break;
                }
            };
            let (frames, decode_error) = match decoder.push(&bytes) {
                Ok(frames) => (frames, None),
                Err((frames, error)) => (frames, Some(error)),
            };
            for (name, data) in frames {
                if data == "[DONE]" {
                    continue;
                }
                let event: Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(e) => {
                        error = Some(format!("Invalid Grok stream event: {e}"));
                        break 'reading;
                    }
                };
                match state.handle(&event, name.as_deref()) {
                    Ok(chunks) => {
                        for chunk in chunks {
                            if tx
                                .send(Ok(Event::default().data(chunk.to_string())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        error = Some(e);
                        break 'reading;
                    }
                }
                if state.finished {
                    break 'reading;
                }
            }
            if let Some(message) = decode_error {
                error = Some(message.to_owned());
                break;
            }
        }
        if !state.finished && error.is_none() {
            error = Some("Grok stream ended without a terminal response".into());
        }
        if let Some(message) = error {
            let _ = tx
                .send(Ok(Event::default()
                    .event("error")
                    .data(json!({"error":{"message":message}}).to_string())))
                .await;
        } else {
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        }
    });
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Deterministic request, response, and stream translation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Chat history and tool replay retain their wire semantics.
    #[test]
    fn chat_history_tools_and_options_translate() {
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model":"grok-4", "stream":true, "temperature":0.5,
            "max_completion_tokens":50,
            "messages":[
                {"role":"system","content":"Be precise"},
                {"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]},
                {"role":"assistant","content":null,"provider_parts":[{"type":"reasoning","id":"r1","encrypted_content":"opaque","summary":[]}],"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}}]},
                {"role":"tool","tool_call_id":"call_1","content":"result"}
            ],
            "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}],
            "tool_choice":{"type":"function","function":{"name":"lookup"}}
        })).unwrap();
        let payload = chat_to_responses(&req);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["max_output_tokens"], 50);
        assert_eq!(payload["input"][0]["role"], "system");
        assert_eq!(
            payload["input"][1]["content"][1]["image_url"],
            "https://example.com/a.png"
        );
        assert_eq!(payload["input"][2]["encrypted_content"], "opaque");
        assert_eq!(payload["input"][3]["call_id"], "call_1");
        assert_eq!(payload["input"][4]["output"], "result");
        assert_eq!(payload["tools"][0]["name"], "lookup");
        assert_eq!(
            payload["tool_choice"],
            json!({"type":"function","name":"lookup"})
        );
        assert!(payload.get("store").is_none());
    }

    /// Completed responses retain content, calls, reasoning and usage.
    #[test]
    fn completed_response_preserves_calls_reasoning_and_usage() {
        let response = json!({"id":"resp_1","created_at":42,"status":"completed","output":[
            {"type":"reasoning","id":"reason_1","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"thinking"}]},
            {"type":"message","content":[{"type":"output_text","text":"answer"}]},
            {"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{}"}
        ],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}});
        let chat = responses_to_chat(&response, "grok-4").unwrap();
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chat["choices"][0]["message"]["content"], "answer");
        assert_eq!(
            chat["choices"][0]["message"]["reasoning_content"],
            "thinking"
        );
        assert_eq!(
            chat["choices"][0]["message"]["provider_parts"][0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_1"
        );
        assert_eq!(chat["usage"]["total_tokens"], 5);
    }

    #[test]
    fn split_multiline_sse_event_is_decoded() {
        let mut decoder = Decoder::default();
        assert_eq!(
            decoder
                .push(b"event: response.created\ndata: {\"type\":\"response.created\",\nda")
                .unwrap(),
            Vec::new()
        );
        let frames = decoder
            .push(b"ta: \"response\":{\"id\":\"r\",\"created_at\":42}}\n\n")
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0.as_deref(), Some("response.created"));
        let event: Value = serde_json::from_str(&frames[0].1).unwrap();
        assert_eq!(event["response"]["id"], "r");
    }

    #[test]
    fn complete_events_precede_size_errors_regardless_of_chunking() {
        let frame = b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\n";
        for suffix in [
            "x".repeat(MAX_SSE_BYTES + 1),
            format!(
                "data: {}\ndata: {}\n",
                "x".repeat(MAX_SSE_BYTES / 2),
                "y".repeat(MAX_SSE_BYTES / 2)
            ),
        ] {
            for split in [false, true] {
                let mut decoder = Decoder::default();
                let mut input = Vec::new();
                if split {
                    assert_eq!(decoder.push(frame).unwrap().len(), 1);
                } else {
                    input.extend_from_slice(frame);
                }
                input.extend_from_slice(suffix.as_bytes());
                let (frames, error) = decoder.push(&input).unwrap_err();
                assert!(error.contains("size limit"));
                assert_eq!(frames.len(), usize::from(!split));
                if !split {
                    let event: Value = serde_json::from_str(&frames[0].1).unwrap();
                    assert_eq!(event["response"]["id"], "r");
                }
            }
        }
    }

    /// Split SSE frames and completed argument suffixes are decoded correctly.
    #[test]
    fn stream_decodes_split_frames_and_tool_argument_suffixes() {
        let mut decoder = Decoder::default();
        assert_eq!(
            decoder
                .push(b"event: response.created\r\ndata: {\"type\":\"response.")
                .unwrap()
                .len(),
            0
        );
        let frames = decoder
            .push(b"created\",\"response\":{\"id\":\"r\",\"created_at\":42}}\r\n\r\n")
            .unwrap();
        let mut state = ChatStream {
            id: String::new(),
            created: 0,
            model: "grok".into(),
            calls: vec![],
            finished: false,
            include_usage: true,
        };
        let created: Value = serde_json::from_str(&frames[0].1).unwrap();
        assert_eq!(
            state.handle(&created, frames[0].0.as_deref()).unwrap()[0]["choices"][0]["delta"]["role"],
            "assistant"
        );
        let added = json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc","call_id":"call","name":"lookup","arguments":""}});
        assert_eq!(
            state.handle(&added, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "call"
        );
        let delta = json!({"type":"response.function_call_arguments.delta","item_id":"fc","delta":"{\"x\":"});
        assert_eq!(
            state.handle(&delta, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["function"]
                ["arguments"],
            "{\"x\":"
        );
        let done = json!({"type":"response.function_call_arguments.done","item_id":"fc","arguments":"{\"x\":1}"});
        assert_eq!(
            state.handle(&done, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["function"]
                ["arguments"],
            "1}"
        );
        assert_eq!(state.handle(&done, None).unwrap().len(), 0);
        let terminal = json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}});
        let chunks = state.handle(&terminal, None).unwrap();
        assert_eq!(chunks[0]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chunks[1]["usage"]["total_tokens"], 3);
        assert!(state.finished);
    }

    /// Effort options follow each xAI model's supported settings.
    #[test]
    fn xai_reasoning_options_follow_model_capabilities() {
        for (model, effort, expected) in [
            ("grok-build", "high", None),
            ("grok-build-0.1", "high", None),
            ("grok-4.20-0309-non-reasoning", "high", None),
            ("grok-4.3", "xhigh", Some("high")),
            ("grok-4.6", "minimal", Some("low")),
            ("grok-4.6", "xhigh", Some("xhigh")),
            ("grok-4.20-multi-agent-0309", "xhigh", Some("xhigh")),
        ] {
            let req: ChatCompletionRequest = serde_json::from_value(json!({"model":model,"messages":[],"reasoning_effort":effort,"reasoning":{"summary":"auto"}})).unwrap();
            let payload = chat_to_responses(&req);
            assert_eq!(
                payload.pointer("/reasoning/effort").and_then(Value::as_str),
                expected
            );
            assert!(payload.pointer("/reasoning/summary").is_none());
            assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
        }
    }

    /// The HTTP stream exposes tool chunks and truncated-stream errors.
    #[tokio::test]
    async fn upstream_stream_is_exposed_as_chat_sse_and_errors_are_exposed() {
        use axum::{
            Router,
            body::{Body, Bytes},
            routing::get,
        };
        let app = Router::new()
            .route("/complete", get(|| async {
                let chunks: Vec<Result<Bytes, Infallible>> = vec![
                    Ok(Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\ndata: {\"type\":\"response.output_item.add")),
                    Ok(Bytes::from_static(b"ed\",\"item\":{\"type\":\"function_call\",\"id\":\"fc\",\"call_id\":\"call\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc\",\"delta\":\"{}\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")),
                ];
                Body::from_stream(futures_util::stream::iter(chunks))
            }))
            .route("/truncated", get(|| async {
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\n"
            }))
            .route("/oversized-line", get(|| async {
                format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\n{}", "x".repeat(MAX_SSE_BYTES + 1))
            }))
            .route("/oversized-event", get(|| async {
                format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\ndata: {}\ndata: {}\n\n", "x".repeat(MAX_SSE_BYTES / 2), "y".repeat(MAX_SSE_BYTES / 2))
            }))
            .route("/terminal-before-oversized", get(|| async {
                format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\"}}}}\n\n{}", "x".repeat(MAX_SSE_BYTES + 1))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{addr}/complete"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(
            stream_to_chat(upstream, "grok".into(), false).into_body(),
            1024 * 1024,
        )
        .await
        .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let events: Vec<Value> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect();
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "call"
        );
        assert_eq!(
            events[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        assert_eq!(events[3]["choices"][0]["finish_reason"], "tool_calls");
        assert!(text.contains("data: [DONE]"));
        let upstream = reqwest::get(format!("http://{addr}/truncated"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(
            stream_to_chat(upstream, "grok".into(), false).into_body(),
            1024 * 1024,
        )
        .await
        .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("event: error"));
        assert!(text.contains("without a terminal response"));
        assert!(!text.contains("[DONE]"));
        for (path, expected) in [
            ("oversized-line", "line exceeds size limit"),
            ("oversized-event", "event data exceeds size limit"),
        ] {
            let upstream = reqwest::get(format!("http://{addr}/{path}")).await.unwrap();
            let body = axum::body::to_bytes(
                stream_to_chat(upstream, "grok".into(), false).into_body(),
                1024 * 1024,
            )
            .await
            .unwrap();
            let text = std::str::from_utf8(&body).unwrap();
            assert!(text.contains("event: error") && text.contains(expected));
            assert!(
                text.find("\"role\":\"assistant\"").unwrap() < text.find("event: error").unwrap()
            );
            assert!(!text.contains("[DONE]"));
        }
        let upstream = reqwest::get(format!("http://{addr}/terminal-before-oversized"))
            .await
            .unwrap();
        let body = axum::body::to_bytes(
            stream_to_chat(upstream, "grok".into(), false).into_body(),
            1024 * 1024,
        )
        .await
        .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("assistant") && text.contains("\"finish_reason\":\"stop\""));
        assert!(text.contains("[DONE]"));
        assert!(!text.contains("event: error"));
        server.abort();
    }

    /// Invalid tool events and failed responses produce errors.
    #[test]
    fn fails_on_invalid_or_failed_upstream_events() {
        let mut state = ChatStream {
            id: "r".into(),
            created: 1,
            model: "grok".into(),
            calls: vec![],
            finished: false,
            include_usage: false,
        };
        assert!(state.handle(&json!({"type":"response.function_call_arguments.delta","item_id":"unknown","delta":"x"}), None).is_err());
        assert!(
            responses_to_chat(
                &json!({"id":"r","created_at":1,"status":"failed","output":[]}),
                "grok"
            )
            .is_err()
        );
    }
}
