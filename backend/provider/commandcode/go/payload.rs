//! `OpenAI` chat requests rendered in the Command Code CLI `/alpha/generate` shape.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::openai::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent};

/// Output token limit the CLI sends when the caller sets none.
const DEFAULT_MAX_TOKENS: u32 = 64_000;
/// Longest tool-call identifier the generate endpoint accepts.
const TOOL_CALL_ID_LIMIT: usize = 64;

/// Build the `/alpha/generate` body for one turn.
pub(super) fn build(req: &ChatCompletionRequest, thread_id: &str) -> Result<Value, String> {
    if let Some(choice) = &req.tool_choice
        && choice.as_str() != Some("auto")
    {
        return Err("tool_choice is not supported by Command Code Go".into());
    }
    let (system, messages) = messages(&req.messages)?;
    let tools: Vec<Value> = req
        .tools
        .iter()
        .flatten()
        .map(|tool| {
            json!({
                "name": tool.function.name,
                "description": tool.function.description.as_deref().unwrap_or_default(),
                "input_schema": tool.function.parameters.clone()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect();
    let mut params = json!({
        "model": req.model,
        "messages": messages,
        "tools": tools,
        "system": system,
        "max_tokens": req.max_completion_tokens.or(req.max_tokens).unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": true,
    });
    if let Some(object) = params.as_object_mut() {
        if let Some(temperature) = req.temperature {
            object.insert("temperature".into(), json!(temperature));
        }
        let effort = req.reasoning_effort.as_deref().or_else(|| {
            req.reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        });
        if let Some(effort) = effort {
            object.insert("reasoning_effort".into(), json!(effort));
        }
    }
    Ok(json!({
        "config": {
            "workingDir": "/",
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": "linux",
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "memory": null,
        "taste": null,
        "skills": null,
        "permissionMode": "standard",
        "threadId": thread_id,
        "params": params,
    }))
}

/// Shorten identifiers the generate endpoint would reject, keeping calls and results paired.
fn wire_id(id: &str) -> String {
    if id.len() <= TOOL_CALL_ID_LIMIT {
        return id.to_owned();
    }
    let digest = Sha256::digest(id.as_bytes());
    let hex: String = digest
        .iter()
        .take(16)
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect();
    format!("call_{hex}")
}

/// Convert one user content value to generate parts.
fn user_content(content: Option<&MessageContent>) -> Result<Value, String> {
    let Some(MessageContent::Parts(parts)) = content else {
        return Ok(json!(
            content.map(MessageContent::as_text).unwrap_or_default()
        ));
    };
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text }
            | ContentPart::InputText { text }
            | ContentPart::OutputText { text } => Ok(json!({"type": "text", "text": text})),
            ContentPart::ImageUrl { image_url } => Ok(image(&image_url.url)),
            ContentPart::InputImage {
                image_url: Some(url),
            } => Ok(image(url)),
            _ => Err("Command Code Go accepts only text and image content".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// Render an image URL, recording the media type of data URLs.
fn image(url: &str) -> Value {
    let media = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(';'))
        .map(|(media, _)| media);
    media.map_or_else(
        || json!({"type": "image", "image": url}),
        |media| json!({"type": "image", "image": url, "mimeType": media}),
    )
}

/// Split leading instructions into the system prompt and convert the conversation.
fn messages(input: &[ChatMessage]) -> Result<(String, Vec<Value>), String> {
    let answered: HashSet<&str> = input
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut system = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    let flush = |out: &mut Vec<Value>, results: &mut Vec<Value>| {
        if !results.is_empty() {
            out.push(json!({"role": "tool", "content": std::mem::take(results)}));
        }
    };
    for message in input {
        let text = || {
            message
                .content
                .as_ref()
                .map(MessageContent::as_text)
                .unwrap_or_default()
        };
        match message.role.as_str() {
            "system" | "developer" if out.is_empty() && results.is_empty() => system.push(text()),
            "system" | "developer" | "user" => {
                flush(&mut out, &mut results);
                let content = if message.role == "user" {
                    user_content(message.content.as_ref())?
                } else {
                    json!(text())
                };
                out.push(json!({"role": "user", "content": content}));
            }
            "assistant" => {
                flush(&mut out, &mut results);
                let mut parts = Vec::new();
                let content = text();
                if !content.is_empty() {
                    parts.push(json!({"type": "text", "text": content}));
                }
                let mut missing = Vec::new();
                for call in message.tool_calls.iter().flatten() {
                    names.insert(&call.id, &call.function.name);
                    let input = serde_json::from_str::<Value>(&call.function.arguments)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    let id = wire_id(&call.id);
                    parts.push(json!({"type": "tool-call", "toolCallId": id, "toolName": call.function.name, "input": input}));
                    if !answered.contains(call.id.as_str()) {
                        missing.push(json!({"type": "tool-result", "toolCallId": id, "toolName": call.function.name, "output": {"type": "error-text", "value": "No result: the tool call did not complete."}}));
                    }
                }
                if !parts.is_empty() {
                    out.push(json!({"role": "assistant", "content": parts}));
                }
                if !missing.is_empty() {
                    out.push(json!({"role": "tool", "content": missing}));
                }
            }
            "tool" => {
                let id = message.tool_call_id.as_deref().unwrap_or_default();
                results.push(json!({
                    "type": "tool-result",
                    "toolCallId": wire_id(id),
                    "toolName": names.get(id).copied().unwrap_or("unknown"),
                    "output": {"type": "text", "value": text()},
                }));
            }
            role => return Err(format!("Unsupported message role: {role}")),
        }
    }
    flush(&mut out, &mut results);
    Ok((system.join("\n\n"), out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn request(value: Value) -> ChatCompletionRequest {
        serde_json::from_value(value).unwrap()
    }

    /// A tool round trip keeps call/result pairing, names, and the system prompt.
    #[test]
    fn converts_tool_history_and_system_prompt() {
        let long = "x".repeat(80);
        let body = build(
            &request(json!({
                "model": "deepseek/deepseek-v4-flash",
                "max_tokens": 100,
                "reasoning_effort": "high",
                "messages": [
                    {"role": "system", "content": "Be brief."},
                    {"role": "user", "content": [
                        {"type": "text", "text": "Look"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                    ]},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": long, "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}},
                        {"id": "call_2", "type": "function", "function": {"name": "list", "arguments": "{}"}}
                    ]},
                    {"role": "tool", "tool_call_id": long, "content": "file body"}
                ],
                "tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object"}}}]
            })),
            "6f1c6a8e-4b1a-4f5e-9d2b-0c6f3b1a2d4e",
        )
        .unwrap();
        let params = &body["params"];
        assert_eq!(params["system"], "Be brief.");
        assert_eq!(params["max_tokens"], 100);
        assert_eq!(params["reasoning_effort"], "high");
        assert_eq!(
            params["tools"][0]["input_schema"],
            json!({"type": "object"})
        );
        let messages = params["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][1]["mimeType"], "image/png");
        let calls = &messages[1]["content"];
        let wire = calls[0]["toolCallId"].as_str().unwrap();
        assert!(wire.len() <= TOOL_CALL_ID_LIMIT);
        assert_eq!(calls[0]["input"], json!({"path": "a"}));
        // The unanswered call gets a synthetic error result right after the assistant turn.
        assert_eq!(messages[2]["content"][0]["toolCallId"], "call_2");
        assert_eq!(messages[2]["content"][0]["output"]["type"], "error-text");
        assert_eq!(messages[3]["content"][0]["toolCallId"], wire);
        assert_eq!(messages[3]["content"][0]["toolName"], "read");
        assert_eq!(messages[3]["content"][0]["output"]["value"], "file body");
        assert_eq!(body["threadId"], "6f1c6a8e-4b1a-4f5e-9d2b-0c6f3b1a2d4e");
    }

    /// Options the generate endpoint cannot honour are rejected instead of ignored.
    #[test]
    fn rejects_forced_tool_choice_and_audio() {
        let forced = request(json!({"model": "m", "messages": [], "tool_choice": "required"}));
        assert!(build(&forced, "t").is_err());
        let audio = request(
            json!({"model": "m", "messages": [{"role": "user", "content": [
                {"type": "input_audio", "input_audio": {"data": "", "format": "wav"}}
            ]}]}),
        );
        assert!(build(&audio, "t").is_err());
    }
}
