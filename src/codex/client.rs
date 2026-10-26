use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexCredentials};
use crate::codex::payload::{build_codex_payload_from_chat, build_codex_payload_from_responses};
use crate::openai::{
    AssistantMessage, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice,
    ChunkChoice, ChunkDelta, ChunkFunctionCall, ChunkToolCall, FunctionCall, ToolCall, Usage,
};
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

    pub(crate) async fn send_request(
        &self,
        creds: &CodexCredentials,
        payload: &Value,
    ) -> Result<reqwest::Response, String> {
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
            return Err(format!("Codex upstream returned {status}: {body}"));
        }

        Ok(resp)
    }

    pub(crate) async fn stream_chat(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
    ) -> Response {
        let payload = build_codex_payload_from_chat(req);
        let upstream = match self.send_request(creds, &payload).await {
            Ok(r) => r,
            Err(e) => {
                error!("Codex upstream error: {e}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(crate::server::error_body(
                        &e,
                        "upstream_error",
                        Some("bad_gateway"),
                    )),
                )
                    .into_response();
            }
        };

        let model = req.model.clone();
        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_chat_chunks(tx, upstream, model).await;
        });

        Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response()
    }

    pub(crate) async fn complete_chat_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String> {
        let payload = build_codex_payload_from_chat(req);
        let upstream = self.send_request(creds, &payload).await?;

        let model = req.model.clone();
        let mut text_acc = String::new();
        let mut tool_calls_map: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
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
                    CodexParsedEvent::ToolCallStart { call_id, name } => {
                        tool_calls_map.push((call_id, name, String::new()));
                    }
                    CodexParsedEvent::ToolCallDelta { call_id, delta } => {
                        if let Some(entry) =
                            tool_calls_map.iter_mut().find(|(id, _, _)| id == &call_id)
                        {
                            entry.2.push_str(&delta);
                        } else if let Some(last) = tool_calls_map.last_mut() {
                            last.2.push_str(&delta);
                        }
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
                    .map(|(id, name, arguments)| ToolCall {
                        id,
                        r#type: "function".to_string(),
                        function: FunctionCall { name, arguments },
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
            reasoning_content: None,
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
    ) -> Response {
        let payload = build_codex_payload_from_responses(req);
        let upstream = match self.send_request(creds, &payload).await {
            Ok(r) => r,
            Err(e) => {
                error!("Codex upstream error: {e}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(crate::server::error_body(
                        &e,
                        "upstream_error",
                        Some("bad_gateway"),
                    )),
                )
                    .into_response();
            }
        };

        let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);

        tokio::spawn(async move {
            pump_responses_events(tx, upstream).await;
        });

        Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response()
    }

    pub(crate) async fn complete_responses_non_streaming(
        &self,
        creds: &CodexCredentials,
        req: &ResponseCreateRequest,
    ) -> Result<ResponseObject, String> {
        let payload = build_codex_payload_from_responses(req);
        let upstream = self.send_request(creds, &payload).await?;

        let model = req.model.clone();
        let mut output_text = String::new();
        let mut output_items = Vec::new();
        let mut tool_calls_map: Vec<(String, String, String)> = Vec::new();
        let mut usage_opt = None;

        let mut byte_stream = upstream.bytes_stream();
        let mut decoder = SseLineDecoder::default();

        while let Some(item) = byte_stream.next().await {
            let chunk = item.map_err(|e| format!("Stream read error: {e}"))?;
            let events = decoder.push_chunk(&chunk);
            for ev in events {
                match ev {
                    CodexParsedEvent::TextDelta(t) => output_text.push_str(&t),
                    CodexParsedEvent::ToolCallStart { call_id, name } => {
                        tool_calls_map.push((call_id, name, String::new()));
                    }
                    CodexParsedEvent::ToolCallDelta { call_id, delta } => {
                        if let Some(entry) =
                            tool_calls_map.iter_mut().find(|(id, _, _)| id == &call_id)
                        {
                            entry.2.push_str(&delta);
                        } else if let Some(last) = tool_calls_map.last_mut() {
                            last.2.push_str(&delta);
                        }
                    }
                    CodexParsedEvent::Usage(u) => usage_opt = Some(ResponseUsage::from(u)),
                    CodexParsedEvent::Finish { .. } => {}
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

        for (call_id, name, args_str) in tool_calls_map {
            let arguments =
                serde_json::from_str::<Value>(&args_str).unwrap_or(Value::String(args_str));
            output_items.push(ResponseOutputItem::FunctionCall {
                id: format!("fc_{}", uuid::Uuid::new_v4().simple()),
                status: "completed".to_string(),
                name,
                call_id,
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
    let mut tool_index = 0;
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
                        CodexParsedEvent::ToolCallStart { call_id, name } => {
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        tool_calls: Some(vec![ChunkToolCall {
                                            index: tool_index,
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
                            tool_index += 1;
                            if let Ok(data) = serde_json::to_string(&chunk)
                                && tx.send(Ok(Event::default().data(data))).await.is_err()
                            {
                                return;
                            }
                        }
                        CodexParsedEvent::ToolCallDelta { delta, .. } => {
                            let target_index = if tool_index > 0 { tool_index - 1 } else { 0 };
                            let chunk = ChatCompletionChunk {
                                id: chunk_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created: now,
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        tool_calls: Some(vec![ChunkToolCall {
                                            index: target_index,
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
                finish_reason: Some(if tool_index > 0 { "tool_calls" } else { "stop" }.to_string()),
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
    ToolCallStart { call_id: String, name: String },
    ToolCallDelta { call_id: String, delta: String },
    Finish { reason: String },
    Usage(Usage),
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

            if let Some(ev) = line.strip_prefix("event: ") {
                self.current_event_name = Some(ev.trim().to_string());
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
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
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("function")
                            .to_string();
                        out.push(CodexParsedEvent::ToolCallStart { call_id, name });
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                let call_id = val
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                    out.push(CodexParsedEvent::ToolCallDelta {
                        call_id,
                        delta: delta.to_string(),
                    });
                }
            }
            Some("response.completed" | "response.done") => {
                if let Some(usage_obj) = val.pointer("/response/usage") {
                    let prompt_tokens = usage_obj
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    let completion_tokens = usage_obj
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    let total_tokens = usage_obj
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::from(prompt_tokens + completion_tokens))
                        as u32;

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
