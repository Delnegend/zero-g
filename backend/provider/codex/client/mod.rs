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
use crate::provider::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, Credentials};
use crate::provider::codex::payload::{
    build_codex_payload_from_chat, build_codex_payload_from_responses,
};
use crate::provider::common::errors::UpstreamError;
#[cfg(test)]
use crate::provider::common::stream::MAX_SSE_BYTES;
use crate::responses::ResponseCreateRequest;

/// Codex stream parsing and tool-call state.
mod stream;

use stream::{
    CodexParsedEvent, PendingToolCall, SseLineDecoder, apply_tool_event, chunk_tool_index,
    codex_raw_decoder,
};

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
        creds: &Credentials,
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
    pub(super) async fn stream_chat(
        &self,
        creds: &Credentials,
        req: &ChatCompletionRequest,
        session_id: &str,
    ) -> Result<Response, UpstreamError> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, payload, session_id).await?;

        let model = req.model.clone();
        let include_usage = req
            .stream_options
            .as_ref()
            .and_then(|options| options.include_usage)
            .unwrap_or(false);
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_chat_chunks(tx, upstream, model, include_usage).await;
        });

        Ok(Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response())
    }

    /// Collects an upstream stream into a single chat completion.
    pub(super) async fn complete_chat_non_streaming(
        &self,
        creds: &Credentials,
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
            let (events, decode_error) = match decoder.push_chunk(&chunk) {
                Ok(events) => (events, None),
                Err((events, error)) => (events, Some(error)),
            };
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
            if let Some(error) = decode_error {
                return Err(error.into());
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
    pub(super) async fn stream_responses(
        &self,
        creds: &Credentials,
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
    pub(super) async fn complete_responses_non_streaming(
        &self,
        creds: &Credentials,
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
            let (events, decode_error) = match decoder.push_chunk(&chunk) {
                Ok(events) => (events, None),
                Err((events, error)) => (events, Some(error)),
            };
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
            if let Some(error) = decode_error {
                return Err(error.into());
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
    let mut decoder = codex_raw_decoder();
    let mut terminal = false;
    let mut repaired_ids = std::collections::HashMap::new();

    while let Some(item) = byte_stream.next().await {
        match item {
            Ok(bytes) => {
                let (raw_events, decode_error) = match decoder.push_chunk(&bytes) {
                    Ok(events) => (events, None),
                    Err((events, error)) => (events, Some(error)),
                };
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
                    if terminal {
                        return;
                    }
                }
                if let Some(message) = decode_error {
                    let _ = tx
                        .send(Ok(Event::default().event("error").data(
                            serde_json::json!({"type":"error","error":{"message":message}})
                                .to_string(),
                        )))
                        .await;
                    return;
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

/// Serializes ordinary chunks with the required null usage field when requested.
fn serialize_chat_chunk(
    chunk: &ChatCompletionChunk,
    include_usage: bool,
) -> Result<String, serde_json::Error> {
    let mut data = serde_json::to_string(chunk)?;
    if include_usage && chunk.usage.is_none() {
        data.pop();
        data.push_str(",\"usage\":null}");
    }
    Ok(data)
}

/// Converts upstream events into streaming chat completion chunks.
// Streaming events share ordering and tool-call state throughout this loop.
#[allow(clippy::too_many_lines)]
async fn pump_chat_chunks(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    upstream: reqwest::Response,
    model: String,
    include_usage: bool,
) {
    let chunk_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut byte_stream = upstream.bytes_stream();
    let mut decoder = SseLineDecoder::default();
    let mut tool_calls: Vec<PendingToolCall> = Vec::new();
    let mut has_emitted_finish = false;
    let mut usage = None;

    while let Some(item) = byte_stream.next().await {
        match item {
            Ok(bytes) => {
                let (events, decode_error) = match decoder.push_chunk(&bytes) {
                    Ok(events) => (events, None),
                    Err((events, error)) => (events, Some(error)),
                };
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
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
                            if let Ok(data) = serialize_chat_chunk(&chunk, include_usage)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::Usage(value) => usage = Some(value),
                    }
                }
                if let Some(message) = decode_error {
                    let _ = tx
                        .send(Ok(Event::default().event("error").data(
                            serde_json::json!({"error":{"message":message}}).to_string(),
                        )))
                        .await;
                    return;
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

    if include_usage {
        let chunk = ChatCompletionChunk {
            id: chunk_id,
            object: "chat.completion.chunk".to_string(),
            created: now,
            model,
            choices: Vec::new(),
            usage,
        };
        if let Ok(data) = serialize_chat_chunk(&chunk, true)
            && tx.send(Ok(Event::default().data(data))).await.is_err()
        {
            return;
        }
    }

    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests;
