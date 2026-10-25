use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde_json::{Value, json};

use crate::antigravity::AntigravityClient;
use crate::openai::{ChatCompletionRequest, MessageContent};

impl AntigravityClient {
    pub(crate) fn build_request_payload(
        req: &ChatCompletionRequest,
        project_id: &str,
        wire_model: &str,
    ) -> Value {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let agent_id = uuid::Uuid::new_v4().to_string();
        let traj_id = uuid::Uuid::new_v4().to_string();
        let request_id = format!("agent/{agent_id}/{now_ms}/{traj_id}/1");

        let mut rng = rand::rng();
        let session_id = format!("-{}", rng.next_u64() % 9_000_000_000_000_000_000);

        let is_claude = wire_model.contains("claude");

        let mut system_prompts = Vec::new();
        let mut contents = Vec::new();
        let mut tool_call_names: HashMap<String, String> = HashMap::new();

        for msg in &req.messages {
            match msg.role.as_str() {
                "system" | "developer" => {
                    if let Some(c) = &msg.content {
                        let text = c.as_text();
                        if !text.trim().is_empty() {
                            system_prompts.push(text);
                        }
                    }
                }
                "user" => {
                    let mut parts = Vec::new();
                    if let Some(c) = &msg.content {
                        match c {
                            MessageContent::Text(t) => {
                                if !t.trim().is_empty() {
                                    parts.push(json!({ "text": t }));
                                }
                            }
                            MessageContent::Parts(subparts) => {
                                for p in subparts {
                                    match p {
                                        crate::openai::ContentPart::Text { text } => {
                                            if !text.trim().is_empty() {
                                                parts.push(json!({ "text": text }));
                                            }
                                        }
                                        crate::openai::ContentPart::ImageUrl { image_url } => {
                                            if let Some((mime, data)) =
                                                parse_data_url(&image_url.url)
                                            {
                                                parts.push(json!({
                                                    "inlineData": {
                                                        "mimeType": mime,
                                                        "data": data
                                                    }
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !parts.is_empty() {
                        contents.push(json!({
                            "role": "user",
                            "parts": parts
                        }));
                    }
                }
                "assistant" => {
                    let mut parts = Vec::new();
                    if let Some(c) = &msg.content {
                        let text = c.as_text();
                        if !text.trim().is_empty() {
                            parts.push(json!({ "text": text }));
                        }
                    }
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            tool_call_names.insert(tc.id.clone(), tc.function.name.clone());
                            let args: Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or_else(|_| json!({}));
                            parts.push(json!({
                                "functionCall": {
                                    "name": tc.function.name,
                                    "args": args,
                                    "id": tc.id
                                },
                                "thoughtSignature": "skip_thought_signature_validator"
                            }));
                        }
                    }
                    if !parts.is_empty() {
                        contents.push(json!({
                            "role": "model",
                            "parts": parts
                        }));
                    }
                }
                "tool" => {
                    let tool_name = msg
                        .name
                        .clone()
                        .or_else(|| {
                            msg.tool_call_id
                                .as_ref()
                                .and_then(|id| tool_call_names.get(id).cloned())
                        })
                        .unwrap_or_else(|| "tool".to_string());
                    let result_str = msg
                        .content
                        .as_ref()
                        .map(super::super::openai::MessageContent::as_text)
                        .unwrap_or_default();
                    let response_obj: Value = serde_json::from_str(&result_str)
                        .unwrap_or_else(|_| json!({ "output": result_str }));

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

                    let mut merged = false;
                    if let Some(last) = contents.last_mut()
                        && last.get("role").and_then(|r| r.as_str()) == Some("user")
                        && last
                            .get("parts")
                            .and_then(|p| p.as_array())
                            .is_some_and(|parts| {
                                parts.iter().any(|p| p.get("functionResponse").is_some())
                            })
                        && let Some(parts) = last.get_mut("parts").and_then(|p| p.as_array_mut())
                    {
                        parts.push(part.clone());
                        merged = true;
                    }
                    if !merged {
                        contents.push(json!({
                            "role": "user",
                            "parts": [part]
                        }));
                    }
                }
                _ => {}
            }
        }

        let mut generation_config = json!({});
        if let Some(temp) = req.temperature {
            generation_config["temperature"] = json!(temp);
        }
        if let Some(top_p) = req.top_p {
            generation_config["topP"] = json!(top_p);
        }
        let max_output = req
            .max_completion_tokens
            .or(req.max_tokens)
            .unwrap_or(if is_claude { 64000 } else { 65535 });
        let max_output = if is_claude && max_output > 64000 {
            64000
        } else {
            max_output
        };
        generation_config["maxOutputTokens"] = json!(max_output);

        let mut labels = json!({
            "trajectory_id": traj_id,
            "last_step_index": "1"
        });
        if is_claude {
            labels["used_claude"] = json!("true");
            labels["used_claude_conservative"] = json!("true");
        }

        let mut inner_request = json!({
            "contents": contents,
            "generationConfig": generation_config,
            "labels": labels,
            "sessionId": session_id,
        });

        if !system_prompts.is_empty() {
            let combined = system_prompts.join("\n\n");
            inner_request["systemInstruction"] = json!({
                "role": "user",
                "parts": [{ "text": combined }]
            });
        }

        if let Some(tools) = &req.tools
            && !tools.is_empty()
        {
            let mut decls = Vec::new();
            for t in tools {
                let mut decl = json!({
                    "name": t.function.name,
                });
                if let Some(desc) = &t.function.description {
                    decl["description"] = json!(desc);
                }
                if let Some(params) = &t.function.parameters {
                    decl["parameters"] = params.clone();
                }
                decls.push(decl);
            }
            inner_request["tools"] = json!([{
                "functionDeclarations": decls
            }]);
        }

        if is_claude || req.tools.as_ref().is_some_and(|t| !t.is_empty()) {
            inner_request["toolConfig"] = json!({
                "functionCallingConfig": {
                    "mode": "VALIDATED"
                }
            });
        }

        json!({
            "project": project_id,
            "requestId": request_id,
            "userAgent": "antigravity",
            "model": wire_model,
            "request": inner_request
        })
    }
}

pub(crate) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    if !url.starts_with("data:") {
        return None;
    }
    let rest = &url[5..];
    let semicolon = rest.find(';')?;
    let mime = &rest[..semicolon];
    let after_semi = &rest[semicolon + 1..];
    let base64_prefix = "base64,";
    if let Some(data) = after_semi.strip_prefix(base64_prefix) {
        Some((mime, data))
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::openai::{ChatMessage, MessageContent};

    #[test]
    fn test_map_model_aliases() {
        assert_eq!(
            AntigravityClient::map_model_id("claude-3-5-sonnet"),
            "claude-sonnet-5-5-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("gemini-flash"),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            AntigravityClient::map_model_id("gemini-pro"),
            "gemini-4-argon-low"
        );
        assert_eq!(
            AntigravityClient::map_model_id("claude-opus"),
            "claude-opus-5-5-medium"
        );
        assert_eq!(
            AntigravityClient::map_model_id("claude-sonnet-5-5-high"),
            "claude-sonnet-5-5-high"
        );
    }

    #[test]
    fn test_build_request_payload_system_and_user() {
        let req = ChatCompletionRequest {
            model: "claude-sonnet".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text("You are an assistant".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello world".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(4096),
            max_completion_tokens: None,
            stream: None,
            stream_options: None,
            tools: None,
            tool_choice: None,
        };

        let payload = AntigravityClient::build_request_payload(
            &req,
            "test-proj-123",
            "claude-sonnet-5-5-medium",
        );

        assert_eq!(payload["project"], "test-proj-123");
        assert_eq!(payload["model"], "claude-sonnet-5-5-medium");
        assert_eq!(payload["userAgent"], "antigravity");

        let inner = &payload["request"];
        assert_eq!(inner["systemInstruction"]["role"], "user");
        assert_eq!(
            inner["systemInstruction"]["parts"][0]["text"],
            "You are an assistant"
        );

        let contents = inner["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello world");

        assert_eq!(inner["labels"]["used_claude"], "true");
        let temp = inner["generationConfig"]["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 1e-4);
        assert_eq!(inner["generationConfig"]["maxOutputTokens"], 4096);
    }

    #[test]
    fn test_parse_data_url() {
        let data_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let parsed = parse_data_url(data_url);
        assert!(parsed.is_some());
        let (mime, data) = parsed.unwrap();
        assert_eq!(mime, "image/png");
        assert!(data.starts_with("iVBORw0KGgo"));
    }
}
