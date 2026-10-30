use serde_json::{Value, json};

use crate::openai::{ChatCompletionRequest, ContentPart, MessageContent};
use crate::responses::{
    ResponseCreateRequest, ResponseInput, ResponseInputItem, ResponseTaggedItem,
};

/// Converts a chat completion request into the streaming Codex wire payload.
// These payloads are created as JSON objects; indexing inserts optional wire fields.
#[allow(clippy::indexing_slicing)]
pub(super) fn build_codex_payload_from_chat(req: &ChatCompletionRequest) -> Value {
    let mut input_items = Vec::new();
    let mut instructions = String::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" | "developer" => {
                append_instructions(&mut instructions, msg.content.as_ref());
            }
            "user" => {
                let parts = convert_content_to_input_parts(msg.content.as_ref(), "user");
                if !parts.is_empty() {
                    input_items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": parts,
                    }));
                }
            }
            "assistant" => {
                if let Some(c) = &msg.content {
                    let content = c.as_text();
                    input_items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": content,
                        }],
                    }));
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        input_items.push(json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }));
                    }
                }
            }
            "tool" => {
                let call_id = msg.tool_call_id.clone().unwrap_or_default();
                let output = msg
                    .content
                    .as_ref()
                    .map(MessageContent::as_text)
                    .unwrap_or_default();
                input_items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            _ => {}
        }
    }
    repair_call_ids(&mut input_items);

    let mut payload = json!({
        "model": req.model,
        "store": false,
        "stream": true,
        "input": input_items,
    });

    if !instructions.is_empty() {
        payload["instructions"] = json!(instructions);
    }

    if let Some(tools) = &req.tools {
        let wire_tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "parameters": t.function.parameters.clone().unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect();
        if !wire_tools.is_empty() {
            payload["tools"] = json!(wire_tools);
        }
    }

    if let Some(choice) = &req.tool_choice {
        payload["tool_choice"] = choice.clone();
    }

    payload["reasoning"] =
        extract_reasoning_payload(req.reasoning_effort.as_deref(), req.reasoning.as_ref());
    payload
}

