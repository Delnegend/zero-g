use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::Response;
use serde_json::{Value, json};

use crate::antigravity::AntigravityClient;
use crate::antigravity::models::{CLAUDE_THINKING_BETA_HEADER, SANDBOX_ENDPOINT};
use crate::auth::{ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials};
use crate::openai::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, FunctionCall, ToolCall, Usage,
};

impl AntigravityClient {
    pub(crate) async fn stream_request(
        &self,
        creds: &Credentials,
        payload: &Value,
        is_claude: bool,
    ) -> Result<Response, String> {
        let endpoints = [CLOUD_CODE_ASSIST_ENDPOINT, SANDBOX_ENDPOINT];
        let mut last_err = String::new();

        for endpoint in &endpoints {
            let url = format!("{endpoint}/v1internal:streamGenerateContent?alt=sse");
            let mut req_builder = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", creds.access_token))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("User-Agent", ANTIGRAVITY_USER_AGENT);

            if is_claude {
                req_builder = req_builder.header("anthropic-beta", CLAUDE_THINKING_BETA_HEADER);
            }

            match req_builder.json(payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(resp);
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    last_err = format!("Antigravity error ({status}): {body}");
                    if status.is_server_error() {
                        continue;
                    }
                    return Err(last_err);
                }
                Err(e) => {
                    last_err = format!("Request failed to {endpoint}: {e}");
                }
            }
        }

        Err(last_err)
    }

    pub(crate) async fn complete_non_streaming(
        &self,
        creds: &Credentials,
        req: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String> {
        let wire_model = Self::map_model_id(&req.model);
        let is_claude = wire_model.contains("claude");
        let payload = Self::build_request_payload(req, &creds.project_id, &wire_model);

        let response = self.stream_request(creds, &payload, is_claude).await?;
        let mut byte_stream = response.bytes_stream();

        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = Some("stop".to_string());
        let mut usage = Usage::default();

        let mut buffer = String::new();

        while let Some(chunk_res) = byte_stream.next().await {
            let bytes = chunk_res.map_err(|e| format!("Stream read error: {e}"))?;
            let chunk_str = String::from_utf8_lossy(&bytes);
            buffer.push_str(&chunk_str);

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim_end().to_string();
                buffer.drain(..=line_end);

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        continue;
                    }
                    if let Ok(parsed) = serde_json::from_str::<Value>(data)
                        && let Some(resp) = parsed.get("response")
                    {
                        if let Some(candidates) = resp.get("candidates").and_then(|c| c.as_array())
                            && let Some(cand) = candidates.first()
                        {
                            if let Some(parts) = cand
                                .get("content")
                                .and_then(|c| c.get("parts"))
                                .and_then(|p| p.as_array())
                            {
                                for part in parts {
                                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                        if part
                                            .get("thought")
                                            .and_then(serde_json::Value::as_bool)
                                            .unwrap_or(false)
                                        {
                                            full_reasoning.push_str(text);
                                        } else {
                                            full_text.push_str(text);
                                        }
                                    }
                                    if let Some(fc) = part.get("functionCall") {
                                        let name =
                                            fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                        let args =
                                            fc.get("args").cloned().unwrap_or_else(|| json!({}));
                                        let id =
                                            fc.get("id").and_then(|id| id.as_str()).map_or_else(
                                                || {
                                                    format!(
                                                        "call_{}",
                                                        uuid::Uuid::new_v4().simple()
                                                    )
                                                },
                                                std::string::ToString::to_string,
                                            );
                                        tool_calls.push(ToolCall {
                                            id,
                                            r#type: "function".to_string(),
                                            function: FunctionCall {
                                                name: name.to_string(),
                                                arguments: args.to_string(),
                                            },
                                        });
                                    }
                                }
                            }
                            if let Some(fr) = cand.get("finishReason").and_then(|f| f.as_str()) {
                                finish_reason = Some(map_finish_reason(fr, !tool_calls.is_empty()));
                            }
                        }
                        if let Some(meta) = resp.get("usageMetadata") {
                            if let Some(pt) = meta
                                .get("promptTokenCount")
                                .and_then(serde_json::Value::as_u64)
                            {
                                usage.prompt_tokens = pt as u32;
                            }
                            if let Some(ct) = meta
                                .get("candidatesTokenCount")
                                .and_then(serde_json::Value::as_u64)
                            {
                                usage.completion_tokens = ct as u32;
                            }
                            if let Some(tt) = meta
                                .get("totalTokenCount")
                                .and_then(serde_json::Value::as_u64)
                            {
                                usage.total_tokens = tt as u32;
                            }
                        }
                    }
                }
            }
        }

        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".to_string(),
            created,
            model: req.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: crate::openai::AssistantMessage {
                    role: "assistant".to_string(),
                    content: if full_text.is_empty() && !tool_calls.is_empty() {
                        None
                    } else {
                        Some(full_text)
                    },
                    reasoning_content: if full_reasoning.is_empty() {
                        None
                    } else {
                        Some(full_reasoning)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                },
                finish_reason,
            }],
            usage: Some(usage),
        })
    }
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
