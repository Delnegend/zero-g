use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

use crate::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, FunctionDefinition, MessageContent, Tool,
    ToolCall,
};

/// Request to create an OpenAI-compatible response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseCreateRequest {
    /// Requested model identifier.
    pub(crate) model: String,
    /// Prompt text or ordered input items.
    pub(crate) input: ResponseInput,
    /// Optional system instructions.
    pub(crate) instructions: Option<String>,
    /// Whether to stream response events.
    #[serde(default)]
    pub(crate) stream: Option<bool>,
    /// Available tools in Responses or chat format.
    #[serde(default, deserialize_with = "deserialize_tools")]
    pub(crate) tools: Option<Vec<Tool>>,
    /// Requested tool selection policy.
    pub(crate) tool_choice: Option<Value>,
    /// Sampling temperature.
    pub(crate) temperature: Option<f32>,
    /// Nucleus sampling threshold.
    pub(crate) top_p: Option<f32>,
    /// Maximum generated tokens.
    pub(crate) max_output_tokens: Option<u32>,
    /// Requested reasoning effort.
    pub(crate) reasoning_effort: Option<String>,
    /// Provider-specific reasoning options.
    pub(crate) reasoning: Option<Value>,
}

/// Responses input as a prompt or a sequence of items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponseInput {
    /// Plain text prompt.
    Text(String),
    /// Ordered conversation and tool items.
    Items(Vec<ResponseInputItem>),
}

/// An input item in Responses or chat message form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponseInputItem {
    /// Item with a Responses API type discriminator.
    Tagged(ResponseTaggedItem),
    /// Chat-format message.
    Message(ChatMessage),
}

/// Typed Responses API input item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseTaggedItem {
    /// Conversation message.
    #[serde(rename = "message")]
    Message {
        /// Speaker role, defaulting to user.
        #[serde(default = "default_user_role")]
        role: String,
        /// Text or multimodal content.
        #[serde(default)]
        content: Option<MessageContent>,
        /// Optional participant name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Tool calls associated with the message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    /// Function call emitted by an assistant.
    #[serde(rename = "function_call")]
    FunctionCall {
        /// Call identifier used to match the result.
        #[serde(default)]
        call_id: Option<String>,
        /// Alternative item identifier.
        #[serde(default)]
        id: Option<String>,
        /// Function name.
        name: String,
        /// Function arguments as a JSON value or string.
        arguments: Value,
    },
    /// Result of an earlier function call.
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        /// Identifier of the call being answered.
        #[serde(default)]
        call_id: Option<String>,
        /// Function result as a JSON value or string.
        output: Value,
    },
}

/// Supplies the default role for a typed message.
fn default_user_role() -> String {
    "user".to_string()
}

/// Function tools arrive in two shapes. The Responses API puts `name`,
/// `description`, and `parameters` at the top level; Chat Completions nests
/// them under `function`. Both normalize to the chat shape used internally,
/// and `codex::payload` re-emits the flat wire form upstream.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ToolDefinition {
    /// Flat function definition used by the Responses API.
    Responses {
        /// Function name.
        name: String,
        /// Optional function description.
        #[serde(default)]
        description: Option<String>,
        /// JSON schema for function arguments.
        #[serde(default)]
        parameters: Option<Value>,
    },
    /// Nested function definition used by chat completions.
    Chat(Tool),
}

impl From<ToolDefinition> for Tool {
    fn from(tool: ToolDefinition) -> Self {
        match tool {
            ToolDefinition::Responses {
                name,
                description,
                parameters,
            } => Self {
                r#type: "function".to_string(),
                function: FunctionDefinition {
                    name,
                    description,
                    parameters,
                },
            },
            ToolDefinition::Chat(tool) => tool,
        }
    }
}

/// Accepts either supported tool definition shape and normalizes to chat tools.
fn deserialize_tools<'de, D>(deserializer: D) -> Result<Option<Vec<Tool>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let tools = Option::<Vec<ToolDefinition>>::deserialize(deserializer)?;
    Ok(tools.map(|tools| tools.into_iter().map(Tool::from).collect()))
}

