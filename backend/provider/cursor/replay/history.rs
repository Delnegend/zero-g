use std::collections::{HashMap, HashSet};

use prost::Message;
use serde_json::json;

use super::{
    call_arguments, call_step, deterministic_id, proto, root_user_content, store, store_json, text,
    tool_id, user_message,
};
use crate::openai::ChatMessage;

/// Stores a completed conversation turn and returns its blob ID.
fn store_turn(
    blobs: &mut HashMap<Vec<u8>, Vec<u8>>,
    user: Vec<u8>,
    steps: Vec<Vec<u8>>,
) -> Vec<u8> {
    let turn = proto::ConversationTurnStructure {
        turn: Some(
            proto::conversation_turn_structure::Turn::AgentConversationTurn(
                proto::AgentConversationTurnStructure {
                    user_message: user,
                    steps,
                    ..Default::default()
                },
            ),
        ),
    };
    store(blobs, turn.encode_to_vec())
}

/// Indexes tool results and call names for pairing historical messages.
fn history_indexes(
    history: &[ChatMessage],
) -> (HashMap<String, &ChatMessage>, HashMap<String, String>) {
    let mut results = HashMap::new();
    let mut names = HashMap::new();
    for message in history {
        if message.role == "tool"
            && let Some(id) = &message.tool_call_id
        {
            results.insert(id.clone(), message);
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                names.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }
    (results, names)
}

/// Rebuilds conversation turns and their root-prompt history.
pub(super) fn history_records(
    history: &[ChatMessage],
    blobs: &mut HashMap<Vec<u8>, Vec<u8>>,
    root: &mut Vec<Vec<u8>>,
) -> Result<Vec<Vec<u8>>, String> {
    let (results, call_names) = history_indexes(history);
    let mut turns = Vec::new();
    let mut current_user: Option<Vec<u8>> = None;
    let mut steps = Vec::new();
    let mut emitted_results = HashSet::new();
    for message in history {
        match message.role.as_str() {
            "system" => {}
            "user" | "developer" => {
                if let Some(user) = current_user.take() {
                    turns.push(store_turn(blobs, user, std::mem::take(&mut steps)));
                }
                let content = root_user_content(message)?;
                let user = user_message(
                    message,
                    deterministic_id(&format!("u:{}:{}", turns.len(), text(message).trim())),
                )?;
                if !content.is_empty() {
                    root.push(store_json(
                        blobs,
                        &json!({"role":"user","content":content}),
                    )?);
                    current_user = Some(store(blobs, user.encode_to_vec()));
                }
            }
            "assistant" => {
                let mut seen_calls = HashSet::new();
                let mut parts = Vec::new();
                let value = text(message);
                if !value.is_empty() {
                    parts.push(json!({"type":"text","text":value}));
                    let step = proto::ConversationStep {
                        message: Some(proto::conversation_step::Message::AssistantMessage(
                            proto::AssistantMessage { text: value },
                        )),
                    };
                    steps.push(store(blobs, step.encode_to_vec()));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        if !seen_calls.insert(call.id.clone()) {
                            continue;
                        }
                        let args = call_arguments(call)?;
                        parts.push(json!({"type":"tool-call","toolCallId":tool_id(&call.id),"toolName":call.function.name,"args":args}));
                        let step = call_step(call, results.get(&call.id).copied())?;
                        steps.push(store(blobs, step.encode_to_vec()));
                    }
                }
                if !parts.is_empty() {
                    root.push(store_json(
                        blobs,
                        &json!({"role":"assistant","content":parts}),
                    )?);
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .ok_or_else(|| "Tool result is missing tool_call_id".to_string())?;
                if !emitted_results.insert(id.to_string()) {
                    continue;
                }
                let message = results.get(id).copied().unwrap_or(message);
                if let Some(name) = call_names.get(id) {
                    root.push(store_json(blobs, &json!({"role":"tool","id":tool_id(id),"content":[{"type":"tool-result","toolName":name,"toolCallId":tool_id(id),"result":text(message)}]}))?);
                } else {
                    let result = format!("[Tool Result]\n{}", text(message));
                    root.push(store_json(
                        blobs,
                        &json!({"role":"assistant","content":[{"type":"text","text":result}]}),
                    )?);
                    let step = proto::ConversationStep {
                        message: Some(proto::conversation_step::Message::AssistantMessage(
                            proto::AssistantMessage { text: result },
                        )),
                    };
                    steps.push(store(blobs, step.encode_to_vec()));
                }
            }
            role => return Err(format!("Unsupported conversation role: {role}")),
        }
    }
    if let Some(user) = current_user {
        turns.push(store_turn(blobs, user, steps));
    }
    Ok(turns)
}
