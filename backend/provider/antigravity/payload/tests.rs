//! Tests for Antigravity request payload conversion.

use super::{AntigravityClient, ChatCompletionRequest, parse_data_url, validate_media};
use crate::openai::{ChatMessage, MessageContent};
use serde_json::json;

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

#[test]
fn tool_schemas_are_reduced_to_the_gemini_subset() {
    let req: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "gemini-3.8-flash",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "function": {
            "name": "grep",
            "parameters": {
                "type": "object",
                "properties": {
                    "skip": {"type": ["number", "null"]},
                    "mode": {"const": "fast"},
                    "query": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
                    "choice": {"type": "string", "anyOf": [{"type": "string"}, {"type": "null"}]},
                    "nested": {
                        "type": "object",
                        "properties": {"value": {"type": ["string", "null"]}},
                        "additionalProperties": false
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }}]
    }))
    .unwrap();
    let payload =
        AntigravityClient::build_request_payload(&req, "p", "gemini-3.8-flash", "s", None);
    let params = &payload["request"]["tools"][0]["functionDeclarations"][0]["parameters"];

    assert_eq!(params["type"], json!("object"));
    assert_eq!(params["properties"]["skip"]["type"], json!("number"));
    assert_eq!(params["properties"]["skip"]["nullable"], json!(true));
    assert_eq!(params["properties"]["mode"]["enum"], json!(["fast"]));
    assert_eq!(
        params["properties"]["query"]["anyOf"][0]["type"],
        json!("string")
    );
    assert!(params["properties"]["query"].get("type").is_none());
    assert!(params["properties"]["choice"].get("type").is_none());
    assert_eq!(
        params["properties"]["nested"]["properties"]["value"]["type"],
        json!("string")
    );
    assert_eq!(
        params["properties"]["nested"]["properties"]["value"]["nullable"],
        json!(true)
    );
    assert!(params.get("additionalProperties").is_none());
    assert!(
        params["properties"]["nested"]
            .get("additionalProperties")
            .is_none()
    );
    assert_eq!(params["required"], json!(["query"]));
}
