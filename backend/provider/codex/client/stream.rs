use serde_json::Value;

use crate::openai::Usage;
use crate::provider::common::stream::SseRawLineDecoder;

/// Events extracted from the Codex Responses stream.
#[derive(Debug)]
pub(super) enum CodexParsedEvent {
    /// Incremental assistant output text.
    TextDelta(String),
    /// Incremental assistant reasoning text.
    ReasoningDelta(String),
    /// A function call has been introduced.
    ToolCallStart {
        /// Identifier exposed to the caller for the function call.
        call_id: String,
        /// Upstream output item identifier, when supplied.
        item_id: Option<String>,
        /// Name of the function being called.
        name: String,
    },
    /// An incremental function-call argument fragment.
    ToolCallDelta {
        /// Identifier used to associate the fragment with a call.
        key: Option<String>,
        /// Argument text to append.
        delta: String,
    },
    /// The complete arguments for a function call.
    ToolCallDone {
        /// Identifier used to associate the arguments with a call.
        key: Option<String>,
        /// Complete argument text, replacing accumulated fragments.
        arguments: String,
    },
    /// The upstream response has finished.
    Finish {
        /// Chat completion finish reason.
        reason: String,
        /// Responses lifecycle status.
        status: String,
        /// Original terminal response object, if supplied.
        response: Option<Value>,
    },
    /// Token usage reported by the upstream response.
    Usage(Usage),
}

/// A function call being assembled from the upstream event stream.
///
/// Upstream identifies a call by two ids: `call_id` (`call_…`) on the item
/// events, and `item_id` (`fc_…`) on the argument events. Argument deltas and
/// the final `*.done` events only carry `item_id`, so both are matched.
pub(super) struct PendingToolCall {
    /// Identifier exposed to the caller.
    pub(super) call_id: String,
    /// Upstream output item identifier, if available.
    pub(super) item_id: Option<String>,
    /// Function name.
    pub(super) name: String,
    /// Accumulated or final argument text.
    pub(super) arguments: String,
    /// Bytes of argument deltas already forwarded to the client. The Codex
    /// backend only streams deltas for single-call turns; for parallel calls it
    /// jumps straight to `*.done` with the complete arguments, which then have
    /// to be emitted in one chunk.
    pub(super) streamed_len: usize,
}

impl PendingToolCall {
    /// Checks whether a supplied key matches either upstream identifier.
    pub(super) fn matches(&self, key: Option<&str>) -> bool {
        key.is_some_and(|key| key == self.call_id || self.item_id.as_deref() == Some(key))
    }
}

