//! Client-facing response collection and streaming serialization.
use std::time::{SystemTime, UNIX_EPOCH};

use crate::openai::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, FunctionCall, ToolCall, Usage,
};
use crate::provider::antigravity::AntigravityClient;
use crate::provider::antigravity::auth::Credentials;
use crate::provider::antigravity::{AntigravityEvent, map_finish_reason, spawn_event_stream};
use crate::provider::common::errors::UpstreamError;

pub(super) mod chat;
pub(super) mod responses;
pub(super) mod sse;

impl AntigravityClient {
    /// Collect a streaming generation into a non-streaming chat response.
    pub(super) async fn complete_non_streaming(
        &self,
        creds: &Credentials,
        req: &ChatCompletionRequest,
        session_id: &str,
    ) -> Result<ChatCompletionResponse, UpstreamError> {
        let wire_model = req.model.clone();
        let is_claude = wire_model.contains("claude");
        let models = self.fetch_models(creds).await?;
        let info = models
            .iter()
            .find(|(id, _)| id == &wire_model)
            .map(|(_, info)| info);
        let payload =
            Self::build_request_payload(req, &creds.project_id, &wire_model, session_id, info);

        let response = self.stream_request(creds, &payload, is_claude).await?;
        let mut events = spawn_event_stream(response.bytes_stream());

        let mut collected = CollectedResponse::default();
        while let Some(event) = events.recv().await {
            match event {
                Ok(event) => collected.apply(event),
                Err(error) => return Err(error.into()),
            }
        }
        if collected.finish_reason.is_none() {
            return Err("Upstream stream ended without a finish reason"
                .to_string()
                .into());
        }
        for call in &mut collected.tool_calls {
            if let Some(signature) = &mut call.thought_signature {
                *signature = super::scoped_signature(&req.model, signature);
            }
        }

        Ok(collected.into_response(req.model.clone()))
    }
}

/// Accumulated events for a non-streaming completion.
#[derive(Default)]
struct CollectedResponse {
    /// Generated response text.
    text: String,
    /// Generated reasoning text.
    reasoning: String,
    /// Ordered generated text and reasoning parts with optional upstream signatures.
    parts: Vec<(String, bool, Option<String>)>,
    /// Generated function calls.
    tool_calls: Vec<ToolCall>,
    /// Upstream finish reason, if received.
    finish_reason: Option<String>,
    /// Latest token usage reported by the stream.
    usage: Usage,
}

impl CollectedResponse {
    /// Incorporate one upstream event into the accumulated response.
    fn apply(&mut self, event: AntigravityEvent) {
        match event {
            AntigravityEvent::Text(text) => {
                self.text.push_str(&text);
                self.parts.push((text, false, None));
            }
            AntigravityEvent::Reasoning(text) => {
                self.reasoning.push_str(&text);
                self.parts.push((text, true, None));
            }
            AntigravityEvent::SignedPart {
                text,
                reasoning,
                signature,
            } => {
                if reasoning {
                    self.reasoning.push_str(&text);
                } else {
                    self.text.push_str(&text);
                }
                self.parts.push((text, reasoning, Some(signature)));
            }
            AntigravityEvent::ToolCall {
                id,
                name,
                arguments,
                thought_signature,
            } => self.tool_calls.push(ToolCall {
                id,
                r#type: "function".to_string(),
                function: FunctionCall { name, arguments },
                thought_signature,
            }),
            AntigravityEvent::Finish { reason } => self.finish_reason = Some(reason),
            AntigravityEvent::Usage(usage) => self.usage = usage,
        }
    }

    /// Convert accumulated events into an OpenAI-compatible response.
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

        let provider_parts = (!self.parts.is_empty()).then(|| {
            self.parts
                .iter()
                .map(|(text, reasoning, signature)| {
                    super::provider_part(&model, text, *reasoning, signature.as_deref())
                })
                .collect()
        });
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
                    provider_parts,
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
    use crate::openai::Usage;
    use crate::provider::antigravity::AntigravityEvent;

    #[test]
    fn collected_response_maps_tool_finish() {
        let mut collected = CollectedResponse::default();
        collected.apply(AntigravityEvent::ToolCall {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            arguments: "{\"city\":\"Paris\"}".to_string(),
            thought_signature: None,
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