impl ResponseCreateRequest {
    /// Converts Responses input and options to a chat completion request.
    pub(crate) fn into_chat_request(self) -> ChatCompletionRequest {
        let mut messages = Vec::new();
        push_system_message(&mut messages, self.instructions);

        match self.input {
            ResponseInput::Text(prompt) => push_user_text(&mut messages, prompt),
            ResponseInput::Items(items) => {
                messages.extend(items.into_iter().map(ChatMessage::from));
            }
        }

        ChatCompletionRequest {
            model: self.model,
            messages,
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens: self.max_output_tokens,
            max_completion_tokens: self.max_output_tokens,
            stream: self.stream,
            stream_options: None,
            tools: self.tools,
            tool_choice: self.tool_choice,
            reasoning_effort: self.reasoning_effort,
            reasoning: self.reasoning,
        }
    }
}

/// Adds nonblank instructions as a system message.
fn push_system_message(messages: &mut Vec<ChatMessage>, instructions: Option<String>) {
    if let Some(instructions) = instructions
        && !instructions.trim().is_empty()
    {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(instructions)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
}

/// Adds a nonblank prompt as a user message.
fn push_user_text(messages: &mut Vec<ChatMessage>, prompt: String) {
    if !prompt.trim().is_empty() {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(prompt)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
}

/// Preserves strings and serializes other JSON values for chat tool messages.
fn value_to_string(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

impl From<ResponseInputItem> for ChatMessage {
    fn from(item: ResponseInputItem) -> Self {
        match item {
            ResponseInputItem::Message(message) => message,
            ResponseInputItem::Tagged(tagged) => Self::from(tagged),
        }
    }
}

impl From<ResponseTaggedItem> for ChatMessage {
    fn from(tagged: ResponseTaggedItem) -> Self {
        match tagged {
            ResponseTaggedItem::Message {
                role,
                content,
                name,
                tool_calls,
            } => Self {
                role,
                content,
                name,
                tool_calls,
                tool_call_id: None,
            },
            ResponseTaggedItem::FunctionCall {
                call_id,
                id,
                name,
                arguments,
            } => {
                let call_id = call_id.or(id).unwrap_or_default();
                Self {
                    role: "assistant".to_string(),
                    content: None,
                    name: None,
                    tool_calls: Some(vec![ToolCall {
                        id: call_id,
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name,
                            arguments: value_to_string(arguments),
                        },
                    }]),
                    tool_call_id: None,
                }
            }
            ResponseTaggedItem::FunctionCallOutput { call_id, output } => Self {
                role: "tool".to_string(),
                content: Some(MessageContent::Text(value_to_string(output))),
                name: None,
                tool_calls: None,
                tool_call_id: call_id,
            },
        }
    }
}

/// Completed Responses API object.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseObject {
    /// Response identifier.
    pub(crate) id: String,
    /// Wire object type.
    pub(crate) object: String,
    /// Creation time as a Unix timestamp.
    pub(crate) created_at: u64,
    /// Model that generated the response.
    pub(crate) model: String,
    /// Response lifecycle status.
    pub(crate) status: String,
    /// Generated messages and function calls.
    pub(crate) output: Vec<ResponseOutputItem>,
    /// Aggregated output text.
    pub(crate) output_text: String,
    /// Token accounting, if supplied.
    pub(crate) usage: Option<ResponseUsage>,
}

/// Generated item in a Responses API object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseOutputItem {
    /// Generated assistant message.
    #[serde(rename = "message")]
    Message {
        /// Item identifier.
        id: String,
        /// Item lifecycle status.
        status: String,
        /// Message role.
        role: String,
        /// Generated content parts.
        content: Vec<ResponseContentPart>,
    },
    /// Generated function invocation.
    #[serde(rename = "function_call")]
    FunctionCall {
        /// Item identifier.
        id: String,
        /// Item lifecycle status.
        status: String,
        /// Function name.
        name: String,
        /// Identifier used to match the function result.
        call_id: String,
        /// JSON-encoded function arguments.
        arguments: String,
    },
}

/// Typed content within a generated message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseContentPart {
    /// Generated output text.
    #[serde(rename = "output_text")]
    OutputText {
        /// Text payload.
        text: String,
    },
}