/// Converts a tool-call index to the chunk representation, saturating on overflow.
pub(super) fn chunk_tool_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Extracts a function-call identifier from an event or its nested item.
fn function_call_key(val: &Value) -> Option<String> {
    val.get("item_id")
        .or_else(|| val.get("call_id"))
        .or_else(|| val.get("id"))
        .or_else(|| val.pointer("/item/id"))
        .or_else(|| val.pointer("/item/call_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Extracts argument text, serializing supported non-string JSON values.
fn extract_arguments(val: &Value) -> Option<String> {
    match val.get("arguments") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) if v.is_object() || v.is_array() || v.is_number() || v.is_boolean() => {
            Some(v.to_string())
        }
        _ => None,
    }
}

/// Folds a tool-call event into the calls seen so far. Deltas append; the
/// upstream's `*.done` events carry the complete arguments and replace whatever
/// was assembled (idempotent when both arrive for the same call).
pub(super) fn apply_tool_event(calls: &mut Vec<PendingToolCall>, ev: CodexParsedEvent) -> bool {
    match ev {
        CodexParsedEvent::ToolCallStart {
            call_id,
            item_id,
            name,
        } => calls.push(PendingToolCall {
            call_id,
            item_id,
            name,
            arguments: String::new(),
            streamed_len: 0,
        }),
        CodexParsedEvent::ToolCallDelta { key, delta } => {
            if let Some(call) = calls.iter_mut().find(|call| call.matches(key.as_deref())) {
                call.arguments.push_str(&delta);
            } else {
                return false;
            }
        }
        CodexParsedEvent::ToolCallDone { key, arguments } => {
            if let Some(call) = calls.iter_mut().find(|call| call.matches(key.as_deref())) {
                call.arguments = arguments;
            } else {
                return false;
            }
        }
        _ => {}
    }
    true
}

/// Incrementally decodes upstream SSE lines into recognized Codex events.
pub(super) struct SseLineDecoder {
    /// Decoder for complete SSE frames.
    raw: SseRawLineDecoder,
    /// Current SSE event name, used when JSON has no type.
    current_event_name: Option<String>,
}
impl Default for SseLineDecoder {
    fn default() -> Self {
        Self {
            raw: codex_raw_decoder(),
            current_event_name: None,
        }
    }
}

impl SseLineDecoder {
    /// Buffers a byte chunk and parses any complete SSE data lines.
    pub(super) fn push_chunk(
        &mut self,
        chunk: &[u8],
    ) -> Result<Vec<CodexParsedEvent>, (Vec<CodexParsedEvent>, &'static str)> {
        let mut out = Vec::new();
        let (frames, error) = match self.raw.push_chunk(chunk) {
            Ok(frames) => (frames, None),
            Err((frames, error)) => (frames, Some(error)),
        };
        for (name, data) in frames {
            self.current_event_name = name;
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                self.parse_json_event(&value, &mut out);
            }
        }
        match error {
            Some(error) => Err((out, error)),
            None => Ok(out),
        }
    }

    /// Appends recognized events from one JSON payload.
    // The event variants form one protocol dispatch table.
    #[allow(clippy::too_many_lines)]
    fn parse_json_event(&self, val: &Value, out: &mut Vec<CodexParsedEvent>) {
        let event_type = val
            .get("type")
            .and_then(Value::as_str)
            .or(self.current_event_name.as_deref());

        match event_type {
            Some("response.text.delta" | "response.output_text.delta") => {
                if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                    out.push(CodexParsedEvent::TextDelta(delta.to_string()));
                }
            }
            Some(
                "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.thought.delta",
            ) => {
                if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                    out.push(CodexParsedEvent::ReasoningDelta(delta.to_string()));
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = val.get("item") {
                    let item_type = item.get("type").and_then(Value::as_str);
                    if item_type == Some("function_call") {
                        let item_id = item
                            .get("id")
                            .or_else(|| item.get("call_id"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .filter(|id| {
                                !id.is_empty()
                                    && id.len() <= 64
                                    && id.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
                                    })
                            })
                            .map_or_else(
                                || format!("call_{}", uuid::Uuid::new_v4().simple()),
                                str::to_owned,
                            );
                        let Some(name) = item
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty())
                        else {
                            return;
                        };
                        let name = name.to_string();
                        out.push(CodexParsedEvent::ToolCallStart {
                            call_id,
                            item_id: item_id.clone(),
                            name,
                        });
                        if let Some(arguments) = extract_arguments(item)
                            && !arguments.is_empty()
                        {
                            out.push(CodexParsedEvent::ToolCallDone {
                                key: item_id,
                                arguments,
                            });
                        }
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                    out.push(CodexParsedEvent::ToolCallDelta {
                        key: function_call_key(val),
                        delta: delta.to_string(),
                    });
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some(arguments) = extract_arguments(val) {
                    out.push(CodexParsedEvent::ToolCallDone {
                        key: function_call_key(val),
                        arguments,
                    });
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = val.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let key = function_call_key(val).or_else(|| function_call_key(item));
                    if let Some(arguments) = extract_arguments(item) {
                        out.push(CodexParsedEvent::ToolCallDone { key, arguments });
                    }
                }
            }
            Some(
                "response.completed"
                | "response.done"
                | "response.incomplete"
                | "response.failed"
                | "error",
            ) => {
                if let Some(usage_obj) = val.pointer("/response/usage").and_then(Value::as_object) {
                    let prompt_tokens = usage_obj
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .map_or(0, |value| u32::try_from(value).unwrap_or(u32::MAX));
                    let completion_tokens = usage_obj
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .map_or(0, |value| u32::try_from(value).unwrap_or(u32::MAX));
                    let total_tokens = usage_obj
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .map_or_else(
                            || prompt_tokens.saturating_add(completion_tokens),
                            |value| u32::try_from(value).unwrap_or(u32::MAX),
                        );

                    out.push(CodexParsedEvent::Usage(Usage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                    }));
                }
                let status = match event_type {
                    Some("response.incomplete") => "incomplete",
                    Some("response.failed" | "error") => "failed",
                    _ => val
                        .pointer("/response/status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed"),
                };
                out.push(CodexParsedEvent::Finish {
                    reason: if status == "incomplete" {
                        "length"
                    } else {
                        "stop"
                    }
                    .to_string(),
                    status: status.to_string(),
                    response: val.get("response").cloned(),
                });
            }
            _ => {}
        }
    }
}

/// Creates the raw decoder with Codex-specific error messages.
pub(super) const fn codex_raw_decoder() -> SseRawLineDecoder {
    SseRawLineDecoder::new(
        "Codex SSE line exceeds size limit",
        "Codex SSE event data exceeds size limit",
    )
}
