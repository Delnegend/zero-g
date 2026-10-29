use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

/// OpenAI-compatible chat completion request.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionRequest {
    /// Requested model identifier.
    pub model: String,
    /// Conversation history in order.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f32>,
    /// Legacy output token limit.
    pub max_tokens: Option<u32>,
    /// Completion token limit.
    pub max_completion_tokens: Option<u32>,
    /// Whether to stream completion chunks.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Additional streaming options.
    pub stream_options: Option<StreamOptions>,
    /// Tools available to the model.
    pub tools: Option<Vec<Tool>>,
    /// Requested tool selection policy.
    pub tool_choice: Option<Value>,
    /// Requested reasoning effort.
    pub reasoning_effort: Option<String>,
    /// Provider-specific reasoning options.
    pub reasoning: Option<Value>,
}

/// A message in an OpenAI-compatible conversation.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    /// Speaker role.
    pub role: String,
    /// Text or multimodal content, if present.
    #[serde(default)]
    pub content: Option<MessageContent>,
    /// Optional participant name.
    pub name: Option<String>,
    /// Tool calls emitted by the assistant.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Call identifier associated with a tool result.
    pub tool_call_id: Option<String>,
}

/// Message content as plain text or typed parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    /// Plain text content.
    Text(String),
    /// Ordered multimodal content parts.
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Concatenates text parts, ignoring images.
    pub(crate) fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        ContentPart::Text { text }
                        | ContentPart::InputText { text }
                        | ContentPart::OutputText { text } => out.push_str(text),
                        ContentPart::ImageUrl { .. } | ContentPart::InputImage { .. } => {}
                    }
                }
                out
            }
        }
    }
}

/// Typed text or image content carried by a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentPart {
    /// Chat-style text content.
    #[serde(rename = "text")]
    Text {
        /// Text payload.
        text: String,
    },
    /// Responses API input text.
    #[serde(rename = "input_text")]
    InputText {
        /// Text payload.
        text: String,
    },
    /// Responses API output text.
    #[serde(rename = "output_text")]
    OutputText {
        /// Text payload.
        text: String,
    },
    /// Chat-style image URL.
    #[serde(rename = "image_url")]
    ImageUrl {
        /// Image location.
        image_url: ImageUrl,
    },
    /// Responses API input image.
    #[serde(rename = "input_image")]
    InputImage {
        /// Optional image location.
        #[serde(default)]
        image_url: Option<String>,
    },
}

/// Image location in chat content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImageUrl {
    /// URL or data URL for the image.
    pub url: String,
}

/// Tool definition in chat completion format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Tool {
    /// Tool type discriminator.
    pub r#type: String,
    /// Function exposed to the model.
    pub function: FunctionDefinition,
}

/// Callable function advertised to the model.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionDefinition {
    /// Function name.
    pub name: String,
    /// Human-readable function description.
    pub description: Option<String>,
    /// JSON schema for function arguments.
    pub parameters: Option<Value>,
}

/// Assistant request to invoke a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolCall {
    /// Tool call identifier.
    pub id: String,
    /// Tool type discriminator.
    pub r#type: String,
    /// Function name and serialized arguments.
    pub function: FunctionCall,
}

/// Function invocation emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionCall {
    /// Function name.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// Complete non-streaming chat response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    /// Completion identifier.
    pub id: String,
    /// Wire object type.
    pub object: String,
    /// Creation time as a Unix timestamp.
    pub created: u64,
    /// Model that generated the response.
    pub model: String,
    /// Generated choices.
    pub choices: Vec<Choice>,
    /// Token accounting, if supplied.
    pub usage: Option<Usage>,
}

/// One complete chat response choice.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Choice {
    /// Choice position.
    pub index: u32,
    /// Generated assistant message.
    pub message: AssistantMessage,
    /// Reason generation stopped.
    pub finish_reason: Option<String>,
}

/// Assistant message in a complete response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AssistantMessage {
    /// Message role.
    pub role: String,
    /// Generated text, if present.
    pub content: Option<String>,
    /// Generated reasoning text, if present.
    pub reasoning_content: Option<String>,
    /// Requested tool invocations.
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// One streaming chat completion chunk.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunk {
    /// Completion identifier shared by chunks.
    pub id: String,
    /// Wire object type.
    pub object: String,
    /// Creation time as a Unix timestamp.
    pub created: u64,
    /// Model generating the response.
    pub model: String,
    /// Incremental choices in this chunk.
    pub choices: Vec<ChunkChoice>,
    /// Token accounting, if supplied.
    pub usage: Option<Usage>,
}

/// Incremental content for one streaming choice.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkChoice {
    /// Choice position.
    pub index: u32,
    /// Incremental message fields.
    pub delta: ChunkDelta,
    /// Reason generation stopped, if finished.
    pub finish_reason: Option<String>,
}

/// Incremental assistant message fields.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChunkDelta {
    /// Message role, when supplied.
    pub role: Option<String>,
    /// Generated text fragment.
    pub content: Option<String>,
    /// Reasoning text fragment.
    pub reasoning_content: Option<String>,
    /// Incremental tool calls.
    pub tool_calls: Option<Vec<ChunkToolCall>>,
}

/// Incremental tool call in a streaming response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkToolCall {
    /// Tool call position within the choice.
    pub index: u32,
    /// Tool call identifier, when supplied.
    pub id: Option<String>,
    /// Tool type discriminator, when supplied.
    pub r#type: Option<String>,
    /// Incremental function invocation.
    pub function: Option<ChunkFunctionCall>,
}

/// Incremental function name and arguments.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkFunctionCall {
    /// Function name, when supplied.
    pub name: Option<String>,
    /// Serialized argument fragment.
    pub arguments: Option<String>,
}

/// Token usage reported by a chat completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// These names are the OpenAI usage schema.
#[allow(clippy::struct_field_names)]
pub(crate) struct Usage {
    /// Input token count.
    pub prompt_tokens: u32,
    /// Generated token count.
    pub completion_tokens: u32,
    /// Combined token count.
    pub total_tokens: u32,
}

/// Optional settings for streamed completions.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamOptions {
    /// Whether the stream should include token usage.
    pub include_usage: Option<bool>,
}

/// OpenAI-compatible model listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelListResponse {
    /// Wire object type.
    pub object: String,
    /// Available model entries.
    pub data: Vec<ModelEntry>,
}

/// Model metadata returned by the listing endpoint.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelEntry {
    /// Model identifier used in requests.
    pub id: String,
    /// Wire object type.
    pub object: String,
    /// Creation time as a Unix timestamp.
    pub created: u64,
    /// Model owner identifier.
    pub owned_by: String,
    /// Optional model name.
    pub name: Option<String>,
    /// Optional human-readable display name.
    pub display_name: Option<String>,
    /// Whether the model supports reasoning.
    pub reasoning: Option<bool>,
    /// Default reasoning effort, if specified.
    pub default_reasoning_effort: Option<String>,
    /// Supported reasoning effort values.
    pub supported_reasoning_efforts: Option<Vec<String>>,
    /// Maximum context size in tokens.
    pub context_window: Option<u64>,
    /// Maximum output size in tokens.
    pub max_output_tokens: Option<u32>,
}
