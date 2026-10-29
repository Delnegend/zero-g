use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::openai::{
    AssistantMessage, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice,
    ChunkChoice, ChunkDelta, ChunkFunctionCall, ChunkToolCall, FunctionCall, ToolCall, Usage,
};
use crate::provider::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexCredentials};
use crate::provider::codex::payload::{
    build_codex_payload_from_chat, build_codex_payload_from_responses,
};
use crate::provider::common::errors::UpstreamError;
use crate::responses::{
    ResponseContentPart, ResponseCreateRequest, ResponseObject, ResponseOutputItem, ResponseUsage,
};

#[derive(Clone)]
pub(crate) struct CodexClient {
    http: reqwest::Client,
}

impl CodexClient {
    pub(crate) fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .tcp_nodelay(true)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    async fn send_request(
        &self,
        creds: &CodexCredentials,
        payload: &Value,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{CODEX_BASE_URL}/codex/responses");
        let model_slug = payload
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("gpt-5.5");

        let mut builder = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "omp")
            .header("version", CODEX_CLIENT_VERSION)
            .header("x-codex-routing-hint", format!("model={model_slug}"))
            .header("accept", "text/event-stream")
            .header("content-type", "application/json");

        if let Some(account_id) = &creds.account_id {
            builder = builder.header("chatgpt-account-id", account_id);
        }
        if let Some(residency) = &creds.residency {
            builder = builder.header("x-openai-internal-codex-residency", residency);
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        builder = builder
            .header("session_id", &session_id)
            .header("conversation_id", &session_id)
            .header("x-client-request-id", &session_id);

        let resp = builder
            .json(payload)
            .send()
            .await
            .map_err(|e| format!("Failed to send Codex request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(UpstreamError {
                status: Some(status),
                message: format!("Codex upstream returned {status}: {body}"),
            });
        }

