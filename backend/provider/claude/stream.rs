use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Accumulates indexed Anthropic content blocks and emits `OpenAI` deltas.
pub(super) struct Decoder {
    /// Gateway completion identifier.
    pub(super) id: String,
    /// Requested model identifier.
    pub(super) model: String,
    /// Creation timestamp.
    pub(super) created: u64,
    /// Indexed upstream content blocks.
    pub(super) blocks: BTreeMap<u64, Value>,
    /// Number of input tokens.
    pub(super) input_tokens: u64,
    /// Number of output tokens.
    pub(super) output_tokens: u64,
    /// Mapped completion reason.
    pub(super) stop_reason: String,
    /// Whether the upstream sent `message_stop`.
    pub(super) finished: bool,
}
impl Decoder {
    /// Initialize a decoder for a model.
    pub(super) fn new(model: String) -> Self {
        Self {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            blocks: BTreeMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: "stop".into(),
            finished: false,
        }
    }
    /// Wrap a delta in a chat completion chunk.
    fn chunk(&self, delta: Value, reason: Value) -> Value {
        let mut choice = serde_json::Map::new();
        choice.insert("index".into(), json!(0));
        choice.insert("delta".into(), delta);
        choice.insert("finish_reason".into(), reason);
        json!({"id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,"choices":[choice]})
    }
    /// Process one Anthropic stream event.
    #[allow(clippy::too_many_lines)]
    pub(super) fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let mut output = Vec::new();
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                let usage = event.pointer("/message/usage");
                self.input_tokens = [
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                ]
                .into_iter()
                .filter_map(|key| usage.and_then(|v| v.get(key)).and_then(Value::as_u64))
                .fold(0_u64, u64::saturating_add);
                output.push(self.chunk(json!({"role":"assistant"}), Value::Null));
            }
            "content_block_start" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Missing block index")?;
                let block = event
                    .get("content_block")
                    .cloned()
                    .ok_or("Missing content block")?;
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .strip_prefix('_')
                        .unwrap_or_else(|| {
                            block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        });
                    let tool_index = self
                        .blocks
                        .values()
                        .filter(|v| v.get("type").and_then(Value::as_str) == Some("tool_use"))
                        .count();
                    output.push(self.chunk(json!({"tool_calls":[{"index":tool_index,"id":block.get("id"),"type":"function","function":{"name":name,"arguments":""}}]}), Value::Null));
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Missing block index")?;
                let delta = event.get("delta").ok_or("Missing block delta")?;
                let tool_index = self
                    .blocks
                    .iter()
                    .filter(|(i, b)| {
                        **i < index && b.get("type").and_then(Value::as_str) == Some("tool_use")
                    })
                    .count();
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or("Delta for unknown Claude block")?;
                let (field, fragment) = match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => (
                        "text",
                        delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("thinking_delta") => (
                        "thinking",
                        delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("signature_delta") => (
                        "signature",
                        delta
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("input_json_delta") => (
                        "partial_json",
                        delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    _ => return Ok(output),
                };
                let mut accumulated = block
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                accumulated.push_str(fragment);
                block[field] = json!(accumulated);
                let wire = match field {
                    "text" => Some(json!({"content":fragment})),
                    "thinking" => Some(json!({"reasoning_content":fragment})),
                    "partial_json" => Some(
                        json!({"tool_calls":[{"index":tool_index,"function":{"arguments":fragment}}]}),
                    ),
                    _ => None,
                };
                if let Some(wire) = wire {
                    output.push(self.chunk(wire, Value::Null));
                }
            }
            "content_block_stop" => {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Missing block index")?;
                let block = self.blocks.get(&index).ok_or("Unknown Claude block")?;
                if block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && block
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                {
                    let tool_index = self
                        .blocks
                        .iter()
                        .filter(|(i, b)| {
                            **i < index && b.get("type").and_then(Value::as_str) == Some("tool_use")
                        })
                        .count();
                    let arguments = block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string();
                    output.push(self.chunk(json!({"tool_calls":[{"index":tool_index,"function":{"arguments":arguments}}]}), Value::Null));
                }
                let part = replay_block(block)?;
                output.push(self.chunk(json!({"provider_parts":[&part]}), Value::Null));
                self.blocks.insert(index, part);
            }
            "message_delta" => {
                if let Some(n) = event
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    self.output_tokens = n;
                }
                self.stop_reason =
                    match event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                        Some("tool_use") => "tool_calls",
                        Some("max_tokens") => "length",
                        _ => "stop",
                    }
                    .into();
            }
            "message_stop" => {
                self.finished = true;
                output.push(self.chunk(json!({}), json!(self.stop_reason)));
            }
            "error" => return Err("Claude upstream reported a stream error".into()),
            _ => {}
        }
        Ok(output)
    }
    /// Assemble the completed assistant message.
    pub(super) fn message(&self) -> Value {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut parts = Vec::new();
        for block in self.blocks.values() {
            parts.push(block.clone());
            match block.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                Some("thinking") => {
                    reasoning.push_str(
                        block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                }
                Some("tool_use") => {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = block
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .map_or_else(
                            || {
                                block
                                    .get("input")
                                    .cloned()
                                    .unwrap_or_else(|| json!({}))
                                    .to_string()
                            },
                            str::to_owned,
                        );
                    calls.push(json!({"id":block.get("id"),"type":"function","function":{"name":name.strip_prefix('_').unwrap_or(name),"arguments":arguments}}));
                }
                _ => {}
            }
        }
        let mut message = serde_json::Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert(
            "content".into(),
            if text.is_empty() {
                Value::Null
            } else {
                json!(text)
            },
        );
        if !reasoning.is_empty() {
            message.insert("reasoning_content".into(), json!(reasoning));
        }
        if !calls.is_empty() {
            message.insert("tool_calls".into(), json!(calls));
        }
        if !parts.is_empty() {
            message.insert("provider_parts".into(), json!(parts));
        }
        Value::Object(message)
    }
    /// Build `OpenAI` token accounting.
    pub(super) fn usage(&self) -> Value {
        json!({"prompt_tokens":self.input_tokens,"completion_tokens":self.output_tokens,"total_tokens":self.input_tokens.saturating_add(self.output_tokens)})
    }
}

