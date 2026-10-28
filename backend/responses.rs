use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

use crate::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, FunctionDefinition, MessageContent, Tool,
    ToolCall,
};

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseCreateRequest {
    pub(crate) model: String,
    pub(crate) input: ResponseInput,
    pub(crate) instructions: Option<String>,
    #[serde(default)]
    pub(crate) stream: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_tools")]
    pub(crate) tools: Option<Vec<Tool>>,
    pub(crate) tool_choice: Option<Value>,
    pub(crate) temperature: Option<f32>,
    pub(crate) top_p: Option<f32>,
    pub(crate) max_output_tokens: Option<u32>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponseInput {
    Text(String),
    Items(Vec<ResponseInputItem>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponseInputItem {
    Tagged(ResponseTaggedItem),
    Message(ChatMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseTaggedItem {
    #[serde(rename = "message")]
    Message {
        #[serde(default = "default_user_role")]
        role: String,
        #[serde(default)]
        content: Option<MessageContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        id: Option<String>,
        name: String,
        arguments: Value,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        #[serde(default)]
        call_id: Option<String>,
        output: Value,
    },
}

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
    Responses {
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        parameters: Option<Value>,
    },
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

fn deserialize_tools<'de, D>(deserializer: D) -> Result<Option<Vec<Tool>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let tools = Option::<Vec<ToolDefinition>>::deserialize(deserializer)?;
    Ok(tools.map(|tools| tools.into_iter().map(Tool::from).collect()))
}

impl ResponseCreateRequest {
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

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseObject {
    pub(crate) id: String,
    pub(crate) object: String,
    pub(crate) created_at: u64,
    pub(crate) model: String,
    pub(crate) status: String,
    pub(crate) output: Vec<ResponseOutputItem>,
    pub(crate) output_text: String,
    pub(crate) usage: Option<ResponseUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseOutputItem {
    #[serde(rename = "message")]
    Message {
        id: String,
        status: String,
        role: String,
        content: Vec<ResponseContentPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        id: String,
        status: String,
        name: String,
        call_id: String,
        arguments: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponseContentPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// These names are the Responses API usage schema.
#[allow(clippy::struct_field_names)]
pub(crate) struct ResponseUsage {
    pub(crate) input_tokens: u32,
    pub(crate) output_tokens: u32,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

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