/// Converts a Responses request into the streaming Codex wire payload.
// The payload is created as a JSON object; indexing inserts optional wire fields.
#[allow(clippy::indexing_slicing)]
#[allow(clippy::too_many_lines)]
pub(super) fn build_codex_payload_from_responses(req: &ResponseCreateRequest) -> Value {
    let mut input_items = Vec::new();
    let mut instructions = req.instructions.clone().unwrap_or_default();

    match &req.input {
        ResponseInput::Text(prompt) => {
            if !prompt.trim().is_empty() {
                input_items.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": prompt,
                    }],
                }));
            }
        }
        ResponseInput::Items(items) => {
            for item in items {
                match item {
                    ResponseInputItem::Tagged(tagged) => match tagged {
                        ResponseTaggedItem::Message {
                            role,
                            content,
                            name: _,
                            tool_calls,
                            provider_parts: _,
                        } => {
                            if is_system_role(role) {
                                append_instructions(&mut instructions, content.as_ref());
                            } else {
                                let parts = convert_content_to_input_parts(content.as_ref(), role);
                                input_items.push(json!({
                                    "type": "message",
                                    "role": role,
                                    "content": parts,
                                }));
                                if let Some(calls) = tool_calls {
                                    for call in calls {
                                        input_items.push(json!({"type":"function_call","call_id":call.id,"name":call.function.name,"arguments":call.function.arguments}));
                                    }
                                }
                            }
                        }
                        ResponseTaggedItem::FunctionCall {
                            call_id,
                            id,
                            name,
                            arguments,
                            thought_signature: _,
                        } => {
                            let cid = call_id.clone().or_else(|| id.clone()).unwrap_or_default();
                            let args = arguments
                                .as_str()
                                .map_or_else(|| arguments.to_string(), str::to_owned);
                            input_items.push(json!({
                                "type": "function_call",
                                "call_id": cid,
                                "name": name,
                                "arguments": args,
                            }));
                        }
                        ResponseTaggedItem::FunctionCallOutput { call_id, output } => {
                            let cid = call_id.clone().unwrap_or_default();
                            let out_str = output
                                .as_str()
                                .map_or_else(|| output.to_string(), str::to_owned);
                            input_items.push(json!({
                                "type": "function_call_output",
                                "call_id": cid,
                                "output": out_str,
                            }));
                        }
                        ResponseTaggedItem::Reasoning {
                            id,
                            summary,
                            encrypted_content,
                        } => {
                            let mut item = json!({"type":"reasoning","id":id,"summary":summary});
                            if let Some(content) = encrypted_content {
                                item["encrypted_content"] = json!(content);
                            }
                            input_items.push(item);
                        }
                    },
                    ResponseInputItem::Message(msg) => {
                        if is_system_role(&msg.role) {
                            append_instructions(&mut instructions, msg.content.as_ref());
                        } else if msg.role == "tool" {
                            input_items.push(json!({"type":"function_call_output","call_id":msg.tool_call_id.as_deref().unwrap_or_default(),"output":msg.content.as_ref().map(MessageContent::as_text).unwrap_or_default()}));
                        } else {
                            if msg.content.is_some() {
                                let parts =
                                    convert_content_to_input_parts(msg.content.as_ref(), &msg.role);
                                input_items.push(
                                    json!({"type":"message","role":msg.role,"content":parts}),
                                );
                            }
                            if let Some(calls) = &msg.tool_calls {
                                for call in calls {
                                    input_items.push(json!({"type":"function_call","call_id":call.id,"name":call.function.name,"arguments":call.function.arguments}));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    repair_call_ids(&mut input_items);

    let mut payload = json!({
        "model": req.model,
        "store": false,
        "stream": true,
        "input": input_items,
    });

    if !instructions.trim().is_empty() {
        payload["instructions"] = json!(instructions);
    }

    if let Some(tools) = &req.tools {
        let wire_tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "parameters": t.function.parameters.clone().unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect();
        if !wire_tools.is_empty() {
            payload["tools"] = json!(wire_tools);
        }
    }

    if let Some(choice) = &req.tool_choice {
        payload["tool_choice"] = choice.clone();
    }

    payload["reasoning"] =
        extract_reasoning_payload(req.reasoning_effort.as_deref(), req.reasoning.as_ref());
    payload
}

/// Repairs call IDs consistently and drops results with no matching call.
#[allow(clippy::indexing_slicing)]
fn repair_call_ids(items: &mut Vec<Value>) {
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut used = std::collections::HashSet::new();
    items.retain_mut(|item| {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let original = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if kind == "function_call" {
            let mut id: String = original
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .take(64)
                .collect();
            if id.is_empty() || used.contains(&id) {
                id = format!("call_{}", uuid::Uuid::new_v4().simple());
            }
            used.insert(id.clone());
            pending.push((original, id.clone()));
            item["call_id"] = json!(id);
        } else if kind == "function_call_output" {
            let index = pending
                .iter()
                .position(|(source, _)| source == &original)
                .or_else(|| (original.is_empty() && pending.len() == 1).then_some(0));
            let Some(index) = index else { return false };
            let (_, id) = pending.remove(index);
            item["call_id"] = json!(id);
        }
        true
    });
}
/// Returns whether a role contributes to the instruction string.
fn is_system_role(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

/// Appends nonblank message text to instructions, separated by a blank line.
fn append_instructions(instructions: &mut String, content: Option<&MessageContent>) {
    let Some(content) = content else { return };
    let text = content.as_text();
    if text.trim().is_empty() {
        return;
    }
    if !instructions.is_empty() {
        instructions.push_str("\n\n");
    }
    instructions.push_str(&text);
}

/// Builds reasoning settings, preferring the explicit effort and defaulting summary to auto.
// Reasoning is created as a JSON object; indexing inserts or replaces wire fields.
#[allow(clippy::indexing_slicing)]
fn extract_reasoning_payload(
    reasoning_effort: Option<&str>,
    reasoning_val: Option<&Value>,
) -> Value {
    let mut reasoning = json!({ "summary": "auto" });

    let effort = reasoning_effort.or_else(|| {
        reasoning_val
            .and_then(|v| v.get("effort"))
            .and_then(Value::as_str)
    });

    if let Some(eff) = effort {
        reasoning["effort"] = json!(eff);
    }

    if let Some(summary) = reasoning_val
        .and_then(|v| v.get("summary"))
        .and_then(Value::as_str)
    {
        reasoning["summary"] = json!(summary);
    }

    reasoning
}

/// Converts message content to role-appropriate Codex text and image parts.
fn convert_content_to_input_parts(content: Option<&MessageContent>, role: &str) -> Vec<Value> {
    let mut parts = Vec::new();
    let Some(c) = content else { return parts };

    // Upstream only accepts `output_text`/`refusal` parts in assistant turns and
    // `input_text` elsewhere, so the part type follows the message role.
    let is_assistant = role == "assistant";
    let text_type = if is_assistant {
        "output_text"
    } else {
        "input_text"
    };

    match c {
        MessageContent::Text(text) => {
            if !text.is_empty() {
                parts.push(json!({
                    "type": text_type,
                    "text": text,
                }));
            }
        }
        MessageContent::Parts(raw_parts) => {
            for part in raw_parts {
                match part {
                    ContentPart::Text { text }
                    | ContentPart::InputText { text }
                    | ContentPart::OutputText { text } => {
                        if !text.is_empty() {
                            parts.push(json!({
                                "type": text_type,
                                "text": text,
                            }));
                        }
                    }
                    ContentPart::ImageUrl { image_url } => {
                        if !is_assistant {
                            parts.push(json!({
                                "type": "input_image",
                                "image_url": &image_url.url,
                            }));
                        }
                    }
                    ContentPart::InputImage { image_url } => {
                        if let (false, Some(url)) = (is_assistant, image_url) {
                            parts.push(json!({
                                "type": "input_image",
                                "image_url": url,
                            }));
                        }
                    }
                    ContentPart::InputAudio { .. }
                    | ContentPart::File { .. }
                    | ContentPart::InputFile { .. }
                    | ContentPart::VideoUrl { .. }
                    | ContentPart::InputVideo { .. } => {
                        // Preserve unfamiliar media for the upstream provider to accept or reject.
                        if let Ok(value) = serde_json::to_value(part) {
                            parts.push(value);
                        }
                    }
                }
            }
        }
    }
    parts
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::openai::ChatMessage;

    #[test]
    fn test_build_codex_payload_from_chat() {
        let req = ChatCompletionRequest {
            model: "gpt-5.5".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text("Be concise.".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    provider_parts: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello world".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    provider_parts: None,
                },
            ],
            temperature: Some(0.7), // must be ignored/omitted in payload
            top_p: Some(0.9),       // must be ignored/omitted in payload
            max_tokens: Some(100),  // must be ignored/omitted in payload
            max_completion_tokens: None,
            stream: Some(true),
            stream_options: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            reasoning: None,
        };

        let payload = build_codex_payload_from_chat(&req);
        assert_eq!(payload["model"], "gpt-5.5");
        assert_eq!(payload["store"], false);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["instructions"], "Be concise.");
        assert_eq!(payload["input"].as_array().unwrap().len(), 1);
        assert_eq!(payload["input"][0]["content"][0]["text"], "Hello world");

        // Disallowed sampling parameters must be absent
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());
        assert!(payload.get("max_tokens").is_none());
        assert!(payload.get("max_output_tokens").is_none());
    }

    #[test]
    fn test_build_codex_payload_keeps_tool_call_arguments_as_string() {
        let req = ChatCompletionRequest {
            model: "gpt-6.1-terra".to_string(),
            messages: vec![ChatMessage {
                role: "assistant".to_string(),
                content: None,
                name: None,
                tool_calls: Some(vec![crate::openai::ToolCall {
                    id: "call_1".to_string(),
                    r#type: "function".to_string(),
                    thought_signature: None,
                    function: crate::openai::FunctionCall {
                        name: "read".to_string(),
                        arguments: r#"{"path":"backend/main.rs"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                provider_parts: None,
            }],
            temperature: None,
            top_p: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            reasoning: None,
        };

        let payload = build_codex_payload_from_chat(&req);
        assert_eq!(payload["input"][0]["type"], "function_call");
        assert_eq!(
            payload["input"][0]["arguments"].as_str(),
            Some(r#"{"path":"backend/main.rs"}"#)
        );
    }

    #[test]
    fn test_build_codex_payload_hoists_system_and_developer_messages() {
        let req = ChatCompletionRequest {
            model: "gpt-5.6-sol".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text("Be concise.".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    provider_parts: None,
                },
                ChatMessage {
                    role: "developer".to_string(),
                    content: Some(MessageContent::Text("Use Rust.".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    provider_parts: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    provider_parts: None,
                },
            ],
            temperature: None,
            top_p: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            reasoning: None,
        };

        let payload = build_codex_payload_from_chat(&req);
        assert_eq!(payload["instructions"], "Be concise.\n\nUse Rust.");
        assert_eq!(payload["input"].as_array().unwrap().len(), 1);
        assert_eq!(payload["input"][0]["role"], "user");
    }

    #[test]
    fn test_build_codex_payload_from_responses_hoists_system_input() {
        let json_val = serde_json::json!({
            "model": "gpt-5.6-sol",
            "instructions": "Base instructions.",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{"type": "input_text", "text": "Extra system."}]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "Hi"}]
                }
            ]
        });
        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();

        let payload = build_codex_payload_from_responses(&req);
        assert_eq!(
            payload["instructions"],
            "Base instructions.\n\nExtra system."
        );
        assert_eq!(payload["input"].as_array().unwrap().len(), 1);
        assert_eq!(payload["input"][0]["role"], "user");
    }

    #[test]
    fn test_build_codex_payload_from_responses() {
        let req = ResponseCreateRequest {
            model: "gpt-5.6-luna".to_string(),
            input: ResponseInput::Text("Ping".to_string()),
            instructions: Some("You are helpful.".to_string()),
            stream: Some(true),
            tools: None,
            tool_choice: None,
            temperature: Some(0.2),
            top_p: None,
            max_output_tokens: Some(500),
            reasoning_effort: None,
            reasoning: None,
        };

        let payload = build_codex_payload_from_responses(&req);
        assert_eq!(payload["model"], "gpt-5.6-luna");
        assert_eq!(payload["instructions"], "You are helpful.");
        assert_eq!(payload["input"][0]["content"][0]["text"], "Ping");
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("max_output_tokens").is_none());
    }

    #[test]
    fn test_responses_function_call_arguments_are_strings() {
        let req: ResponseCreateRequest = serde_json::from_value(json!({
            "model": "gpt-6.1-terra",
            "input": [
                {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": {"path": "Cargo.toml"}},
                {"type": "function_call", "call_id": "call_2", "name": "read", "arguments": "{\"path\":\"backend/main.rs\"}"}
            ]
        })).unwrap();
        let payload = build_codex_payload_from_responses(&req);
        assert_eq!(payload["input"][0]["arguments"], r#"{"path":"Cargo.toml"}"#);
        assert_eq!(
            payload["input"][1]["arguments"],
            r#"{"path":"backend/main.rs"}"#
        );
    }

    #[test]
    fn test_build_codex_payload_from_responses_flat_tools() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "input": "Ping",
            "tools": [
                {
                    "type": "function",
                    "name": "ping",
                    "description": "Ping the host",
                    "parameters": {"type": "object", "properties": {}}
                }
            ]
        });
        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();

        let payload = build_codex_payload_from_responses(&req);
        let tools = payload["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "ping");
        assert_eq!(tools[0]["description"], "Ping the host");
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn test_build_codex_payload_from_responses_assistant_parts_are_output_text() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "say OK"}]}
            ]
        });
        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();

        let payload = build_codex_payload_from_responses(&req);
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["content"][0]["text"], "hello");
        assert_eq!(input[2]["content"][0]["type"], "input_text");
    }

    #[test]
    fn test_build_codex_payload_from_responses_chat_shaped_assistant() {
        let json_val = json!({
            "model": "gpt-5.6-sol",
            "input": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "say OK"}
            ]
        });
        let req: ResponseCreateRequest = serde_json::from_value(json_val).unwrap();

        let payload = build_codex_payload_from_responses(&req);
        let input = payload["input"].as_array().unwrap();
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["content"][0]["text"], "hello");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }
}