/// Token accounting in Responses API format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// These names are the Responses API usage schema.
#[allow(clippy::struct_field_names)]
pub(crate) struct ResponseUsage {
    /// Input token count.
    pub(crate) input_tokens: u32,
    /// Generated token count.
    pub(crate) output_tokens: u32,
    /// Combined token count.
    pub(crate) total_tokens: u32,
}

impl From<crate::openai::Usage> for ResponseUsage {
    fn from(usage: crate::openai::Usage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

/// Responses request conversion coverage.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Converts prompt text and instructions into chat messages.
    #[test]
    fn test_responses_input_text_conversion() {
        let req = ResponseCreateRequest {
            model: "claude-sonnet".to_string(),
            input: ResponseInput::Text("What is machine learning?".to_string()),
            instructions: Some("Be concise.".to_string()),
            stream: Some(false),
            tools: None,
            tool_choice: None,
            temperature: Some(0.5),
            top_p: None,
            max_output_tokens: Some(1024),
            reasoning_effort: None,
            reasoning: None,
        };

        let chat_req = req.into_chat_request();
        assert_eq!(chat_req.model, "claude-sonnet");
        assert_eq!(chat_req.messages.len(), 2);
        assert_eq!(chat_req.messages[0].role, "system");
        assert_eq!(
            chat_req.messages[0].content.as_ref().unwrap().as_text(),
            "Be concise."
        );
        assert_eq!(chat_req.messages[1].role, "user");
        assert_eq!(
            chat_req.messages[1].content.as_ref().unwrap().as_text(),
            "What is machine learning?"
        );
        assert_eq!(chat_req.max_tokens, Some(1024));
    }

    /// Accepts Responses input text parts in message items.
    #[test]
    fn test_responses_input_items_accept_responses_content_parts() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "instructions": "You are a coding agent.",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "say OK"}]
                }
            ]
        });

        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();
        let chat_req = req.into_chat_request();
        assert_eq!(chat_req.messages.len(), 2);
        assert_eq!(chat_req.messages[1].role, "user");
        assert_eq!(
            chat_req.messages[1].content.as_ref().unwrap().as_text(),
            "say OK"
        );
    }

    /// Converts messages, function calls, and function results in order.
    #[test]
    fn test_responses_input_items_conversion() {
        let json_val = json!({
            "model": "gemini-flash",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": "What is the weather in Paris?"
                },
                {
                    "type": "function_call",
                    "call_id": "call_123",
                    "name": "get_weather",
                    "arguments": "{\"city\": \"Paris\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_123",
                    "output": "{\"temp\": 20}"
                }
            ]
        });

        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();

        let chat_req = req.into_chat_request();
        assert_eq!(chat_req.messages.len(), 3);
        assert_eq!(chat_req.messages[0].role, "user");
        assert_eq!(
            chat_req.messages[0].content.as_ref().unwrap().as_text(),
            "What is the weather in Paris?"
        );
        assert_eq!(chat_req.messages[1].role, "assistant");
        assert_eq!(chat_req.messages[1].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(chat_req.messages[2].role, "tool");
        assert_eq!(
            chat_req.messages[2].tool_call_id.as_deref(),
            Some("call_123")
        );
        assert_eq!(
            chat_req.messages[2].content.as_ref().unwrap().as_text(),
            "{\"temp\": 20}"
        );
    }

    /// Accepts flat Responses function definitions.
    #[test]
    fn test_responses_tools_accept_flat_responses_shape() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "input": "say OK",
            "tools": [
                {
                    "type": "function",
                    "name": "ping",
                    "description": "Ping the host",
                    "parameters": {"type": "object", "properties": {}},
                    "strict": false
                }
            ]
        });

        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();
        let tools = req.into_chat_request().tools.unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].r#type, "function");
        assert_eq!(tools[0].function.name, "ping");
        assert_eq!(
            tools[0].function.description.as_deref(),
            Some("Ping the host")
        );
        assert!(tools[0].function.parameters.is_some());
    }

    /// Accepts nested chat function definitions.
    #[test]
    fn test_responses_tools_accept_nested_chat_shape() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "input": "say OK",
            "tools": [
                {
                    "type": "function",
                    "function": {"name": "ping", "parameters": {"type": "object"}}
                }
            ]
        });

        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();
        let tools = req.into_chat_request().tools.unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "ping");
        assert!(tools[0].function.description.is_none());
    }
}