        Ok(resp)
    }

    pub(crate) async fn stream_chat(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
    ) -> Result<Response, UpstreamError> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, &payload).await?;

        let model = req.model.clone();
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_chat_chunks(tx, upstream, model).await;
        });

        Ok(Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response())
    }

    pub(crate) async fn complete_chat_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, UpstreamError> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, &payload).await?;

        let model = req.model.clone();
        let mut text_acc = String::new();
        let mut reasoning_acc = String::new();
        let mut tool_calls_map: Vec<PendingToolCall> = Vec::new();
        let mut finish_reason = "stop".to_string();
        let mut usage_acc = Usage::default();

        let mut byte_stream = upstream.bytes_stream();
        let mut decoder = SseLineDecoder::default();

        while let Some(item) = byte_stream.next().await {
            let chunk = item.map_err(|e| format!("Stream read error: {e}"))?;
            let events = decoder.push_chunk(&chunk);
            for ev in events {
                match ev {
                    CodexParsedEvent::TextDelta(t) => text_acc.push_str(&t),
                    CodexParsedEvent::ReasoningDelta(r) => reasoning_acc.push_str(&r),
                    ev @ (CodexParsedEvent::ToolCallStart { .. }
                    | CodexParsedEvent::ToolCallDelta { .. }
                    | CodexParsedEvent::ToolCallDone { .. }) => {
                        apply_tool_event(&mut tool_calls_map, ev);
                    }
                    CodexParsedEvent::Finish { reason } => finish_reason = reason,
                    CodexParsedEvent::Usage(u) => usage_acc = u,
                }
            }
        }

        let tool_calls = if tool_calls_map.is_empty() {
            None
        } else {
            finish_reason = "tool_calls".to_string();
            Some(
                tool_calls_map
                    .into_iter()
                    .map(|call| ToolCall {
                        id: call.call_id,
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: call.name,
                            arguments: call.arguments,
                        },
                    })
                    .collect(),
            )
        };

        let message = AssistantMessage {
            role: "assistant".to_string(),
            content: if text_acc.is_empty() && tool_calls.is_some() {
                None
            } else {
                Some(text_acc)
            },
            reasoning_content: if reasoning_acc.is_empty() {
                None
            } else {
                Some(reasoning_acc)
            },
            tool_calls,
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".to_string(),
            created: now,
            model,
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason: Some(finish_reason),
            }],
            usage: Some(usage_acc),
        })
    }

    pub(crate) async fn stream_responses(
        &self,
        creds: &CodexCredentials,
        req: &ResponseCreateRequest,
    ) -> Result<Response, UpstreamError> {
        let payload = build_codex_payload_from_responses(req);
        let upstream = self.send_request(creds, &payload).await?;

        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_responses_events(tx, upstream).await;
        });

        Ok(Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response())
    }

    pub(crate) async fn complete_responses_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ResponseCreateRequest,
    ) -> Result<ResponseObject, UpstreamError> {
        let payload = build_codex_payload_from_responses(req);
        let upstream = self.send_request(creds, &payload).await?;

        let model = req.model.clone();
        let mut output_text = String::new();
        let mut output_items = Vec::new();
        let mut tool_calls_map: Vec<PendingToolCall> = Vec::new();
        let mut usage_opt = None;

        let mut byte_stream = upstream.bytes_stream();
        let mut decoder = SseLineDecoder::default();

        while let Some(item) = byte_stream.next().await {
            let chunk = item.map_err(|e| format!("Stream read error: {e}"))?;
            let events = decoder.push_chunk(&chunk);
            for ev in events {
                match ev {
                    CodexParsedEvent::TextDelta(t) => output_text.push_str(&t),
                    ev @ (CodexParsedEvent::ToolCallStart { .. }
                    | CodexParsedEvent::ToolCallDelta { .. }
                    | CodexParsedEvent::ToolCallDone { .. }) => {
                        apply_tool_event(&mut tool_calls_map, ev);
                    }
                    CodexParsedEvent::Usage(u) => usage_opt = Some(ResponseUsage::from(u)),
                    CodexParsedEvent::ReasoningDelta(_) | CodexParsedEvent::Finish { .. } => {}
                }
            }
        }

        if !output_text.is_empty() {
            output_items.push(ResponseOutputItem::Message {
                id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                status: "completed".to_string(),
                role: "assistant".to_string(),
                content: vec![ResponseContentPart::OutputText {
                    text: output_text.clone(),
                }],
            });
        }

        for call in tool_calls_map {
            let arguments = serde_json::from_str::<Value>(&call.arguments)
                .unwrap_or(Value::String(call.arguments));
            output_items.push(ResponseOutputItem::FunctionCall {
                id: format!("fc_{}", uuid::Uuid::new_v4().simple()),
                status: "completed".to_string(),
                name: call.name,
                call_id: call.call_id,
                arguments: arguments.to_string(),
            });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        Ok(ResponseObject {
            id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            object: "response".to_string(),
            created_at: now,
            model,
            status: "completed".to_string(),
            output: output_items,
            output_text,
            usage: usage_opt,
        })
    }
}

