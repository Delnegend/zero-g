use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD;
use base64::read::DecoderReader;
use serde_json::{Value, json};

use crate::openai::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent, Tool};
use crate::provider::antigravity::AntigravityClient;

/// Per-request identifiers and model-specific metadata.
struct RequestMeta {
    /// Whether the wire model is a Claude model.
    is_claude: bool,
    /// Generated session identifier.
    session_id: String,
    /// Generated trajectory identifier.
    traj_id: String,
    /// Generated upstream request identifier.
    request_id: String,
}

impl AntigravityClient {
    /// Converts a chat completion request into the upstream request envelope.
    // These values are constructed as JSON objects; mutable indexing inserts optional fields.
    #[allow(clippy::indexing_slicing)]
    pub(crate) fn build_request_payload(
        req: &ChatCompletionRequest,
        project_id: &str,
        wire_model: &str,
        session_id: &str,
        info: Option<&super::AntigravityModelInfo>,
    ) -> Value {
        let meta = Self::new_request_meta(wire_model, session_id);
        let (system_prompts, contents) = Self::collect_messages(&req.messages, wire_model);
        let generation_config = Self::build_generation_config(req, info);
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

        if meta.is_claude
            || req.tool_choice.is_some()
            || req.tools.as_ref().is_some_and(|t| !t.is_empty())
        {
            let choice = req.tool_choice.as_ref();
            let mode = match choice.and_then(Value::as_str) {
                Some("none") => "NONE",
                Some("required") => "ANY",
                Some("auto") => "AUTO",
                _ if choice.is_some_and(Value::is_object) => "ANY",
                _ => "VALIDATED",
            };
            let mut config = json!({ "mode": mode });
            if let Some(name) = choice
                .and_then(|value| {
                    value
                        .pointer("/function/name")
                        .or_else(|| value.get("name"))
                })
                .and_then(Value::as_str)
            {
                config["allowedFunctionNames"] = json!([name]);
            }
            inner_request["toolConfig"] = json!({ "functionCallingConfig": config });
        }

        json!({
            "project": project_id,
            "requestId": meta.request_id,
            "userAgent": "antigravity",
            "model": wire_model,
            "request": inner_request
        })
    }

    /// Generates request identifiers and detects Claude models.
    fn new_request_meta(wire_model: &str, session_id: &str) -> RequestMeta {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let agent_id = uuid::Uuid::new_v4().to_string();
        let traj_id = uuid::Uuid::new_v4().to_string();
        let request_id = format!("agent/{agent_id}/{now_ms}/{traj_id}/1");

        RequestMeta {
            is_claude: wire_model.contains("claude"),
            session_id: session_id.to_owned(),
            traj_id,
            request_id,
        }
    }

    /// Separates system prompts from converted conversation contents.
    fn collect_messages(messages: &[ChatMessage], wire_model: &str) -> (Vec<String>, Vec<Value>) {
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
                    .and_then(|signature| super::replay_signature(wire_model, signature));
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
                    .and_then(|signature| super::replay_signature(wire_model, signature))
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

    /// Builds generation settings, applying model-specific output token limits.
    // The generation config is constructed as a JSON object; indexing inserts fields.
    #[allow(clippy::indexing_slicing)]
    fn build_generation_config(
        req: &ChatCompletionRequest,
        info: Option<&super::AntigravityModelInfo>,
    ) -> Value {
        let mut generation_config = json!({});
        if let Some(temp) = req.temperature {
            generation_config["temperature"] = json!(temp);
        }
        if let Some(top_p) = req.top_p {
            generation_config["topP"] = json!(top_p);
        }
        let requested = req.max_completion_tokens.or(req.max_tokens);
        let limit = info.and_then(|model| model.max_output_tokens);
        if let Some(max_output) = match (requested, limit) {
            (Some(requested), Some(limit)) => Some(requested.min(limit)),
            (Some(requested), None) => Some(requested),
            (None, limit) => limit,
        } {
            generation_config["maxOutputTokens"] = json!(max_output);
        }
        if let Some(info) = info {
            let mut thinking = json!({});
            if let Some(level) = info
                .extra
                .get("thinkingLevel")
                .and_then(Value::as_u64)
                .and_then(|level| match level {
                    1 => Some("LOW"),
                    2 => Some("MEDIUM"),
                    3 => Some("HIGH"),
                    _ => None,
                })
            {
                thinking["thinkingLevel"] = json!(level);
            } else if let Some(budget) = info.extra.get("thinkingBudget").and_then(Value::as_i64) {
                thinking["thinkingBudget"] = json!(budget);
            }
            if thinking
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                thinking["includeThoughts"] = json!(true);
                generation_config["thinkingConfig"] = thinking;
            }
        }
        generation_config
    }

