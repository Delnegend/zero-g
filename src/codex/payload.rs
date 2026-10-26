use serde_json::{Value, json};

use crate::openai::{ChatCompletionRequest, ContentPart, MessageContent};
use crate::responses::{
    ResponseCreateRequest, ResponseInput, ResponseInputItem, ResponseTaggedItem,
};

pub(crate) fn build_codex_payload_from_chat(req: &ChatCompletionRequest) -> Value {
    let mut input_items = Vec::new();
    let mut instructions = String::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(c) = &msg.content {
                    let content = c.as_text();
                    if !instructions.is_empty() {
                        instructions.push_str("\n\n");
                    }
                    instructions.push_str(&content);
                }
            }
            "user" => {
                let parts = convert_content_to_input_parts(msg.content.as_ref());
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
                        let args = if let Ok(parsed) =
                            serde_json::from_str::<Value>(&tc.function.arguments)
                        {
                            parsed
                        } else {
                            Value::String(tc.function.arguments.clone())
                        };
                        input_items.push(json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": args,
                        }));
                    }
                }
            }
            "tool" => {
                let call_id = msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_unknown".to_string());
                let output = msg
                    .content
                    .as_ref()
                    .map(super::super::openai::MessageContent::as_text)
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

pub(crate) fn build_codex_payload_from_responses(req: &ResponseCreateRequest) -> Value {
    let mut input_items = Vec::new();

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
                            tool_calls: _,
                        } => {
                            let parts = convert_content_to_input_parts(content.as_ref());
                            input_items.push(json!({
                                "type": "message",
                                "role": role,
                                "content": parts,
                            }));
                        }
                        ResponseTaggedItem::FunctionCall {
                            call_id,
                            id,
                            name,
                            arguments,
                        } => {
                            let cid = call_id
                                .clone()
                                .or_else(|| id.clone())
                                .unwrap_or_else(|| "call_unknown".to_string());
                            input_items.push(json!({
                                "type": "function_call",
                                "call_id": cid,
                                "name": name,
                                "arguments": arguments,
                            }));
                        }
                        ResponseTaggedItem::FunctionCallOutput { call_id, output } => {
                            let cid = call_id
                                .clone()
                                .unwrap_or_else(|| "call_unknown".to_string());
                            let out_str = if let Some(s) = output.as_str() {
                                s.to_string()
                            } else {
                                output.to_string()
                            };
                            input_items.push(json!({
                                "type": "function_call_output",
                                "call_id": cid,
                                "output": out_str,
                            }));
                        }
                    },
                    ResponseInputItem::Message(msg) => {
                        let parts = convert_content_to_input_parts(msg.content.as_ref());
                        input_items.push(json!({
                            "type": "message",
                            "role": msg.role,
                            "content": parts,
                        }));
                    }
                }
            }
        }
    }

    let mut payload = json!({
        "model": req.model,
        "store": false,
        "stream": true,
        "input": input_items,
    });

    if let Some(instructions) = &req.instructions
        && !instructions.trim().is_empty()
    {
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

fn convert_content_to_input_parts(content: Option<&MessageContent>) -> Vec<Value> {
    let mut parts = Vec::new();
    let Some(c) = content else { return parts };

    match c {
        MessageContent::Text(text) => {
            if !text.is_empty() {
                parts.push(json!({
                    "type": "input_text",
                    "text": text,
                }));
            }
        }
        MessageContent::Parts(raw_parts) => {
            for part in raw_parts {
                match part {
                    ContentPart::Text { text } => {
                        if !text.is_empty() {
                            parts.push(json!({
                                "type": "input_text",
                                "text": text,
                            }));
                        }
                    }
                    ContentPart::ImageUrl { image_url } => {
                        parts.push(json!({
                            "type": "input_image",
                            "image_url": { "url": &image_url.url },
                        }));
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
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello world".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
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
}
