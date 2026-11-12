use std::collections::HashMap;
use std::fmt::Write;

use base64::{Engine, engine::general_purpose::STANDARD};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::proto;
use crate::openai::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent, ToolCall};

/// Historical conversation record construction.
mod history;

use history::history_records;

/// A run request and the content-addressed records the server may request.
pub(super) struct Replay {
    /// Initial protocol request.
    pub(super) request: proto::AgentRunRequest,
    /// History records indexed by SHA-256 digest.
    pub(super) blobs: HashMap<Vec<u8>, Vec<u8>>,
    /// Client tools supplied during the context handshake.
    pub(super) tools: Vec<proto::McpToolDefinition>,
    /// System instructions supplied as Cursor rules.
    pub(super) rules: Vec<proto::CursorRule>,
}

/// A JSON value in the `google.protobuf.Value` wire format.
#[derive(Clone, PartialEq, Message)]
struct JsonValue {
    /// Selected JSON value variant.
    #[prost(oneof = "json_value::Kind", tags = "1, 2, 3, 4, 5, 6")]
    kind: Option<json_value::Kind>,
}
/// Variants of a protobuf JSON value.
mod json_value {
    /// Supported JSON value variants.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(super) enum Kind {
        /// JSON null.
        #[prost(enumeration = "Null", tag = "1")]
        Null(i32),
        /// JSON number.
        #[prost(double, tag = "2")]
        Number(f64),
        /// JSON string.
        #[prost(string, tag = "3")]
        String(String),
        /// JSON boolean.
        #[prost(bool, tag = "4")]
        Bool(bool),
        /// JSON object.
        #[prost(message, tag = "5")]
        Object(super::JsonObject),
        /// JSON array.
        #[prost(message, tag = "6")]
        Array(super::JsonArray),
    }
    /// Protobuf null enumeration.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
    pub(super) enum Null {
        /// The sole protobuf null value.
        Null = 0,
    }
}
/// A protobuf JSON object.
#[derive(Clone, PartialEq, Message)]
struct JsonObject {
    /// Object entries.
    #[prost(map = "string, message", tag = "1")]
    fields: HashMap<String, JsonValue>,
}
/// A protobuf JSON array.
#[derive(Clone, PartialEq, Message)]
struct JsonArray {
    /// Array elements.
    #[prost(message, repeated, tag = "1")]
    values: Vec<JsonValue>,
}

/// Converts a JSON value to the standard protobuf representation.
fn protobuf_json(value: &Value) -> JsonValue {
    use json_value::Kind;
    let kind = match value {
        Value::Null => Kind::Null(0),
        Value::Bool(v) => Kind::Bool(*v),
        Value::Number(v) => Kind::Number(v.as_f64().unwrap_or_default()),
        Value::String(v) => Kind::String(v.clone()),
        Value::Array(v) => Kind::Array(JsonArray {
            values: v.iter().map(protobuf_json).collect(),
        }),
        Value::Object(v) => Kind::Object(JsonObject {
            fields: v
                .iter()
                .map(|(k, v)| (k.clone(), protobuf_json(v)))
                .collect(),
        }),
    };
    JsonValue { kind: Some(kind) }
}

/// Stores a blob under its SHA-256 digest.
fn store(blobs: &mut HashMap<Vec<u8>, Vec<u8>>, bytes: Vec<u8>) -> Vec<u8> {
    let id = Sha256::digest(&bytes).to_vec();
    blobs.insert(id.clone(), bytes);
    id
}

/// Serializes and stores a JSON record.
fn store_json(blobs: &mut HashMap<Vec<u8>, Vec<u8>>, value: &Value) -> Result<Vec<u8>, String> {
    Ok(store(
        blobs,
        serde_json::to_vec(value).map_err(|e| e.to_string())?,
    ))
}

/// Normalizes a tool-call identifier to Cursor's accepted character set.
fn tool_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// Derives a stable UUID-shaped identifier from a seed.
fn deterministic_id(seed: &str) -> String {
    let hash = Sha256::digest(seed.as_bytes());
    let mut result = String::with_capacity(36);
    for (index, byte) in hash.iter().take(16).enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            result.push('-');
        }
        let _ = write!(result, "{byte:02x}");
    }
    result
}

/// Extracts text from a chat message.
fn text(message: &ChatMessage) -> String {
    message
        .content
        .as_ref()
        .map_or_else(String::new, MessageContent::as_text)
}

