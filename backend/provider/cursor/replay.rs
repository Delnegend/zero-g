use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use base64::{Engine, engine::general_purpose::STANDARD};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::proto;
use crate::openai::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent, ToolCall};

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

/// Stores a completed conversation turn and returns its blob ID.
fn store_turn(
    blobs: &mut HashMap<Vec<u8>, Vec<u8>>,
    user: Vec<u8>,
    steps: Vec<Vec<u8>>,
) -> Vec<u8> {
    let turn = proto::ConversationTurnStructure {
        turn: Some(
            proto::conversation_turn_structure::Turn::AgentConversationTurn(
                proto::AgentConversationTurnStructure {
                    user_message: user,
                    steps,
                    ..Default::default()
                },
            ),
        ),
    };
    store(blobs, turn.encode_to_vec())
}

/// Indexes tool results and call names for pairing historical messages.
fn history_indexes(
    history: &[ChatMessage],
) -> (HashMap<String, &ChatMessage>, HashMap<String, String>) {
    let mut results = HashMap::new();
    let mut names = HashMap::new();
    for message in history {
        if message.role == "tool"
            && let Some(id) = &message.tool_call_id
        {
            results.insert(id.clone(), message);
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                names.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }
    (results, names)
}

/// Rebuilds conversation turns and their root-prompt history.
fn history_records(
    history: &[ChatMessage],
    blobs: &mut HashMap<Vec<u8>, Vec<u8>>,
    root: &mut Vec<Vec<u8>>,
) -> Result<Vec<Vec<u8>>, String> {
    let (results, call_names) = history_indexes(history);
    let mut turns = Vec::new();
    let mut current_user: Option<Vec<u8>> = None;
    let mut steps = Vec::new();
    let mut emitted_results = HashSet::new();
    for message in history {
        match message.role.as_str() {
            "system" => {}
            "user" | "developer" => {
                if let Some(user) = current_user.take() {
                    turns.push(store_turn(blobs, user, std::mem::take(&mut steps)));
                }
                let content = root_user_content(message)?;
                let user = user_message(
                    message,
                    deterministic_id(&format!("u:{}:{}", turns.len(), text(message).trim())),
                )?;
                if !content.is_empty() {
                    root.push(store_json(
                        blobs,
                        &json!({"role":"user","content":content}),
                    )?);
                    current_user = Some(store(blobs, user.encode_to_vec()));
                }
            }
            "assistant" => {
                let mut seen_calls = HashSet::new();
                let mut parts = Vec::new();
                let value = text(message);
                if !value.is_empty() {
                    parts.push(json!({"type":"text","text":value}));
                    let step = proto::ConversationStep {
                        message: Some(proto::conversation_step::Message::AssistantMessage(
                            proto::AssistantMessage { text: value },
                        )),
                    };
                    steps.push(store(blobs, step.encode_to_vec()));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        if !seen_calls.insert(call.id.clone()) {
                            continue;
                        }
                        let args = call_arguments(call)?;
                        parts.push(json!({"type":"tool-call","toolCallId":tool_id(&call.id),"toolName":call.function.name,"args":args}));
                        let step = call_step(call, results.get(&call.id).copied())?;
                        steps.push(store(blobs, step.encode_to_vec()));
                    }
                }
                if !parts.is_empty() {
                    root.push(store_json(
                        blobs,
                        &json!({"role":"assistant","content":parts}),
                    )?);
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .ok_or_else(|| "Tool result is missing tool_call_id".to_string())?;
                if !emitted_results.insert(id.to_string()) {
                    continue;
                }
                let message = results.get(id).copied().unwrap_or(message);
                if let Some(name) = call_names.get(id) {
                    root.push(store_json(blobs, &json!({"role":"tool","id":tool_id(id),"content":[{"type":"tool-result","toolName":name,"toolCallId":tool_id(id),"result":text(message)}]}))?);
                } else {
                    let result = format!("[Tool Result]\n{}", text(message));
                    root.push(store_json(
                        blobs,
                        &json!({"role":"assistant","content":[{"type":"text","text":result}]}),
                    )?);
                    let step = proto::ConversationStep {
                        message: Some(proto::conversation_step::Message::AssistantMessage(
                            proto::AssistantMessage { text: result },
                        )),
                    };
                    steps.push(store(blobs, step.encode_to_vec()));
                }
            }
            role => return Err(format!("Unsupported conversation role: {role}")),
        }
    }
    if let Some(user) = current_user {
        turns.push(store_turn(blobs, user, steps));
    }
    Ok(turns)
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
mod tests {
    use super::*;

    /// Parses a chat request fixture.
    fn request(value: Value) -> ChatCompletionRequest {
        serde_json::from_value(value).unwrap_or_else(|e| panic!("invalid fixture: {e}"))
    }

    /// Replays a paired tool result while excluding the active user message.
    #[test]
    fn history_replays_tool_results_and_excludes_active_user() {
        let req = request(json!({"model":"model", "messages":[
            {"role":"system","content":"Be concise"}, {"role":"user","content":"old"},
            {"role":"assistant","tool_calls":[{"id":"call|1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Oslo\"}"}}]},
            {"role":"tool","tool_call_id":"call|1","content":"rain"}, {"role":"user","content":"new"}
        ], "tools":[{"type":"function","function":{"name":"weather","parameters":{"type":"object"}}}]}));
        let replay = build(&req).unwrap_or_else(|e| panic!("replay failed: {e}"));
        let state = replay
            .request
            .conversation_state
            .as_ref()
            .unwrap_or_else(|| panic!("missing state"));
        let records: Vec<Value> = state
            .root_prompt_messages_json
            .iter()
            .map(|id| {
                serde_json::from_slice(
                    replay
                        .blobs
                        .get(id)
                        .unwrap_or_else(|| panic!("missing blob")),
                )
                .unwrap_or_else(|e| panic!("bad JSON: {e}"))
            })
            .collect();
        assert!(records.iter().any(|v| v["role"] == "tool"
            && v["id"] == "call_1"
            && v["content"][0]["result"] == "rain"));
        assert!(!records.iter().any(|v| v.to_string().contains("new")));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(replay.tools.len(), 1);
        assert_eq!(replay.rules.len(), 1);
        let again = build(&req).unwrap_or_else(|e| panic!("replay failed: {e}"));
        assert_eq!(
            state.turns,
            again.request.conversation_state.unwrap_or_default().turns
        );
    }

    /// Rejects malformed calls and unsupported remote image URLs.
    #[test]
    fn rejects_malformed_tool_arguments_and_unsupported_remote_image() {
        let bad_call = request(
            json!({"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"a","type":"function","function":{"name":"f","arguments":"{"}}]}]}),
        );
        assert!(build(&bad_call).is_err());
        let bad_image = request(
            json!({"model":"m","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png"}}]}]}),
        );
        assert!(build(&bad_image).is_err());
    }
    /// Both API request shapes enforce supported tool-selection modes.
    #[test]
    fn tool_choice_is_enforced_for_chat_and_responses() {
        for choice in [
            json!("none"),
            json!("auto"),
            json!("required"),
            json!({"type":"function","function":{"name":"weather"}}),
        ] {
            let chat = request(
                json!({"model":"m","messages":[{"role":"user","content":"hello"}],
                "tools":[{"type":"function","function":{"name":"weather"}}], "tool_choice":choice}),
            );
            let response: crate::responses::ResponseCreateRequest = serde_json::from_value(json!({
                "model":"m","input":"hello","tools":[{"type":"function","name":"weather"}],"tool_choice":choice
            })).unwrap_or_else(|error| panic!("invalid fixture: {error}"));
            for req in [chat, response.into_chat_request()] {
                let built = build(&req);
                match choice.as_str() {
                    Some("none") => assert_eq!(
                        built.unwrap_or_else(|error| panic!("{error}")).tools.len(),
                        0
                    ),
                    Some("auto") => assert_eq!(
                        built.unwrap_or_else(|error| panic!("{error}")).tools.len(),
                        1
                    ),
                    _ => assert!(built.is_err()),
                }
            }
        }
    }
    /// Builds an authenticated Cursor router with no upstream credential.
    async fn tool_choice_router() -> axum::Router {
        use crate::storage::{ProviderRecord, VirtualKey};
        use axum::Router;

        let database = crate::database::Database::open(":memory:")
            .await
            .unwrap_or_else(|error| panic!("database: {error}"));
        let store = crate::server::settings::SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap_or_else(|error| panic!("settings: {error}"));
        let mut db = (*database.db).clone();
        toasty::create!(ProviderRecord {
            id: "cursor".to_string(),
            name: "Cursor".to_string(),
            kind: "cursor".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap_or_else(|error| panic!("provider: {error}"));
        let key = "test-cursor-key";
        let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "test".to_string(),
            key_hash: digest.as_ref().to_vec(),
            providers: "[]".to_string(),
            models: "[]".to_string(),
            created_at: 0_i64
        })
        .exec(&mut db)
        .await
        .unwrap_or_else(|error| panic!("key: {error}"));
        let state = crate::server::AppState {
            providers: crate::provider::Providers::new(),
            settings: store.clone(),
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        Router::new()
            .nest("/cursor/{id}", super::super::routes::router(&store))
            .with_state(state)
    }

    /// The actual authenticated routes reject unsupported selections before credential lookup.
    #[tokio::test]
    async fn tool_choice_http_routes_validate_before_credential_lookup() {
        use axum::{
            body::{Body, to_bytes},
            http::{Request, StatusCode, header},
        };
        use tower::ServiceExt;

        let app = tool_choice_router().await;
        let key = "test-cursor-key";
        for (path, input) in [
            (
                "/cursor/cursor/v1/chat/completions",
                json!({"model":"m","messages":[{"role":"user","content":"hello"}]}),
            ),
            (
                "/cursor/cursor/v1/responses",
                json!({"model":"m","input":"hello"}),
            ),
        ] {
            for (choice, expected) in [
                (json!("none"), StatusCode::BAD_GATEWAY),
                (json!("required"), StatusCode::BAD_REQUEST),
                (
                    json!({"type":"function","function":{"name":"weather"}}),
                    StatusCode::BAD_REQUEST,
                ),
            ] {
                let mut body = input.clone();
                let Some(object) = body.as_object_mut() else {
                    panic!("invalid fixture")
                };
                object.insert("tool_choice".into(), choice);
                object.insert(
                    "tools".into(),
                    json!([{"type":"function","function":{"name":"weather"}}]),
                );
                let request = Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {key}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap_or_else(|error| panic!("request: {error}"));
                let response = app
                    .clone()
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|error| panic!("router: {error}"));
                assert_eq!(response.status(), expected);
                let bytes = to_bytes(response.into_body(), 1_000_000)
                    .await
                    .unwrap_or_else(|error| panic!("body: {error}"));
                let body: Value =
                    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("JSON: {error}"));
                let message = body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if expected == StatusCode::BAD_REQUEST {
                    assert!(message.contains("tool_choice"));
                } else {
                    assert!(message.contains("not configured"));
                }
            }
        }
    }
}
