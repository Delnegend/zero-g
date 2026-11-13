//! Conversion of chat messages and tool calls to Gemini content parts.

use super::media::inline_media;
use crate::openai::{ChatMessage, ContentPart, MessageContent};
use crate::provider::antigravity::AntigravityClient;
use serde_json::{Value, json};
use std::collections::HashMap;

impl AntigravityClient {
    /// Separates system prompts from converted conversation contents.
    pub(super) fn collect_messages(
        messages: &[ChatMessage],
        wire_model: &str,
    ) -> (Vec<String>, Vec<Value>) {
        let mut system_prompts = Vec::new();
        let mut contents = Vec::new();
        let mut tool_call_names: HashMap<String, String> = HashMap::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" | "developer" => {
                    Self::handle_system_message(msg, &mut system_prompts);
                }
                "user" => {
                    Self::handle_user_message(msg, &mut contents);
                }
                "assistant" => {
                    Self::handle_assistant_message(
                        msg,
                        &mut contents,
                        &mut tool_call_names,
                        wire_model,
                    );
                }
                "tool" => {
                    Self::handle_tool_message(msg, &mut contents, &tool_call_names);
                }
                _ => {}
            }
        }

        // Some upstream models pair parallel function responses by position, even
        // when IDs are present. Restore call order without moving unknown parts.
        let mut ordinals = HashMap::new();
        let mut next = 0usize;
        for content in &contents {
            if content.get("role").and_then(Value::as_str) != Some("model") {
                continue;
            }
            if let Some(parts) = content.get("parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(id) = part.pointer("/functionCall/id").and_then(Value::as_str) {
                        ordinals.insert(id.to_owned(), next);
                        next = next.saturating_add(1);
                    }
                }
            }
        }
        for content in &mut contents {
            if content.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut known = Vec::new();
            let mut positions = Vec::new();
            for (position, part) in parts.iter_mut().enumerate() {
                if let Some(ordinal) = part
                    .pointer("/functionResponse/id")
                    .and_then(Value::as_str)
                    .and_then(|id| ordinals.get(id))
                {
                    positions.push(position);
                    known.push((*ordinal, std::mem::take(part)));
                }
            }
            known.sort_by_key(|(ordinal, _)| *ordinal);
            for (position, (_, part)) in positions.into_iter().zip(known) {
                if let Some(slot) = parts.get_mut(position) {
                    *slot = part;
                }
            }
        }

        (system_prompts, contents)
    }

    /// Collects nonempty system or developer message text.
    fn handle_system_message(msg: &ChatMessage, system_prompts: &mut Vec<String>) {
        if let Some(content) = &msg.content {
            let text = content.as_text();
            if !text.trim().is_empty() {
                system_prompts.push(text);
            }
        }
    }

    /// Appends a user message when it contains supported content.
    fn handle_user_message(msg: &ChatMessage, contents: &mut Vec<Value>) {
        let parts = Self::user_content_parts(msg.content.as_ref());
        if !parts.is_empty() {
            contents.push(json!({
                "role": "user",
                "parts": parts
            }));
        }
    }

    /// Converts user text and inline media into upstream content parts.
    fn user_content_parts(content: Option<&MessageContent>) -> Vec<Value> {
        let mut parts = Vec::new();
        if let Some(content) = content {
            match content {
                MessageContent::Text(text) => {
                    if !text.trim().is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                }
                MessageContent::Parts(subparts) => {
                    for part in subparts {
                        match part {
                            ContentPart::Text { text }
                            | ContentPart::InputText { text }
                            | ContentPart::OutputText { text } => {
                                if !text.trim().is_empty() {
                                    parts.push(json!({ "text": text }));
                                }
                            }
                            _ => {
                                if let Ok(Some((mime, data))) = inline_media(part) {
                                    parts.push(
                                        json!({ "inlineData": { "mimeType": mime, "data": data } }),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        parts
    }

    /// Appends an assistant message and records its tool call names.
    fn handle_assistant_message(
        msg: &ChatMessage,
        contents: &mut Vec<Value>,
        tool_call_names: &mut HashMap<String, String>,
        wire_model: &str,
    ) {
        let parts = Self::assistant_parts(msg, tool_call_names, wire_model);
        if !parts.is_empty() {
            contents.push(json!({
                "role": "model",
                "parts": parts
            }));
        }
    }

    /// Converts assistant text and tool calls into upstream parts.
    fn assistant_parts(
        msg: &ChatMessage,
        tool_call_names: &mut HashMap<String, String>,
        wire_model: &str,
    ) -> Vec<Value> {
        let mut parts = Vec::new();
        if let Some(provider_parts) = &msg.provider_parts {
            for item in provider_parts {
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let reasoning = match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => true,
                    Some("text") => false,
                    _ => continue,
                };
                let signature = item
                    .get("thought_signature")
                    .and_then(Value::as_str)
                    .and_then(|signature| {
                        crate::provider::antigravity::replay_signature(wire_model, signature)
                    });
                let mut part = json!({ "text": text });
                if let Some(object) = part.as_object_mut() {
                    if reasoning {
                        object.insert("thought".to_string(), json!(true));
                    }
                    if let Some(signature) = signature {
                        object.insert("thoughtSignature".to_string(), json!(signature));
                    }
                }
                parts.push(part);
            }
        }
        if parts.is_empty()
            && let Some(content) = &msg.content
        {
            let text = content.as_text();
            if !text.trim().is_empty() {
                parts.push(json!({ "text": text }));
            }
        }
        if let Some(tool_calls) = &msg.tool_calls {
            for (index, tool_call) in tool_calls.iter().enumerate() {
                tool_call_names.insert(tool_call.id.clone(), tool_call.function.name.clone());
                let args: Value = serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({}));
                let mut part = json!({
                    "functionCall": {
                        "name": tool_call.function.name,
                        "args": args,
                        "id": tool_call.id
                    }
                });
                let signature = tool_call
                    .thought_signature
                    .as_deref()
                    .and_then(|signature| {
                        crate::provider::antigravity::replay_signature(wire_model, signature)
                    })
                    .or_else(|| {
                        (index == 0).then(|| "skip_thought_signature_validator".to_string())
                    });
                if let (Some(object), Some(signature)) = (part.as_object_mut(), signature) {
                    object.insert("thoughtSignature".to_string(), json!(signature));
                }
                parts.push(part);
            }
        }
        parts
    }

    /// Converts a tool result and merges it with adjacent tool responses.
    // The function response is constructed as a JSON object, and indexing inserts its optional id.
    #[allow(clippy::indexing_slicing)]
    fn handle_tool_message(
        msg: &ChatMessage,
        contents: &mut Vec<Value>,
        tool_call_names: &HashMap<String, String>,
    ) {
        let tool_name = msg
            .name
            .clone()
            .or_else(|| {
                msg.tool_call_id
                    .as_ref()
                    .and_then(|id| tool_call_names.get(id).cloned())
            })
            .unwrap_or_else(|| "tool".to_string());
        let result_str = msg.content.as_ref().map_or_default(MessageContent::as_text);
        let response_obj: Value = match serde_json::from_str::<Value>(&result_str) {
            Ok(value @ Value::Object(_)) => value,
            Ok(value) => json!({ "output": value }),
            Err(_) => json!({ "output": result_str }),
        };

        let mut func_resp = json!({
            "name": tool_name,
            "response": response_obj,
        });
        if let Some(id) = &msg.tool_call_id {
            func_resp["id"] = json!(id);
        }

        let part = json!({
            "functionResponse": func_resp
        });

        if !Self::try_merge_tool_response(contents, &part) {
            contents.push(json!({
                "role": "user",
                "parts": [part]
            }));
        }
    }

    /// Adds a tool response to the preceding tool-response message if possible.
    fn try_merge_tool_response(contents: &mut [Value], part: &Value) -> bool {
        if let Some(last) = contents.last_mut()
            && last.get("role").and_then(|r| r.as_str()) == Some("user")
            && last
                .get("parts")
                .and_then(|p| p.as_array())
                .is_some_and(|parts| parts.iter().any(|p| p.get("functionResponse").is_some()))
            && let Some(parts) = last.get_mut("parts").and_then(|p| p.as_array_mut())
        {
            parts.push(part.clone());
            true
        } else {
            false
        }
    }
}