/// Cursor embeds image bytes in `SelectedContext`; remote URLs cannot be used as bytes.
fn user_message(message: &ChatMessage, id: String) -> Result<proto::UserMessage, String> {
    let mut images = Vec::new();
    if let Some(MessageContent::Parts(parts)) = &message.content {
        for part in parts {
            let url = match part {
                ContentPart::ImageUrl { image_url } => Some(image_url.url.as_str()),
                ContentPart::InputImage { image_url } => image_url.as_deref(),
                ContentPart::Text { .. }
                | ContentPart::InputText { .. }
                | ContentPart::OutputText { .. } => None,
                _ => return Err("Cursor does not support this input content type".into()),
            };
            if let Some(url) = url {
                let (metadata, data) = url
                    .strip_prefix("data:")
                    .and_then(|s| s.split_once(','))
                    .ok_or_else(|| "Cursor images must be base64 data URLs".to_string())?;
                let mime = metadata
                    .strip_suffix(";base64")
                    .filter(|s| s.starts_with("image/"))
                    .ok_or_else(|| {
                        "Cursor images must have an image MIME type and base64 encoding".to_string()
                    })?;
                let data = STANDARD
                    .decode(data)
                    .map_err(|e| format!("Invalid image data: {e}"))?;
                images.push(proto::SelectedImage {
                    uuid: deterministic_id(&format!("{id}:{url}")),
                    mime_type: mime.to_string(),
                    data_or_blob_id: Some(proto::selected_image::DataOrBlobId::Data(data)),
                    ..Default::default()
                });
            } else if matches!(part, ContentPart::InputImage { .. }) {
                return Err("Cursor image is missing its data URL".into());
            }
        }
    }
    Ok(proto::UserMessage {
        text: text(message).trim().to_string(),
        message_id: id,
        selected_context: (!images.is_empty()).then(|| proto::SelectedContext {
            selected_images: images,
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Converts historical user content to Cursor root-prompt parts.
fn root_user_content(message: &ChatMessage) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    match &message.content {
        Some(MessageContent::Text(s)) if !s.trim().is_empty() => {
            result.push(json!({"type":"text", "text":s.trim()}));
        }
        Some(MessageContent::Parts(parts)) => {
            for part in parts {
                match part {
                    ContentPart::Text { text }
                    | ContentPart::InputText { text }
                    | ContentPart::OutputText { text }
                        if !text.trim().is_empty() =>
                    {
                        result.push(json!({"type":"text", "text":text.trim()}));
                    }
                    ContentPart::ImageUrl { image_url } => {
                        push_root_image(&mut result, &image_url.url)?;
                    }
                    ContentPart::InputImage {
                        image_url: Some(url),
                    } => push_root_image(&mut result, url)?,
                    ContentPart::Text { .. }
                    | ContentPart::InputText { .. }
                    | ContentPart::OutputText { .. } => {}
                    _ => return Err("Cursor does not support this input content type".into()),
                }
            }
        }
        _ => {}
    }
    Ok(result)
}

/// Validates and appends an inline image to a root-prompt record.
fn push_root_image(parts: &mut Vec<Value>, url: &str) -> Result<(), String> {
    let mime = url
        .strip_prefix("data:")
        .and_then(|s| s.split_once(';'))
        .map(|(mime, _)| mime)
        .filter(|mime| mime.starts_with("image/"))
        .ok_or_else(|| "Cursor images must be base64 data URLs".to_string())?;
    // Validate encoding just as for the action's SelectedContext.
    let data = url
        .split_once(',')
        .filter(|(header, _)| header.ends_with(";base64"))
        .map(|(_, data)| data)
        .ok_or_else(|| "Cursor images must be base64 data URLs".to_string())?;
    STANDARD
        .decode(data)
        .map_err(|e| format!("Invalid image data: {e}"))?;
    parts.push(json!({"type":"image", "image":url, "mediaType":mime}));
    Ok(())
}

/// Parses an assistant tool call's object arguments.
fn call_arguments(call: &ToolCall) -> Result<Value, String> {
    let args: Value = serde_json::from_str(&call.function.arguments).map_err(|e| {
        format!(
            "Invalid tool call arguments for {}: {e}",
            call.function.name
        )
    })?;
    if !args.is_object() {
        return Err(format!(
            "Tool call arguments for {} must be an object",
            call.function.name
        ));
    }
    Ok(args)
}

/// Builds a historical MCP tool call and its optional result.
fn call_step(
    call: &ToolCall,
    result: Option<&ChatMessage>,
) -> Result<proto::ConversationStep, String> {
    let args = call_arguments(call)?;
    let fields = args
        .as_object()
        .ok_or_else(|| "Tool arguments must be an object".to_string())?
        .iter()
        .map(|(key, value)| (key.clone(), protobuf_json(value).encode_to_vec()))
        .collect();
    let id = tool_id(&call.id);
    let result = result.map(|message| proto::McpToolResult {
        result: Some(proto::mcp_tool_result::Result::Success(proto::McpSuccess {
            content: vec![proto::McpToolResultContentItem {
                content: Some(proto::mcp_tool_result_content_item::Content::Text(
                    proto::McpTextContent {
                        text: text(message),
                        ..Default::default()
                    },
                )),
            }],
            ..Default::default()
        })),
    });
    let mcp = proto::McpToolCall {
        args: Some(proto::McpArgs {
            name: call.function.name.clone(),
            tool_name: call.function.name.clone(),
            tool_call_id: id.clone(),
            provider_identifier: "pi-agent".into(),
            args: fields,
            ..Default::default()
        }),
        result,
        ..Default::default()
    };
    Ok(proto::ConversationStep {
        message: Some(proto::conversation_step::Message::ToolCall(
            proto::ToolCall {
                tool: Some(proto::tool_call::Tool::McpToolCall(mcp)),
                tool_call_id: Some(id),
            },
        )),
    })
}

/// Rejects tool-choice modes the Cursor agent cannot enforce.
fn tools_enabled(choice: Option<&Value>) -> Result<bool, String> {
    match choice {
        None | Some(Value::Null) => Ok(true),
        Some(Value::String(value)) if value == "auto" => Ok(true),
        Some(Value::String(value)) if value == "none" => Ok(false),
        Some(_) => Err("Cursor does not support required or named tool_choice".into()),
    }
}

/// Translates advertised function tools into Cursor MCP definitions.
fn tool_definitions(req: &ChatCompletionRequest, enabled: bool) -> Vec<proto::McpToolDefinition> {
    req.tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|_| enabled)
        .map(|tool| {
            let schema = tool
                .function
                .parameters
                .clone()
                .unwrap_or_else(|| json!({"type":"object","properties":{},"required":[]}));
            proto::McpToolDefinition {
                name: tool.function.name.clone(),
                tool_name: tool.function.name.clone(),
                provider_identifier: "pi-agent".into(),
                description: tool.function.description.clone().unwrap_or_default(),
                input_schema: protobuf_json(&schema).encode_to_vec(),
                ..Default::default()
            }
        })
        .collect()
}

/// Builds the exact Cursor request and KV records from a consumer conversation.
pub(super) fn build(req: &ChatCompletionRequest) -> Result<Replay, String> {
    let tools_enabled = tools_enabled(req.tool_choice.as_ref())?;
    let mut blobs = HashMap::new();
    let systems: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(text)
        .filter(|s| !s.trim().is_empty())
        .collect();
    let prompts = if systems.is_empty() {
        vec!["You are a helpful assistant.".to_string()]
    } else {
        systems.clone()
    };
    let mut root = Vec::new();
    for system in &prompts {
        root.push(store_json(
            &mut blobs,
            &json!({"role":"system","content":system}),
        )?);
    }
    let rules = systems
        .iter()
        .enumerate()
        .map(|(i, content)| proto::CursorRule {
            full_path: format!("/omp/system-prompt/{i}.mdc"),
            content: content.clone(),
            source: 2,
            r#type: Some(proto::CursorRuleType {
                r#type: Some(proto::cursor_rule_type::Type::Global(
                    proto::CursorRuleTypeGlobal {},
                )),
            }),
            ..Default::default()
        })
        .collect();
    let active = req
        .messages
        .last()
        .filter(|m| m.role == "user" || m.role == "developer")
        .map(|_| req.messages.len().saturating_sub(1));
    let end = active.unwrap_or(req.messages.len());
    let history = req
        .messages
        .get(..end)
        .ok_or_else(|| "Invalid conversation history".to_string())?;
    if history.iter().any(|m| m.role != "system") && !systems.is_empty() {
        root.push(store_json(&mut blobs, &json!({"role":"user","content":format!("<rules>\n{}\n</rules>", systems.join("\n\n"))}))?);
    }
    let turns = history_records(history, &mut blobs, &mut root)?;
    let action = if let Some(index) = active {
        let message = req
            .messages
            .get(index)
            .ok_or_else(|| "Invalid active message".to_string())?;
        let user = user_message(message, uuid::Uuid::new_v4().to_string())?;
        if user.text.is_empty() && user.selected_context.is_none() {
            proto::conversation_action::Action::ResumeAction(proto::ResumeAction::default())
        } else {
            proto::conversation_action::Action::UserMessageAction(proto::UserMessageAction {
                user_message: Some(user),
                ..Default::default()
            })
        }
    } else {
        proto::conversation_action::Action::ResumeAction(proto::ResumeAction::default())
    };
    let tools = tool_definitions(req, tools_enabled);
    let request = proto::AgentRunRequest {
        conversation_state: Some(proto::ConversationStateStructure {
            root_prompt_messages_json: root,
            turns,
            ..Default::default()
        }),
        action: Some(proto::ConversationAction {
            action: Some(action),
        }),
        model_details: Some(proto::ModelDetails {
            model_id: req.model.clone(),
            display_model_id: req.model.clone(),
            display_name: req.model.clone(),
            ..Default::default()
        }),
        requested_model: Some(proto::RequestedModel {
            model_id: req.model.clone(),
            ..Default::default()
        }),
        conversation_id: Some(uuid::Uuid::new_v4().to_string()),
        ..Default::default()
    };
    Ok(Replay {
        request,
        blobs,
        tools,
        rules,
    })
}

/// Consumer-facing history replay tests.
#[cfg(test)]
mod tests;