    /// Builds trajectory and model-specific request labels.
    // Labels are constructed as a JSON object; indexing inserts optional fields.
    #[allow(clippy::indexing_slicing)]
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

    /// Converts function tools into upstream function declarations.
    // Each declaration is constructed as a JSON object; indexing inserts optional fields.
    #[allow(clippy::indexing_slicing)]
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

/// Validates media before model resolution or upstream calls.
pub(super) fn validate_media(messages: &[ChatMessage]) -> Result<(), String> {
    for message in messages {
        if let Some(MessageContent::Parts(parts)) = &message.content {
            for part in parts {
                if let Some((_, data)) = inline_media(part)? {
                    if message.role != "user" {
                        return Err(format!(
                            "Media is not supported in {} messages",
                            message.role
                        ));
                    }
                    if data.is_empty()
                        || std::io::copy(
                            &mut DecoderReader::new(data.as_bytes(), &STANDARD),
                            &mut std::io::sink(),
                        )
                        .is_err()
                    {
                        return Err("Media data must be nonempty valid base64".into());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Identifies supported inline media without fetching URLs or copying encoded bytes.
fn inline_media(part: &ContentPart) -> Result<Option<(&str, &str)>, String> {
    let (mime, data) = match part {
        ContentPart::ImageUrl { image_url } => data_url(&image_url.url, "image/")?,
        ContentPart::InputImage { image_url } => data_url(
            image_url
                .as_deref()
                .ok_or("input_image requires an inline image_url")?,
            "image/",
        )?,
        ContentPart::InputAudio { fields } => {
            let audio = fields
                .get("input_audio")
                .and_then(Value::as_object)
                .ok_or("input_audio requires an input_audio object")?;
            let data = audio
                .get("data")
                .and_then(Value::as_str)
                .ok_or("input_audio requires base64 data")?;
            let mime = match audio.get("format").and_then(Value::as_str) {
                Some("wav") => "audio/wav",
                Some("mp3") => "audio/mpeg",
                _ => return Err("input_audio format must be wav or mp3".into()),
            };
            (mime, data)
        }
        ContentPart::File { fields } => {
            let file = fields
                .get("file")
                .and_then(Value::as_object)
                .ok_or("file requires a file object")?;
            if file.contains_key("file_id") {
                return Err("file IDs are not supported; provide inline PDF file_data".into());
            }
            data_url(
                file.get("file_data")
                    .and_then(Value::as_str)
                    .ok_or("file requires inline PDF file_data")?,
                "application/pdf",
            )?
        }
        ContentPart::InputFile { fields } => {
            if fields.contains_key("file_id") || fields.contains_key("file_url") {
                return Err(
                    "file IDs and URLs are not supported; provide inline PDF file_data".into(),
                );
            }
            data_url(
                fields
                    .get("file_data")
                    .and_then(Value::as_str)
                    .ok_or("input_file requires inline PDF file_data")?,
                "application/pdf",
            )?
        }
        ContentPart::VideoUrl { fields } | ContentPart::InputVideo { fields } => {
            let value = fields
                .get("video_url")
                .or_else(|| fields.get("url"))
                .ok_or("video requires an inline video_url")?;
            let url = value
                .as_str()
                .or_else(|| value.get("url").and_then(Value::as_str))
                .ok_or("video_url must be a data URL or an object containing url")?;
            data_url(url, "video/")?
        }
        _ => return Ok(None),
    };
    Ok(Some((mime, data)))
}

/// Accepts only explicit base64 data URLs with a matching MIME type.
fn data_url<'a>(url: &'a str, expected: &str) -> Result<(&'a str, &'a str), String> {
    let (mime, data) = parse_data_url(url)
        .ok_or("Media must be an inline base64 data URL; external URLs are not supported")?;
    let matches = if expected.ends_with('/') {
        mime.strip_prefix(expected).is_some_and(|subtype| {
            !subtype.is_empty()
                && subtype
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
        })
    } else {
        mime == expected
    };
    if !matches {
        return Err(format!("Expected {expected} media MIME type"));
    }
    Ok((mime, data))
}

/// Splits a base64 data URL into its MIME type and encoded data.
fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, after_semi) = rest.split_once(';')?;
    after_semi.strip_prefix("base64,").map(|data| (mime, data))
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
            "test-session",
            None,
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
        assert!((0.6999..0.7001).contains(&temp));
        assert_eq!(inner["generationConfig"]["maxOutputTokens"], 4096);
    }

    #[test]
    fn forwards_ordered_chat_media_as_inline_data() {
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "gemini",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "first"},
                {"type": "input_audio", "input_audio": {"format": "wav", "data": "AQID"}},
                {"type": "input_audio", "input_audio": {"format": "mp3", "data": "BAUG"}},
                {"type": "file", "file": {"filename": "a.pdf", "file_data": "data:application/pdf;base64,BwgJ"}},
                {"type": "video_url", "video_url": {"url": "data:video/mp4;base64,CgsM"}},
                {"type": "text", "text": "last"}
            ]}]
        })).unwrap();
        validate_media(&req.messages).unwrap();
        let payload = AntigravityClient::build_request_payload(&req, "p", "gemini", "s", None);
        assert_eq!(
            payload["request"]["contents"][0]["parts"],
            json!([
                {"text": "first"},
                {"inlineData": {"mimeType": "audio/wav", "data": "AQID"}},
                {"inlineData": {"mimeType": "audio/mpeg", "data": "BAUG"}},
                {"inlineData": {"mimeType": "application/pdf", "data": "BwgJ"}},
                {"inlineData": {"mimeType": "video/mp4", "data": "CgsM"}},
                {"text": "last"}
            ])
        );
    }

    #[test]
    fn responses_file_and_video_reach_payload() {
        let req: crate::responses::ResponseCreateRequest = serde_json::from_value(json!({
            "model": "gemini",
            "input": [{"role": "user", "content": [
                {"type": "input_file", "filename": "a.pdf", "file_data": "data:application/pdf;base64,AQID"},
                {"type": "input_video", "video_url": "data:video/webm;base64,BAUG"}
            ]}]
        })).unwrap();
        let req = req.into_chat_request();
        validate_media(&req.messages).unwrap();
        let payload = AntigravityClient::build_request_payload(&req, "p", "gemini", "s", None);
        assert_eq!(
            payload["request"]["contents"][0]["parts"],
            json!([
                {"inlineData": {"mimeType": "application/pdf", "data": "AQID"}},
                {"inlineData": {"mimeType": "video/webm", "data": "BAUG"}}
            ])
        );
    }

    #[test]
    fn rejects_unforwardable_media_and_non_user_roles() {
        let bad_parts = [
            json!({"type":"image_url","image_url":{"url":"https://example.com/a.png"}}),
            json!({"type":"input_image","file_id":"file-1"}),
            json!({"type":"input_audio","input_audio":{"format":"ogg","data":"AQID"}}),
            json!({"type":"input_audio","input_audio":{"format":"wav","data":"%%%"}}),
            json!({"type":"file","file":{"file_id":"file-1"}}),
            json!({"type":"input_file","file_url":"https://example.com/a.pdf"}),
            json!({"type":"input_file","file_data":"data:text/plain;base64,AQID"}),
            json!({"type":"video_url","video_url":{"url":"https://example.com/a.mp4"}}),
            json!({"type":"input_video","video_url":"data:video/mp4;base64,"}),
        ];
        for part in bad_parts {
            let msg: ChatMessage =
                serde_json::from_value(json!({"role":"user","content":[part]})).unwrap();
            assert!(validate_media(&[msg]).is_err());
        }
        for role in ["system", "developer", "assistant", "tool", "other"] {
            let msg: ChatMessage = serde_json::from_value(json!({"role":role,"content":[
                {"type":"input_audio","input_audio":{"format":"wav","data":"AQID"}}
            ]}))
            .unwrap();
            assert!(validate_media(&[msg]).is_err());
        }
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

    #[test]
    fn parse_data_url_handles_unicode_and_invalid_prefixes() {
        assert_eq!(
            parse_data_url("data:é/base64;base64,abc"),
            Some(("é/base64", "abc"))
        );
        assert_eq!(parse_data_url("data:image/png,abc"), None);
        assert_eq!(parse_data_url("data:image/png;plain,abc"), None);
        assert_eq!(parse_data_url("image/png;base64,abc"), None);
    }
}