async fn pump_responses_events(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    upstream: reqwest::Response,
) {
    let mut byte_stream = upstream.bytes_stream();
    let mut decoder = SseRawLineDecoder::default();

    while let Some(item) = byte_stream.next().await {
        match item {
            Ok(bytes) => {
                let raw_events = decoder.push_chunk(&bytes);
                for (event_name, data) in raw_events {
                    let mut sse_ev = Event::default().data(data);
                    if let Some(ev) = event_name {
                        sse_ev = sse_ev.event(ev);
                    }
                    if tx.send(Ok(sse_ev)).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                error!("Error in Codex upstream stream: {e}");
                break;
            }
        }
    }
}

// Streaming events share ordering and tool-call state throughout this loop.
#[allow(clippy::too_many_lines)]
async fn pump_chat_chunks(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    upstream: reqwest::Response,
    model: String,
) {
    let chunk_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut byte_stream = upstream.bytes_stream();
    let mut decoder = SseLineDecoder::default();
    let mut tool_calls: Vec<PendingToolCall> = Vec::new();
    let mut has_emitted_finish = false;

    while let Some(item) = byte_stream.next().await {
        match item {
            Ok(bytes) => {
                let events = decoder.push_chunk(&bytes);
                for ev in events {
                    match ev {
                        CodexParsedEvent::TextDelta(delta) => {
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        content: Some(delta),
                                        ..Default::default()
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::ReasoningDelta(delta) => {
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        reasoning_content: Some(delta),
                                        ..Default::default()
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::ToolCallStart {
                            call_id,
                            item_id,
                            name,
                        } => {
                            let index = tool_calls.len();
                            tool_calls.push(PendingToolCall {
                                call_id: call_id.clone(),
                                item_id,
                                name: name.clone(),
                                arguments: String::new(),
                                streamed_len: 0,
                            });
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        tool_calls: Some(vec![ChunkToolCall {
                                            index: chunk_tool_index(index),
                                            id: Some(call_id),
                                            r#type: Some("function".to_string()),
                                            function: Some(ChunkFunctionCall {
                                                name: Some(name),
                                                arguments: Some(String::new()),
                                            }),
                                        }]),
                                        ..Default::default()
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::ToolCallDelta { key, delta } => {
                            let index = tool_calls
                                .iter()
                                .position(|call| call.matches(key.as_deref()))
                                .unwrap_or_else(|| tool_calls.len().saturating_sub(1));
                            if let Some(call) = tool_calls.get_mut(index) {
                                call.arguments.push_str(&delta);
                                call.streamed_len = call.streamed_len.saturating_add(delta.len());
                            }
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        tool_calls: Some(vec![ChunkToolCall {
                                            index: chunk_tool_index(index),
                                            id: None,
                                            r#type: None,
                                            function: Some(ChunkFunctionCall {
                                                name: None,
                                                arguments: Some(delta),
                                            }),
                                        }]),
                                        ..Default::default()
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::ToolCallDone { key, arguments } => {
                            let index = tool_calls
                                .iter()
                                .position(|call| call.matches(key.as_deref()))
                                .or_else(|| tool_calls.len().checked_sub(1));
                            let Some(index) = index else { continue };
                            let Some(call) = tool_calls.get_mut(index) else {
                                continue;
                            };
                            let emit = call.streamed_len == 0 && !arguments.is_empty();
                            call.arguments.clone_from(&arguments);
                            if !emit {
                                continue;
                            }
                            call.streamed_len = arguments.len();
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        tool_calls: Some(vec![ChunkToolCall {
                                            index: chunk_tool_index(index),
                                            id: None,
                                            r#type: None,
                                            function: Some(ChunkFunctionCall {
                                                name: None,
                                                arguments: Some(arguments),
                                            }),
                                        }]),
                                        ..Default::default()
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::Finish { reason } => {
                            has_emitted_finish = true;
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta::default(),
                                    finish_reason: Some(reason),
                                }],
                                usage: None,
                            };
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::Usage(_) => {}
                    }
                }
            }
            Err(e) => {
                error!("Error reading Codex byte stream: {e}");
                break;
            }
        }
    }

    if !has_emitted_finish {
        let chunk = ChatCompletionChunk {
            id: chunk_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: now,
            model: model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta::default(),
                finish_reason: Some(
                    if tool_calls.is_empty() {
                        "stop"
                    } else {
                        "tool_calls"
                    }
                    .to_string(),
                ),
            }],
            usage: None,
        };
        if let Ok(data) = serde_json::to_string(&chunk) {
            let _ = tx.send(Ok(Event::default().data(data))).await;
        }
    }

    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
}

#[derive(Debug)]
enum CodexParsedEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart {
        call_id: String,
        item_id: Option<String>,
        name: String,
    },
    ToolCallDelta {
        key: Option<String>,
        delta: String,
    },
    ToolCallDone {
        key: Option<String>,
        arguments: String,
    },
    Finish {
        reason: String,
    },
    Usage(Usage),
}

/// A function call being assembled from the upstream event stream.
///
/// Upstream identifies a call by two ids: `call_id` (`call_…`) on the item
/// events, and `item_id` (`fc_…`) on the argument events. Argument deltas and
/// the final `*.done` events only carry `item_id`, so both are matched.
struct PendingToolCall {
    call_id: String,
    item_id: Option<String>,
    name: String,
    arguments: String,
    /// Bytes of argument deltas already forwarded to the client. The Codex
    /// backend only streams deltas for single-call turns; for parallel calls it
    /// jumps straight to `*.done` with the complete arguments, which then have
    /// to be emitted in one chunk.
    streamed_len: usize,
}

impl PendingToolCall {
    fn matches(&self, key: Option<&str>) -> bool {
        key.is_some_and(|key| key == self.call_id || self.item_id.as_deref() == Some(key))
    }
}

fn chunk_tool_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

fn function_call_key(val: &Value) -> Option<String> {
    val.get("item_id")
        .or_else(|| val.get("call_id"))
        .or_else(|| val.get("id"))
        .or_else(|| val.pointer("/item/call_id"))
        .or_else(|| val.pointer("/item/id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

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
fn apply_tool_event(calls: &mut Vec<PendingToolCall>, ev: CodexParsedEvent) {
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
            } else if let Some(last) = calls.last_mut() {
                last.arguments.push_str(&delta);
            }
        }
        CodexParsedEvent::ToolCallDone { key, arguments } => {
            if let Some(call) = calls.iter_mut().find(|call| call.matches(key.as_deref())) {
                call.arguments = arguments;
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct SseLineDecoder {
    buffer: Vec<u8>,
    current_event_name: Option<String>,
}

impl SseLineDecoder {
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<CodexParsedEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end();

            if line.is_empty() {
                self.current_event_name = None;
                continue;
            }

            if let Some(ev) = line.strip_prefix("event:") {
                self.current_event_name = Some(ev.trim().to_string());
                continue;
            }

            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(json_val) = serde_json::from_str::<Value>(data) {
                    self.parse_json_event(&json_val, &mut out);
                }
            }
        }
        out
    }

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
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("call_unknown")
                            .to_string();
                        let item_id = item.get("id").and_then(Value::as_str).map(str::to_string);
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("function")
                            .to_string();
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
            Some("response.completed" | "response.done") => {
                if let Some(usage_obj) = val.pointer("/response/usage") {
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
                out.push(CodexParsedEvent::Finish {
                    reason: "stop".to_string(),
                });
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct SseRawLineDecoder {
    buffer: Vec<u8>,
    current_event: Option<String>,
}

impl SseRawLineDecoder {
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end();

            if line.is_empty() {
                self.current_event = None;
                continue;
            }

            if let Some(ev) = line.strip_prefix("event: ") {
                self.current_event = Some(ev.trim().to_string());
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                out.push((self.current_event.clone(), data.to_string()));
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Feeds raw upstream SSE through the parser and the accumulator, mirroring
    /// the non-streaming paths.
    fn collect(sse: &str) -> Vec<PendingToolCall> {
        let mut decoder = SseLineDecoder::default();
        let mut calls = Vec::new();
        for ev in decoder.push_chunk(sse.as_bytes()) {
            apply_tool_event(&mut calls, ev);
        }
        calls
    }

    fn added(id: &str, call_id: &str, name: &str) -> String {
        format!(
            "event: response.output_item.added\ndata: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"{id}\",\"call_id\":\"{call_id}\",\"name\":\"{name}\",\"arguments\":\"\"}}}}\n\n"
        )
    }

    fn args_done(id: &str, arguments: &str) -> String {
        let args = serde_json::to_string(arguments).unwrap();
        format!(
            "event: response.function_call_arguments.done\ndata: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"{id}\",\"arguments\":{args}}}\n\n"
        )
    }

    fn item_done(id: &str, call_id: &str, name: &str, arguments: &str) -> String {
        let args = serde_json::to_string(arguments).unwrap();
        format!(
            "event: response.output_item.done\ndata: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"function_call\",\"id\":\"{id}\",\"call_id\":\"{call_id}\",\"name\":\"{name}\",\"arguments\":{args}}}}}\n\n"
        )
    }

    fn args_delta(id: &str, delta: &str) -> String {
        let delta = serde_json::to_string(delta).unwrap();
        format!(
            "event: response.function_call_arguments.delta\ndata: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"{id}\",\"delta\":{delta}}}\n\n"
        )
    }

    /// Parallel calls: the backend sends no argument deltas at all, only the
    /// completed item. Before the fix every call kept `arguments: ""`.
    #[test]
    fn test_parallel_tool_calls_take_arguments_from_done_events() {
        let mut sse = String::new();
        sse += &added("fc_1", "call_1", "grep");
        sse += &args_done("fc_1", "{\"pattern\":\"foo\",\"path\":\"/tmp\"}");
        sse += &item_done(
            "fc_1",
            "call_1",
            "grep",
            "{\"pattern\":\"foo\",\"path\":\"/tmp\"}",
        );
        sse += &added("fc_2", "call_2", "read");
        sse += &args_done("fc_2", "{\"path\":\"/etc/hostname\"}");
        sse += &item_done("fc_2", "call_2", "read", "{\"path\":\"/etc/hostname\"}");
        sse += &added("fc_3", "call_3", "ping");
        sse += &args_done("fc_3", "{\"text\":\"hi\"}");
        sse += &item_done("fc_3", "call_3", "ping", "{\"text\":\"hi\"}");

        let calls = collect(&sse);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].name, "grep");
        assert_eq!(
            calls[0].arguments,
            "{\"pattern\":\"foo\",\"path\":\"/tmp\"}"
        );
        assert_eq!(calls[1].name, "read");
        assert_eq!(calls[1].arguments, "{\"path\":\"/etc/hostname\"}");
        assert_eq!(calls[2].name, "ping");
        assert_eq!(calls[2].arguments, "{\"text\":\"hi\"}");
    }

    /// Single call: deltas arrive first and the `done` event repeats the full
    /// arguments — replacing must not duplicate them.
    #[test]
    fn test_single_tool_call_deltas_then_done_are_not_duplicated() {
        let mut sse = String::new();
        sse += &added("fc_1", "call_1", "ping");
        for delta in ["{\"", "text", "\":\"", "hi", "\"}"] {
            sse += &args_delta("fc_1", delta);
        }
        sse += &args_done("fc_1", "{\"text\":\"hi\"}");
        sse += &item_done("fc_1", "call_1", "ping", "{\"text\":\"hi\"}");

        let calls = collect(&sse);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, "{\"text\":\"hi\"}");
    }

    #[test]
    fn test_tool_delta_can_match_call_id() {
        let mut sse = String::new();
        sse += &added("fc_1", "call_1", "ping");
        sse += "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"call_id\":\"call_1\",\"delta\":\"{\\\"text\\\":\\\"hi\\\"}\"}\n\n";

        let calls = collect(&sse);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, "{\"text\":\"hi\"}");
    }

    #[test]
    fn test_tool_call_arguments_as_json_object() {
        let mut sse = String::new();
        sse += "event:response.output_item.added\ndata:{\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n";
        sse += "event:response.output_item.done\ndata:{\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":{\"path\":\"Cargo.toml\"}}}\n\n";

        let calls = collect(&sse);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, "{\"path\":\"Cargo.toml\"}");
    }
}