/// Restore a completed wire block for exact conversation replay.
fn replay_block(block: &Value) -> Result<Value, String> {
    let mut part = block.clone();
    if let Some(object) = part.as_object_mut()
        && let Some(arguments) = object
            .remove("partial_json")
            .and_then(|v| v.as_str().map(str::to_owned))
    {
        let input = if arguments.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&arguments)
                .map_err(|e| format!("Invalid Claude tool arguments: {e}"))?
        };
        object.insert("input".into(), input);
    }
    Ok(part)
}

/// Stream decoding tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    /// Preserve thought signatures and incremental tool arguments.
    #[test]
    fn preserves_signed_thinking_and_tool_input() {
        let mut decoder = Decoder::new("claude".into());
        for event in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":7}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thought"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call","name":"_lookup","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ] {
            decoder.event(&event).unwrap();
        }
        let message = decoder.message();
        assert_eq!(
            message
                .pointer("/provider_parts/0/signature")
                .and_then(Value::as_str),
            Some("signed")
        );
        assert_eq!(
            message
                .pointer("/tool_calls/0/function/name")
                .and_then(Value::as_str),
            Some("lookup")
        );
        assert_eq!(
            message
                .pointer("/tool_calls/0/function/arguments")
                .and_then(Value::as_str),
            Some("{\"x\":1}")
        );
        assert_eq!(
            decoder.usage().get("total_tokens").and_then(Value::as_u64),
            Some(10)
        );
        assert_eq!(decoder.stop_reason, "tool_calls");
    }
}
