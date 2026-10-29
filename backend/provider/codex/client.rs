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
use crate::responses::ResponseCreateRequest;

/// HTTP client for translating requests to the Codex Responses endpoint.
#[derive(Clone)]
pub(crate) struct CodexClient {
    /// Shared connection pool used for upstream requests.
    http: reqwest::Client,
    /// Responses API base URL.
    base_url: String,
}

impl CodexClient {
    /// Builds a client with connection pooling and TCP no-delay enabled.
    pub(crate) fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .tcp_nodelay(true)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: CODEX_BASE_URL.to_string(),
        }
    }

    /// Sends an authenticated streaming request to the Codex Responses endpoint.
    async fn send_request(
        &self,
        creds: &CodexCredentials,
        mut payload: Value,
        session_id: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{}/codex/responses", self.base_url);
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

        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "prompt_cache_key".to_string(),
                Value::String(session_id.to_owned()),
            );
        }
        builder = builder
            .header("session_id", session_id)
            .header("conversation_id", session_id)
            .header("x-client-request-id", session_id);

        let resp = builder
            .json(&payload)
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

    /// Translates and streams a chat completion as SSE chunks.
    pub(crate) async fn stream_chat(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
        session_id: &str,
    ) -> Result<Response, UpstreamError> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, payload, session_id).await?;

        let model = req.model.clone();
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_chat_chunks(tx, upstream, model).await;
        });

        Ok(Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response())
    }

    /// Collects an upstream stream into a single chat completion.
    pub(crate) async fn complete_chat_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
        session_id: &str,
    ) -> Result<ChatCompletionResponse, UpstreamError> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, payload, session_id).await?;

        let model = req.model.clone();
        let mut text_acc = String::new();
        let mut reasoning_acc = String::new();
        let mut tool_calls_map: Vec<PendingToolCall> = Vec::new();
        let mut finish_reason = None;
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
                        if !apply_tool_event(&mut tool_calls_map, ev) {
                            return Err("Codex emitted an unmatched tool event".into());
                        }
                    }
                    CodexParsedEvent::Finish { reason, status, .. } => {
                        if !matches!(status.as_str(), "completed" | "incomplete") {
                            return Err("Codex response failed".into());
                        }
                        finish_reason = Some(reason);
                    }
                    CodexParsedEvent::Usage(u) => usage_acc = u,
                }
            }
        }
        let mut finish_reason =
            finish_reason.ok_or("Codex stream ended without a terminal response")?;

        let tool_calls = if tool_calls_map.is_empty() {
            None
        } else {
            if finish_reason == "stop" {
                finish_reason = "tool_calls".to_string();
            }
            Some(
                tool_calls_map
                    .into_iter()
                    .map(|call| ToolCall {
                        id: call.call_id,
                        r#type: "function".to_string(),
                        thought_signature: None,
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
            provider_parts: None,
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

    /// Forwards a Responses request as an SSE event stream.
    pub(crate) async fn stream_responses(
        &self,
        creds: &CodexCredentials,
        req: &ResponseCreateRequest,
        session_id: &str,
    ) -> Result<Response, UpstreamError> {
        let payload = build_codex_payload_from_responses(req);
        let upstream = self.send_request(creds, payload, session_id).await?;

        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_responses_events(tx, upstream).await;
        });

        Ok(Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response())
    }

    /// Collects an upstream stream into a single Responses object.
    pub(crate) async fn complete_responses_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ResponseCreateRequest,
        session_id: &str,
    ) -> Result<Value, UpstreamError> {
        let payload = build_codex_payload_from_responses(req);
        let upstream = self.send_request(creds, payload, session_id).await?;

        let mut output_text = String::new();

        let mut byte_stream = upstream.bytes_stream();
        let mut decoder = SseLineDecoder::default();

        while let Some(item) = byte_stream.next().await {
            let chunk = item.map_err(|e| format!("Stream read error: {e}"))?;
            let events = decoder.push_chunk(&chunk);
            for ev in events {
                match ev {
                    CodexParsedEvent::TextDelta(text) => output_text.push_str(&text),
                    CodexParsedEvent::Finish {
                        status, response, ..
                    } => {
                        let mut response =
                            response.ok_or("Codex terminal event omitted its response")?;
                        if !matches!(status.as_str(), "completed" | "incomplete" | "failed")
                            || !response.get("output").is_some_and(Value::is_array)
                        {
                            return Err("Codex terminal response is malformed".into());
                        }
                        if !repair_response_calls(
                            &mut response,
                            &mut std::collections::HashMap::new(),
                        ) {
                            return Err("Codex response contains a malformed function call".into());
                        }
                        if output_text.is_empty()
                            && let Some(items) = response.get("output").and_then(Value::as_array)
                        {
                            for item in items {
                                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                                    for part in parts {
                                        if part.get("type").and_then(Value::as_str)
                                            == Some("output_text")
                                            && let Some(text) =
                                                part.get("text").and_then(Value::as_str)
                                        {
                                            output_text.push_str(text);
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(object) = response.as_object_mut() {
                            object
                                .entry("output_text")
                                .or_insert_with(|| Value::String(output_text));
                        }
                        return Ok(response);
                    }
                    _ => {}
                }
            }
        }
        Err("Codex stream ended without a terminal response".into())
    }
}

/// Repairs function call IDs in an event or terminal response consistently.
fn repair_response_calls(
    value: &mut Value,
    ids: &mut std::collections::HashMap<String, String>,
) -> bool {
    if let Some(item) = value.get_mut("item")
        && !repair_response_calls(item, ids)
    {
        return false;
    }
    if let Some(response) = value.get_mut("response")
        && !repair_response_calls(response, ids)
    {
        return false;
    }
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        for item in output {
            if !repair_response_calls(item, ids) {
                return false;
            }
        }
    }
    if value.get("type").and_then(Value::as_str) != Some("function_call") {
        return true;
    }
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let raw = value
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return true;
    }
    let key = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(raw)
        .to_owned();
    if key.is_empty() {
        return false;
    }
    let repaired = ids
        .entry(key)
        .or_insert_with(|| format!("call_{}", uuid::Uuid::new_v4().simple()))
        .clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("call_id".to_string(), Value::String(repaired));
    }
    true
}

/// Forwards upstream Responses SSE events to the downstream channel.
async fn pump_responses_events(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    upstream: reqwest::Response,
) {
    let mut byte_stream = upstream.bytes_stream();
    let mut decoder = SseRawLineDecoder::default();
    let mut terminal = false;
    let mut repaired_ids = std::collections::HashMap::new();

    while let Some(item) = byte_stream.next().await {
        match item {
            Ok(bytes) => {
                let raw_events = decoder.push_chunk(&bytes);
                for (event_name, data) in raw_events {
                    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
                        if data == "[DONE]" {
                            continue;
                        }
                        let _ = tx.send(Ok(Event::default().event("error").data("{\"type\":\"error\",\"error\":{\"message\":\"Malformed Codex event\"}}"))).await;
                        return;
                    };
                    if !repair_response_calls(&mut value, &mut repaired_ids) {
                        let _ = tx.send(Ok(Event::default().event("error").data("{\"type\":\"error\",\"error\":{\"message\":\"Malformed Codex function call\"}}"))).await;
                        return;
                    }
                    let event_type = value
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| event_name.clone());
                    if matches!(
                        event_type.as_deref(),
                        Some(
                            "response.completed"
                                | "response.done"
                                | "response.incomplete"
                                | "response.failed"
                                | "error"
                        )
                    ) {
                        terminal = true;
                    }
                    let mut sse_ev = Event::default().data(value.to_string());
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
    if !terminal {
        let _ = tx.send(Ok(Event::default().event("error").data("{\"type\":\"error\",\"error\":{\"message\":\"Codex stream ended without a terminal response\"}}"))).await;
    }
}

/// Converts upstream events into streaming chat completion chunks.
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
                                            thought_signature: None,
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
                            let Some(index) = tool_calls
                                .iter()
                                .position(|call| call.matches(key.as_deref()))
                            else {
                                let _ = tx.send(Ok(Event::default().event("error").data("{\"error\":{\"message\":\"Codex emitted an unmatched tool event\"}}"))).await;
                                return;
                            };
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
                                            thought_signature: None,
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
                            let Some(index) = tool_calls
                                .iter()
                                .position(|call| call.matches(key.as_deref()))
                            else {
                                let _ = tx.send(Ok(Event::default().event("error").data("{\"error\":{\"message\":\"Codex emitted an unmatched tool event\"}}"))).await;
                                return;
                            };
                            let Some(call) = tool_calls.get_mut(index) else {
                                continue;
                            };
                            let Some(suffix) = arguments.strip_prefix(&call.arguments) else {
                                let _ = tx.send(Ok(Event::default().event("error").data("{\"error\":{\"message\":\"Codex tool arguments changed after streaming\"}}"))).await;
                                return;
                            };
                            let suffix = suffix.to_owned();
                            call.arguments.clone_from(&arguments);
                            if suffix.is_empty() {
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
                                            thought_signature: None,
                                            function: Some(ChunkFunctionCall {
                                                name: None,
                                                arguments: Some(suffix),
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
                        CodexParsedEvent::Finish {
                            mut reason, status, ..
                        } => {
                            if !matches!(status.as_str(), "completed" | "incomplete") {
                                let _ = tx
                                    .send(Ok(Event::default().event("error").data(
                                        "{\"error\":{\"message\":\"Codex response failed\"}}",
                                    )))
                                    .await;
                                return;
                            }
                            if reason == "stop" && !tool_calls.is_empty() {
                                reason = "tool_calls".to_string();
                            }
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
        let _ = tx
            .send(Ok(Event::default().event("error").data(
                "{\"error\":{\"message\":\"Codex stream ended without a terminal response\"}}",
            )))
            .await;
        return;
    }

    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
}

/// Events extracted from the Codex Responses stream.
#[derive(Debug)]
enum CodexParsedEvent {
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
struct PendingToolCall {
    /// Identifier exposed to the caller.
    call_id: String,
    /// Upstream output item identifier, if available.
    item_id: Option<String>,
    /// Function name.
    name: String,
    /// Accumulated or final argument text.
    arguments: String,
    /// Bytes of argument deltas already forwarded to the client. The Codex
    /// backend only streams deltas for single-call turns; for parallel calls it
    /// jumps straight to `*.done` with the complete arguments, which then have
    /// to be emitted in one chunk.
    streamed_len: usize,
}

impl PendingToolCall {
    /// Checks whether a supplied key matches either upstream identifier.
    fn matches(&self, key: Option<&str>) -> bool {
        key.is_some_and(|key| key == self.call_id || self.item_id.as_deref() == Some(key))
    }
}

/// Converts a tool-call index to the chunk representation, saturating on overflow.
fn chunk_tool_index(index: usize) -> u32 {
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
fn apply_tool_event(calls: &mut Vec<PendingToolCall>, ev: CodexParsedEvent) -> bool {
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
#[derive(Default)]
struct SseLineDecoder {
    /// Decoder for complete SSE frames.
    raw: SseRawLineDecoder,
    /// Current SSE event name, used when JSON has no type.
    current_event_name: Option<String>,
}

impl SseLineDecoder {
    /// Buffers a byte chunk and parses any complete SSE data lines.
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<CodexParsedEvent> {
        let mut out = Vec::new();
        for (name, data) in self.raw.push_chunk(chunk) {
            self.current_event_name = name;
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                self.parse_json_event(&value, &mut out);
            }
        }
        out
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

/// Incrementally decodes SSE lines without interpreting their JSON payloads.
#[derive(Default)]
struct SseRawLineDecoder {
    /// Bytes not yet consumed as complete lines.
    buffer: Vec<u8>,
    /// Event name associated with the current SSE data lines.
    current_event: Option<String>,
    /// Data lines in the current event.
    data: Vec<String>,
}

impl SseRawLineDecoder {
    /// Buffers a byte chunk and returns complete event-name and data pairs.
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end();

            if line.is_empty() {
                if self.data.is_empty() {
                    self.current_event = None;
                } else {
                    out.push((self.current_event.take(), self.data.join("\n")));
                    self.data.clear();
                }
                continue;
            }

            if let Some(ev) = line.strip_prefix("event:") {
                self.current_event = Some(ev.strip_prefix(' ').unwrap_or(ev).to_string());
                continue;
            }

            if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
    /// Starts an isolated upstream and captures requests made by the real client.
    async fn fake_client(
        sse: &'static str,
    ) -> (
        CodexClient,
        tokio::sync::mpsc::Receiver<(axum::http::HeaderMap, Value)>,
    ) {
        use axum::{Json, Router, routing::post};
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let app = Router::new().route(
            "/codex/responses",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let tx = tx.clone();
                    async move {
                        tx.send((headers, body)).await.unwrap();
                        ([("content-type", "text/event-stream")], sse)
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut client = CodexClient::new();
        client.base_url = format!("http://{address}");
        (client, rx)
    }

    /// Returns an inert test credential.
    fn credentials() -> CodexCredentials {
        CodexCredentials {
            access_token: "test-token".to_string(),
            refresh_token: None,
            account_id: None,
            email: None,
            plan_type: None,
            residency: None,
            expires_at: None,
        }
    }

    /// Constructs a minimal chat request.
    fn chat_request() -> ChatCompletionRequest {
        serde_json::from_value(
            serde_json::json!({"model":"test","messages":[{"role":"user","content":"hello"}]}),
        )
        .unwrap()
    }

    /// Reads the consumer-visible SSE response.
    async fn stream_body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Missing or truncated terminal events never produce successful completion.
    #[tokio::test]
    async fn truncated_streams_report_errors() {
        let (client, _requests) = fake_client(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        )
        .await;
        let request = chat_request();
        assert!(
            client
                .complete_chat_non_streaming(&credentials(), &request, "scoped")
                .await
                .is_err()
        );
        let body = stream_body(
            client
                .stream_chat(&credentials(), &request, "scoped")
                .await
                .unwrap(),
        )
        .await;
        assert!(body.contains("partial"));
        assert!(body.contains("event: error"));
        assert!(!body.contains("[DONE]"));
        let responses: ResponseCreateRequest =
            serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
        assert!(
            client
                .complete_responses_non_streaming(&credentials(), &responses, "scoped")
                .await
                .is_err()
        );
        let body = stream_body(
            client
                .stream_responses(&credentials(), &responses, "scoped")
                .await
                .unwrap(),
        )
        .await;
        assert!(body.contains("event: error"));
    }

    /// Incomplete and failed terminal statuses remain visible to consumers.
    #[tokio::test]
    async fn terminal_statuses_are_preserved() {
        for status in ["incomplete", "failed"] {
            let sse = if status == "failed" {
                "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"failed\",\"output\":[],\"error\":{\"message\":\"failed\"}}}\n\n"
            } else {
                "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"incomplete\",\"output\":[],\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
            };
            let (client, _requests) = fake_client(sse).await;
            let response_request: ResponseCreateRequest =
                serde_json::from_value(serde_json::json!({"model":"test","input":"hello"}))
                    .unwrap();
            let response = client
                .complete_responses_non_streaming(&credentials(), &response_request, "scoped")
                .await
                .unwrap();
            assert_eq!(response["status"], status);
            let chat = client
                .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped")
                .await;
            if status == "failed" {
                assert!(chat.is_err());
                let body = stream_body(
                    client
                        .stream_chat(&credentials(), &chat_request(), "scoped")
                        .await
                        .unwrap(),
                )
                .await;
                assert!(body.contains("event: error"));
                assert!(!body.contains("[DONE]"));
            } else {
                assert_eq!(
                    chat.unwrap().choices[0].finish_reason.as_deref(),
                    Some("length")
                );
            }
        }
    }

    /// Both APIs send the middleware-derived identity as headers and cache key.
    #[tokio::test]
    async fn scoped_identity_reaches_both_upstream_apis() {
        let (client, mut requests) = fake_client("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
        let req: ResponseCreateRequest =
            serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
        client
            .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped-id")
            .await
            .unwrap();
        client
            .complete_responses_non_streaming(&credentials(), &req, "scoped-id")
            .await
            .unwrap();
        for _ in 0..2 {
            let (headers, body) = requests.recv().await.unwrap();
            assert_eq!(headers["session_id"], "scoped-id");
            assert_eq!(headers["conversation_id"], "scoped-id");
            assert_eq!(body["prompt_cache_key"], "scoped-id");
        }
    }

    /// Tool arguments and matching results survive a complete upstream round trip.
    #[tokio::test]
    async fn tool_calls_round_trip_without_changing_arguments() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/a\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        );
        let (client, mut requests) = fake_client(sse).await;
        let result = client
            .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped")
            .await
            .unwrap();
        let choice = result.choices.first().unwrap();
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        let call = choice.message.tool_calls.as_ref().unwrap().first().unwrap();
        assert_eq!(call.function.arguments, "{\"path\":\"/tmp/a\"}");
        let replay: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model":"test",
            "messages":[
                {"role":"assistant","tool_calls":[call]},
                {"role":"tool","tool_call_id":call.id,"content":"file contents"}
            ]
        }))
        .unwrap();
        client
            .complete_chat_non_streaming(&credentials(), &replay, "scoped")
            .await
            .unwrap();
        requests.recv().await.unwrap();
        let (_, payload) = requests.recv().await.unwrap();
        assert_eq!(payload["input"][0]["call_id"], call.id);
        assert_eq!(payload["input"][0]["arguments"], call.function.arguments);
        assert_eq!(payload["input"][1]["call_id"], call.id);
        assert_eq!(payload["input"][1]["output"], "file contents");
        let stream = stream_body(
            client
                .stream_chat(&credentials(), &chat_request(), "scoped")
                .await
                .unwrap(),
        )
        .await;
        assert!(stream.contains("call_1"));
        assert!(stream.contains("tool_calls"));
        assert!(stream.contains("[DONE]"));
    }

    /// Responses history keeps opaque reasoning and pairs repaired tool IDs.
    #[tokio::test]
    async fn responses_history_replays_reasoning_and_repairs_call_ids() {
        let (client, mut requests) = fake_client("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
        let request: ResponseCreateRequest = serde_json::from_value(serde_json::json!({
            "model":"test",
            "input":[
                {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thinking"}],"encrypted_content":"opaque"},
                {"type":"function_call","call_id":"bad id!","name":"read","arguments":"{\"path\":\"/tmp\"}"},
                {"type":"function_call_output","call_id":"bad id!","output":"contents"}
            ]
        })).unwrap();
        client
            .complete_responses_non_streaming(&credentials(), &request, "scoped")
            .await
            .unwrap();
        let (_, payload) = requests.recv().await.unwrap();
        assert_eq!(payload["input"][0]["encrypted_content"], "opaque");
        assert_eq!(payload["input"][0]["summary"][0]["text"], "thinking");
        let id = payload["input"][1]["call_id"].as_str().unwrap();
        assert_eq!(id, "badid");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
        assert_eq!(payload["input"][2]["call_id"], id);
        assert_eq!(payload["input"][1]["arguments"], "{\"path\":\"/tmp\"}");
        assert_eq!(payload["input"][2]["output"], "contents");
    }
}
