use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Response;
use serde_json::Value;

use crate::antigravity::AntigravityClient;
use crate::antigravity::auth::{ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials};
use crate::antigravity::models::{CLAUDE_THINKING_BETA_HEADER, SANDBOX_ENDPOINT};
use crate::antigravity::{AntigravityEvent, map_finish_reason, spawn_event_stream};
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
        let wire_model = req.model.clone();
        let is_claude = wire_model.contains("claude");
        let payload = Self::build_request_payload(req, &creds.project_id, &wire_model);

        let response = self.stream_request(creds, &payload, is_claude).await?;
        let mut events = spawn_event_stream(response.bytes_stream());

        let mut collected = CollectedResponse::default();
        while let Some(event) = events.recv().await {
            match event {
                Ok(event) => collected.apply(event),
                Err(error) => return Err(error),
            }
        }

        Ok(collected.into_response(req.model.clone()))
    }
}

#[derive(Default)]
struct CollectedResponse {
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    usage: Usage,
}

impl CollectedResponse {
    fn apply(&mut self, event: AntigravityEvent) {
        match event {
            AntigravityEvent::Text(text) => self.text.push_str(&text),
            AntigravityEvent::Reasoning(text) => self.reasoning.push_str(&text),
            AntigravityEvent::ToolCall {
                id,
                name,
                arguments,
            } => self.tool_calls.push(ToolCall {
                id,
                r#type: "function".to_string(),
                function: FunctionCall { name, arguments },
            }),
            AntigravityEvent::Finish { reason } => self.finish_reason = Some(reason),
            AntigravityEvent::Usage(usage) => self.usage = usage,
        }
    }

    fn into_response(self, model: String) -> ChatCompletionResponse {
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());

        let has_tools = !self.tool_calls.is_empty();
        let finish_reason = self.finish_reason.as_deref().map_or_else(
            || Some("stop".to_string()),
            |reason| Some(map_finish_reason(reason, has_tools)),
        );

        let content = if self.text.is_empty() && has_tools {
            None
        } else {
            Some(self.text)
        };
        let reasoning_content = if self.reasoning.is_empty() {
            None
        } else {
            Some(self.reasoning)
        };
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls)
        };

        ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".to_string(),
            created,
            model,
            choices: vec![Choice {
                index: 0,
                message: crate::openai::AssistantMessage {
                    role: "assistant".to_string(),
                    content,
                    reasoning_content,
                    tool_calls,
                },
                finish_reason,
            }],
            usage: Some(self.usage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::antigravity::AntigravityEvent;
    use crate::openai::Usage;

    #[test]
    fn collected_response_maps_tool_finish() {
        let mut collected = CollectedResponse::default();
        collected.apply(AntigravityEvent::ToolCall {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            arguments: "{\"city\":\"Paris\"}".to_string(),
        });
        collected.apply(AntigravityEvent::Finish {
            reason: "STOP".to_string(),
        });
        collected.apply(AntigravityEvent::Usage(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            total_tokens: 6,
        }));

        let response = collected.into_response("claude-sonnet".to_string());

        assert_eq!(
            response.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        assert!(response.choices[0].message.content.is_none());
        assert_eq!(
            response.choices[0]
                .message
                .tool_calls
                .as_ref()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            response.usage,
            Some(Usage {
                prompt_tokens: 4,
                completion_tokens: 2,
                total_tokens: 6,
            })
        );
    }
}
