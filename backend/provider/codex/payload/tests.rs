use super::*;
use crate::openai::ChatMessage;

#[test]
fn default_chat_options_leave_codex_wire_request_unchanged() {
    let baseline = json!({
        "model": "gpt-5.5",
        "messages": [{"role": "user", "content": "hello"}]
    });
    let mut explicit = baseline.clone();
    explicit["n"] = json!(1);
    explicit["response_format"] = json!({"type":"text"});
    explicit["parallel_tool_calls"] = json!(true);
    let baseline: ChatCompletionRequest = serde_json::from_value(baseline).unwrap();
    let explicit: ChatCompletionRequest = serde_json::from_value(explicit).unwrap();
    let payload = build_codex_payload_from_chat(&explicit);
    assert_eq!(payload, build_codex_payload_from_chat(&baseline));
    assert_eq!(payload["input"][0]["content"][0]["text"], "hello");
}

#[test]
fn default_responses_options_leave_codex_wire_request_unchanged() {
    let baseline = json!({"model":"gpt-5.5", "input":"hello"});
    let mut explicit = baseline.clone();
    explicit["n"] = json!(1);
    explicit["response_format"] = json!({"type":"text"});
    explicit["parallel_tool_calls"] = json!(true);
    explicit["text"] = json!({"format":{"type":"text"}});
    let baseline: ResponseCreateRequest = serde_json::from_value(baseline).unwrap();
    let explicit: ResponseCreateRequest = serde_json::from_value(explicit).unwrap();
    let payload = build_codex_payload_from_responses(&explicit);
    assert_eq!(payload, build_codex_payload_from_responses(&baseline));
    assert_eq!(payload["input"][0]["content"][0]["text"], "hello");
}

#[test]
fn envelope_preserves_api_specific_choices_and_instruction_whitespace() {
    let chat: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "fixture", "messages": [{"role": "system", "content": "  "}],
        "tools": [{"type": "function", "function": {"name": "read"}}],
        "tool_choice": {"type": "function", "function": {"name": "read"}}
    }))
    .unwrap();
    let responses: ResponseCreateRequest = serde_json::from_value(json!({
        "model": "fixture", "input": "hello", "instructions": "  ",
        "tools": [{"type": "function", "name": "read"}],
        "tool_choice": {"type": "function", "function": {"name": "read"}}
    }))
    .unwrap();
    let chat = build_codex_payload_from_chat(&chat);
    let responses = build_codex_payload_from_responses(&responses);
    assert!(chat.get("instructions").is_none());
    assert!(responses.get("instructions").is_none());
    assert_eq!(
        chat["tool_choice"],
        json!({"type": "function", "name": "read"})
    );
    assert_eq!(
        responses["tool_choice"],
        json!({"type": "function", "function": {"name": "read"}})
    );
    for payload in [chat, responses] {
        assert_eq!(
            payload["tools"],
            json!([{"type": "function", "name": "read", "description": "", "parameters": {"type": "object"}}])
        );
    }
}

/// Interrupted Chat history retains context and maps a named tool selection.
#[test]
fn chat_repairs_interrupted_history_and_maps_named_tool_choice() {
    let req: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "gpt-5.5",
        "messages": [
            {"role": "user", "content": "Find the file"},
            {"role": "assistant", "tool_calls": [
                {"id": "call|1", "type": "function", "function": {"name": "read", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "missing", "content": "Important previous result"},
            {"role": "user", "content": "Continue"}
        ],
        "tool_choice": {"type": "function", "function": {"name": "read"}}
    })).unwrap();
    let payload = build_codex_payload_from_chat(&req);
    assert_eq!(
        payload["tool_choice"],
        json!({"type": "function", "name": "read"})
    );
    assert_eq!(payload["input"][0]["content"][0]["text"], "Find the file");
    assert_eq!(payload["input"][1]["call_id"], "call1");
    assert_eq!(payload["input"][2]["type"], "function_call_output");
    assert_eq!(payload["input"][2]["call_id"], "call1");
    assert!(
        payload["input"][2]["output"]
            .as_str()
            .unwrap()
            .contains("No tool output recorded")
    );
    assert_eq!(payload["input"][3]["role"], "assistant");
    let note = payload["input"][3]["content"][0]["text"].as_str().unwrap();
    assert!(note.contains("missing"));
    assert!(note.contains("Important previous result"));
    assert_eq!(payload["input"][4]["content"][0]["text"], "Continue");
    assert_eq!(payload, build_codex_payload_from_chat(&req));
}

/// Responses history retains orphaned results and deterministically pairs calls.
#[test]
fn responses_repairs_orphans_and_keeps_paired_results_and_ids_stable() {
    let req: ResponseCreateRequest = serde_json::from_value(json!({
        "model": "gpt-5.5",
        "input": [
            {"type": "function_call", "call_id": "a|b", "name": "read", "arguments": "{}"},
            {"type": "function_call", "call_id": "ab", "name": "read", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "ab", "output": "found"},
            {"type": "function_call_output", "call_id": "lost", "output": "preserve me"},
            {"type": "message", "role": "user", "content": "continue"}
        ],
        "tool_choice": {"type": "function", "name": "read"}
    }))
    .unwrap();
    let payload = build_codex_payload_from_responses(&req);
    let input = payload["input"].as_array().unwrap();
    assert_eq!(input[0]["call_id"], "ab");
    assert_eq!(input[1]["call_id"], "ab");
    assert_eq!(input[1]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "ab_1");
    assert_eq!(input[3]["call_id"], "ab_1");
    assert_eq!(input[3]["output"], "found");
    assert!(
        input[4]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("preserve me")
    );
    assert_eq!(input[5]["content"][0]["text"], "continue");
    assert_eq!(
        payload["tool_choice"],
        json!({"type": "function", "name": "read"})
    );
    assert_eq!(payload, build_codex_payload_from_responses(&req));
}

#[test]
fn responses_replays_encrypted_reasoning_and_tool_calls() {
    let req: ResponseCreateRequest = serde_json::from_value(json!({
        "model": "gpt-5.5",
        "input": [
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"},
            {"type": "message", "role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{}"}}
            ]},
            {"type": "function_call_output", "call_id": "call_1", "output": "found"}
        ]
    })).unwrap();
    let payload = build_codex_payload_from_responses(&req);
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(
        payload["input"],
        json!([
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"},
            {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "found"}
        ])
    );
}

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
        max_tokens: None,
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
        max_output_tokens: None,
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
fn reasoning_effort_precedence_is_identical_for_both_apis() {
    let chat: ChatCompletionRequest = serde_json::from_value(json!({
        "model":"fixture", "messages":[{"role":"user", "content":"hello"}],
        "reasoning_effort":"high", "reasoning":{"effort":"low", "summary":"detailed"}
    }))
    .unwrap();
    let responses: ResponseCreateRequest = serde_json::from_value(json!({
        "model":"fixture", "input":"hello",
        "reasoning_effort":"high", "reasoning":{"effort":"low", "summary":"detailed"}
    }))
    .unwrap();
    for payload in [
        build_codex_payload_from_chat(&chat),
        build_codex_payload_from_responses(&responses),
    ] {
        assert_eq!(
            payload["reasoning"],
            json!({"effort":"high", "summary":"detailed"})
        );
    }
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
        payload["input"][2]["arguments"],
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
