use serde_json::{Value, json};

use crate::openai::{ChatCompletionRequest, MessageContent, Tool};
use crate::responses::{
    ResponseCreateRequest, ResponseInput, ResponseInputItem, ResponseTaggedItem,
};

/// Conversion helpers for chat and Responses API payloads.
mod conversion;
use conversion::{
    append_instructions, convert_content_to_input_parts, is_system_role, repair_call_ids,
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
    let choice = req.tool_choice.as_ref().map(|choice| {
        if choice.get("type").and_then(Value::as_str) == Some("function")
            && choice
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .is_some()
        {
            json!({"type": "function", "name": choice["function"]["name"]})
        } else {
            choice.clone()
        }
    });

    build_envelope(
        &req.model,
        input_items,
        (!instructions.is_empty()).then_some(instructions),
        req.tools.as_deref(),
        choice,
        req.reasoning_effort.as_deref(),
        req.reasoning.as_ref(),
    )
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
    let has_instructions = !instructions.trim().is_empty();
    build_envelope(
        &req.model,
        input_items,
        has_instructions.then_some(instructions),
        req.tools.as_deref(),
        req.tool_choice.clone(),
        req.reasoning_effort.as_deref(),
        req.reasoning.as_ref(),
    )
}

/// Assembles fields shared by both request formats after their input conversion.
#[allow(clippy::indexing_slicing)]
fn build_envelope(
    model: &str,
    mut input_items: Vec<Value>,
    instructions: Option<String>,
    tools: Option<&[Tool]>,
    choice: Option<Value>,
    reasoning_effort: Option<&str>,
    reasoning: Option<&Value>,
) -> Value {
    repair_call_ids(&mut input_items);
    let mut payload = json!({
        "model": model,
        "store": false,
        "stream": true,
        "input": input_items,
    });
    if let Some(instructions) = instructions {
        payload["instructions"] = json!(instructions);
    }
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        let wire_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.function.name,
                    "description": tool.function.description.as_deref().unwrap_or_default(),
                    "parameters": tool.function.parameters.clone().unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect();
        payload["tools"] = json!(wire_tools);
    }
    if let Some(choice) = choice {
        payload["tool_choice"] = choice;
    }
    payload["reasoning"] = extract_reasoning_payload(reasoning_effort, reasoning);
    payload
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
