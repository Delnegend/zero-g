use axum::response::sse::Event;
use serde_json::json;

use crate::openai::{
    ChatCompletionChunk, ChunkChoice, ChunkDelta, ChunkFunctionCall, ChunkToolCall, Usage,
};
use crate::provider::antigravity::AntigravityEvent;
use crate::provider::antigravity::map_finish_reason;

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ChatStreamSerializer {
    pub(crate) stream_id: String,
    pub(crate) req_model: String,
    pub(crate) created: u64,
    pub(crate) include_usage: bool,
    pub(crate) sent_role: bool,
    pub(crate) has_tools: bool,
    pub(crate) finish_sent: bool,
    pub(crate) tool_call_index: u32,
    pub(crate) usage: Usage,
    pub(crate) saw_usage: bool,
}

impl crate::provider::antigravity::sse::SseSerializer for ChatStreamSerializer {
    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event> {
        match event {
            AntigravityEvent::Text(text) => self.text_event(Some(text), None),
            AntigravityEvent::Reasoning(text) => self.text_event(None, Some(text)),
            AntigravityEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                self.has_tools = true;
                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: if self.sent_role {
                                None
                            } else {
                                self.sent_role = true;
                                Some("assistant".to_string())
                            },
                            content: None,
                            reasoning_content: None,
                            tool_calls: Some(vec![ChunkToolCall {
                                index: self.tool_call_index,
                                id: Some(id),
                                r#type: Some("function".to_string()),
                                function: Some(ChunkFunctionCall {
                                    name: Some(name),
                                    arguments: Some(arguments),
                                }),
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                self.tool_call_index = self.tool_call_index.saturating_add(1);
                serde_json::to_string(&chunk)
                    .map_or_else(|_| Vec::new(), |data| vec![Event::default().data(data)])
            }
            AntigravityEvent::Finish { reason } => {
                self.finish_sent = true;
                let mapped = map_finish_reason(&reason, self.has_tools);
                let chunk = ChatCompletionChunk {
                    id: self.stream_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created: self.created,
                    model: self.req_model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta::default(),
                        finish_reason: Some(mapped),
                    }],
                    usage: None,
                };
                serde_json::to_string(&chunk)
                    .map_or_else(|_| Vec::new(), |data| vec![Event::default().data(data)])
            }
            AntigravityEvent::Usage(usage) => {
                self.usage = usage;
                self.saw_usage = true;
                Vec::new()
            }
        }
    }

    fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();

        if !self.finish_sent {
            let reason = if self.has_tools { "tool_calls" } else { "stop" };
            let chunk = ChatCompletionChunk {
                id: self.stream_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created: self.created,
                model: self.req_model.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta::default(),
                    finish_reason: Some(reason.to_string()),
                }],
                usage: None,
            };
            if let Ok(data) = serde_json::to_string(&chunk) {
                events.push(Event::default().data(data));
            }
        }

        if self.include_usage && self.saw_usage {
            let chunk = json!({
                "id": self.stream_id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.req_model,
                "choices": [],
                "usage": {
                    "prompt_tokens": self.usage.prompt_tokens,
                    "completion_tokens": self.usage.completion_tokens,
                    "total_tokens": self.usage.total_tokens
                }
            });
            events.push(Event::default().data(chunk.to_string()));
        }

        events
    }
}

impl ChatStreamSerializer {
    fn text_event(
        &mut self,
        content: Option<String>,
        reasoning_content: Option<String>,
    ) -> Vec<Event> {
        let chunk = ChatCompletionChunk {
            id: self.stream_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: self.created,
            model: self.req_model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: if self.sent_role {
                        None
                    } else {
                        self.sent_role = true;
                        Some("assistant".to_string())
                    },
                    content,
                    reasoning_content,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        serde_json::to_string(&chunk)
            .map_or_else(|_| Vec::new(), |data| vec![Event::default().data(data)])
    }
}
