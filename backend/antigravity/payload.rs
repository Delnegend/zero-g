use std::collections::HashMap;

use rand::RngCore;
use serde_json::{Value, json};

use crate::antigravity::AntigravityClient;
use crate::openai::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent, Tool};

struct RequestMeta {
    is_claude: bool,
    session_id: String,
    traj_id: String,
    request_id: String,
}

impl AntigravityClient {
    pub(crate) fn build_request_payload(
        req: &ChatCompletionRequest,
        project_id: &str,
        wire_model: &str,
    ) -> Value {
        let meta = Self::new_request_meta(wire_model);
        let (system_prompts, contents) = Self::collect_messages(&req.messages);
        let generation_config = Self::build_generation_config(req, meta.is_claude);
        let labels = Self::build_labels(&meta.traj_id, meta.is_claude);

        let mut inner_request = json!({
            "contents": contents,
            "generationConfig": generation_config,
            "labels": labels,
            "sessionId": meta.session_id,
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
            inner_request["tools"] = json!([{
                "functionDeclarations": Self::tool_declarations(tools)
            }]);
        }

        if meta.is_claude || req.tools.as_ref().is_some_and(|t| !t.is_empty()) {
            inner_request["toolConfig"] = json!({
                "functionCallingConfig": {
                    "mode": "VALIDATED"
                }
            });
        }

        json!({
            "project": project_id,
            "requestId": meta.request_id,
            "userAgent": "antigravity",
            "model": wire_model,
            "request": inner_request
        })
    }

    fn new_request_meta(wire_model: &str) -> RequestMeta {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let agent_id = uuid::Uuid::new_v4().to_string();
        let traj_id = uuid::Uuid::new_v4().to_string();
        let request_id = format!("agent/{agent_id}/{now_ms}/{traj_id}/1");

        let mut rng = rand::rng();
        let session_id = format!("-{}", rng.next_u64() % 9_000_000_000_000_000_000);

        RequestMeta {
            is_claude: wire_model.contains("claude"),
            session_id,
            traj_id,
            request_id,
        }
    }

    fn collect_messages(messages: &[ChatMessage]) -> (Vec<String>, Vec<Value>) {
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
                    Self::handle_assistant_message(msg, &mut contents, &mut tool_call_names);
                }
                "tool" => {
                    Self::handle_tool_message(msg, &mut contents, &tool_call_names);
                }
                _ => {}
            }
        }

        (system_prompts, contents)
    }

    fn handle_system_message(msg: &ChatMessage, system_prompts: &mut Vec<String>) {
        if let Some(content) = &msg.content {
            let text = content.as_text();
            if !text.trim().is_empty() {
                system_prompts.push(text);
            }
        }
    }

    fn handle_user_message(msg: &ChatMessage, contents: &mut Vec<Value>) {
        let parts = Self::user_content_parts(msg.content.as_ref());
        if !parts.is_empty() {
            contents.push(json!({
                "role": "user",
                "parts": parts
            }));
        }
    }

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
                            ContentPart::ImageUrl { image_url } => {
                                push_inline_image(&mut parts, &image_url.url);
                            }
                            ContentPart::InputImage { image_url } => {
                                if let Some(url) = image_url {
                                    push_inline_image(&mut parts, url);
                                }
                            }
                        }
                    }
                }
            }
        }
        parts
    }

    fn handle_assistant_message(
        msg: &ChatMessage,
        contents: &mut Vec<Value>,
        tool_call_names: &mut HashMap<String, String>,
    ) {
        let parts = Self::assistant_parts(msg, tool_call_names);
        if !parts.is_empty() {
            contents.push(json!({
                "role": "model",
                "parts": parts
            }));
        }
    }

    fn assistant_parts(
        msg: &ChatMessage,
        tool_call_names: &mut HashMap<String, String>,
    ) -> Vec<Value> {
        let mut parts = Vec::new();
        if let Some(content) = &msg.content {
            let text = content.as_text();
            if !text.trim().is_empty() {
                parts.push(json!({ "text": text }));
            }
        }
        if let Some(tool_calls) = &msg.tool_calls {
            for tool_call in tool_calls {
                tool_call_names.insert(tool_call.id.clone(), tool_call.function.name.clone());
                let args: Value = serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({}));
                parts.push(json!({
                    "functionCall": {
                        "name": tool_call.function.name,
                        "args": args,
                        "id": tool_call.id
                    },
                    "thoughtSignature": "skip_thought_signature_validator"
                }));
            }
        }
        parts
    }

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
        let result_str = msg
            .content
            .as_ref()
            .map(MessageContent::as_text)
            .unwrap_or_default();
        let response_obj: Value =
            serde_json::from_str(&result_str).unwrap_or_else(|_| json!({ "output": result_str }));

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

    fn build_generation_config(req: &ChatCompletionRequest, is_claude: bool) -> Value {
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
        generation_config
    }

    fn build_labels(traj_id: &str, is_claude: bool) -> Value {
        let mut labels = json!({
            "trajectory_id": traj_id,
            "last_step_index": "1"
        });
        if is_claude {
            labels["used_claude"] = json!("true");
            labels["used_claude_conservative"] = json!("true");
        }
        labels
    }

    fn tool_declarations(tools: &[Tool]) -> Vec<Value> {
        let mut decls = Vec::new();
        for tool in tools {
            let mut decl = json!({
                "name": tool.function.name,
            });
            if let Some(desc) = &tool.function.description {
                decl["description"] = json!(desc);
            }
            if let Some(params) = &tool.function.parameters {
                decl["parameters"] = params.clone();
            }
            decls.push(decl);
        }
        decls
    }
}

fn push_inline_image(parts: &mut Vec<Value>, url: &str) {
    if let Some((mime, data)) = parse_data_url(url) {
        parts.push(json!({
            "inlineData": {
                "mimeType": mime,
                "data": data
            }
        }));
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
            reasoning_effort: None,
            reasoning: None,
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
