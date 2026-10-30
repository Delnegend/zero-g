use crate::openai::{ChatCompletionRequest, ContentPart, MessageContent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Claude Code version reflected in the OAuth wire fingerprint.
pub(super) const CLAUDE_VERSION: &str = "2.1.280";

/// Translate `OpenAI` conversation history to Anthropic messages and tools.
#[allow(clippy::too_many_lines)]
pub(super) fn build(req: &ChatCompletionRequest) -> Result<Value, String> {
    let first = req
        .messages
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref())
        .map_or_else(String::new, MessageContent::as_text);
    let units: Vec<u16> = first.encode_utf16().collect();
    let fingerprint: String = [4, 7, 20]
        .iter()
        .map(|i| {
            units
                .get(*i)
                .and_then(|u| char::from_u32(u32::from(*u)))
                .unwrap_or('0')
        })
        .collect();
    let hash = Sha256::digest(format!("59cf53e54c78{fingerprint}{CLAUDE_VERSION}").as_bytes());
    let hex = super::client::hex(&hash);
    let suffix: String = hex.chars().take(3).collect();
    let billing = format!(
        "x-anthropic-billing-header: cc_version={CLAUDE_VERSION}.{suffix}; cc_entrypoint=cli; cch=00000;"
    );
    let mut system = vec![
        json!({"type":"text","text":billing}),
        json!({"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude.","cache_control":{"type":"ephemeral"}}),
    ];
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        if message.role == "system" || message.role == "developer" {
            if let Some(content) = &message.content {
                system.push(json!({"type":"text","text":text_only(content)?}));
            }
            if message.provider_parts.is_some() || message.tool_calls.is_some() {
                return Err("Unsupported system message fields".into());
            }
            continue;
        }
        let mut blocks = Vec::new();
        let replay_complete = message.provider_parts.as_ref().is_some_and(|parts| {
            parts.iter().any(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "tool_use")
                )
            })
        });
        if let Some(parts) = &message.provider_parts {
            for part in parts {
                if message.role != "assistant"
                    || !matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking" | "text" | "tool_use")
                    )
                {
                    return Err("Unsupported Claude provider part".into());
                }
                blocks.push(part.clone());
            }
        }
        if message.role == "tool" {
            let id = message
                .tool_call_id
                .as_deref()
                .ok_or("Tool result missing tool_call_id")?;
            let content = message
                .content
                .as_ref()
                .map_or_else(|| Ok(String::new()), text_only)?;
            blocks.push(json!({"type":"tool_result","tool_use_id":id,"content":content}));
        } else if !replay_complete {
            if let Some(content) = &message.content {
                match content {
                    MessageContent::Text(text) => {
                        if !text.is_empty() {
                            blocks.push(json!({"type":"text","text":text}));
                        }
                    }
                    MessageContent::Parts(parts) => {
                        for part in parts {
                            match part {
                                ContentPart::Text { text }
                                | ContentPart::InputText { text }
                                | ContentPart::OutputText { text } => {
                                    blocks.push(json!({"type":"text","text":text}));
                                }
                                ContentPart::ImageUrl { image_url } => {
                                    blocks.push(image(&image_url.url)?);
                                }
                                ContentPart::InputImage {
                                    image_url: Some(url),
                                } => blocks.push(image(url)?),
                                _ => return Err("Unsupported Claude content part".into()),
                            }
                        }
                    }
                }
            }
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    let input: Value = serde_json::from_str(&call.function.arguments)
                        .map_err(|e| format!("Invalid tool arguments: {e}"))?;
                    blocks.push(json!({"type":"tool_use","id":call.id,"name":format!("_{}",call.function.name),"input":input}));
                }
            }
        }
        let role = match message.role.as_str() {
            "assistant" => "assistant",
            "user" | "tool" => "user",
            _ => return Err("Unsupported Claude message role".into()),
        };
        if blocks.is_empty() {
            continue;
        }
        if let Some(last) = messages
            .last_mut()
            .filter(|last| last.get("role").and_then(Value::as_str) == Some(role))
        {
            if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                content.extend(blocks);
            }
        } else {
            messages.push(json!({"role":role,"content":blocks}));
        }
    }
    if messages.is_empty() {
        return Err("Claude request needs a message".into());
    }
    let mut result = json!({"model":req.model,"max_tokens":req.max_completion_tokens.or(req.max_tokens).unwrap_or(8192),"system":system,"messages":messages,"stream":true});
    let object = result.as_object_mut().ok_or("Invalid request")?;
    if let Some(tools) = &req.tools {
        object.insert("tools".into(), Value::Array(tools.iter().map(|t| json!({"name":format!("_{}", t.function.name),"description":t.function.description,"input_schema":t.function.parameters.clone().unwrap_or_else(|| json!({"type":"object","properties":{}}))})).collect()));
    }
    if let Some(choice) = &req.tool_choice {
        if choice.as_str() == Some("none") {
            object.remove("tools");
        } else {
            let selected = match choice.as_str() {
                Some("auto") => json!({"type":"auto"}),
                Some("required") => json!({"type":"any"}),
                _ => {
                    let name = choice
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .or_else(|| choice.get("name").and_then(Value::as_str))
                        .ok_or("Invalid tool choice")?;
                    json!({"type":"tool","name":format!("_{name}")})
                }
            };
            object.insert("tool_choice".into(), selected);
        }
    }
    if let Some(temperature) = req.temperature {
        object.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        object.insert("top_p".into(), json!(top_p));
    }
    if let Some(effort) = req
        .reasoning_effort
        .as_deref()
        .or_else(|| {
            req.reasoning
                .as_ref()
                .and_then(|v| v.get("effort"))
                .and_then(Value::as_str)
        })
        .filter(|e| *e != "none")
    {
        let effort = match effort {
            "minimal" => "low",
            "low" | "medium" | "high" | "xhigh" | "max" | "adaptive" => effort,
            _ => return Err("Unsupported Claude reasoning effort".into()),
        };
        object.insert("thinking".into(), json!({"type":"adaptive"}));
        object.insert("output_config".into(), json!({"effort":effort}));
        object.remove("temperature");
        object.remove("top_p");
    }
    Ok(result)
}

/// Convert text-only system and tool content without dropping other parts.
fn text_only(content: &MessageContent) -> Result<String, String> {
    match content {
        MessageContent::Text(text) => Ok(text.clone()),
        MessageContent::Parts(parts) => {
            let mut result = String::new();
            for part in parts {
                match part {
                    ContentPart::Text { text }
                    | ContentPart::InputText { text }
                    | ContentPart::OutputText { text } => result.push_str(text),
                    _ => return Err("Unsupported non-text system or tool content".into()),
                }
            }
            Ok(result)
        }
    }
}

/// Convert a URL or base64 image into Anthropic's source format.
fn image(url: &str) -> Result<Value, String> {
    if let Some(data) = url.strip_prefix("data:") {
        let (meta, data) = data.split_once(',').ok_or("Invalid image data URL")?;
        let media = meta
            .strip_suffix(";base64")
            .ok_or("Image must be base64 encoded")?;
        Ok(json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}}))
    } else if url.starts_with("https://") || url.starts_with("http://") {
        Ok(json!({"type":"image","source":{"type":"url","url":url}}))
    } else {
        Err("Unsupported image URL".into())
    }
}
