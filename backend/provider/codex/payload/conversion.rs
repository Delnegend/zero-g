use serde_json::{Value, json};

use crate::openai::{ContentPart, MessageContent};

/// Repairs incomplete tool history without discarding the original context.
#[allow(clippy::indexing_slicing)]
pub(super) fn repair_call_ids(items: &mut Vec<Value>) {
    let mut pending: Vec<(String, String, usize)> = Vec::new();
    let mut used = std::collections::HashSet::new();
    for (position, item) in items.iter_mut().enumerate() {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let original = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if kind == "function_call" {
            let base: String = original
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .take(64)
                .collect();
            let base = if base.is_empty() {
                "call".to_owned()
            } else {
                base
            };
            let mut id = base.clone();
            let mut suffix = 1usize;
            while used.contains(&id) {
                let ending = format!("_{suffix}");
                let prefix: String = base
                    .chars()
                    .take(64usize.saturating_sub(ending.len()))
                    .collect();
                id = format!("{prefix}{ending}");
                suffix = suffix.saturating_add(1);
            }
            used.insert(id.clone());
            pending.push((original, id.clone(), position));
            item["call_id"] = json!(id);
        } else if kind == "function_call_output" {
            let index = pending
                .iter()
                .position(|(source, _, _)| source == &original)
                .or_else(|| (original.is_empty() && pending.len() == 1).then_some(0));
            if let Some(index) = index {
                let (_, id, _) = pending.remove(index);
                item["call_id"] = json!(id);
            } else {
                let output = item
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                *item = json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": format!("[Previous tool result; call_id={original}]\n{output}")}],
                });
            }
        }
    }
    for (_, id, position) in pending.into_iter().rev() {
        items.insert(
            position.saturating_add(1),
            json!({
                "type": "function_call_output",
                "call_id": id,
                "output": "[No tool output recorded; the tool call may have been interrupted.]",
            }),
        );
    }
}
/// Returns whether a role contributes to the instruction string.
pub(super) fn is_system_role(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

/// Appends nonblank message text to instructions, separated by a blank line.
pub(super) fn append_instructions(instructions: &mut String, content: Option<&MessageContent>) {
    let Some(content) = content else { return };
    let text = content.as_text();
    if text.trim().is_empty() {
        return;
    }
    if !instructions.is_empty() {
        instructions.push_str("\n\n");
    }
    instructions.push_str(&text);
}

/// Converts message content to role-appropriate Codex text and image parts.
pub(super) fn convert_content_to_input_parts(
    content: Option<&MessageContent>,
    role: &str,
) -> Vec<Value> {
    let mut parts = Vec::new();
    let Some(c) = content else { return parts };

    // Upstream only accepts `output_text`/`refusal` parts in assistant turns and
    // `input_text` elsewhere, so the part type follows the message role.
    let is_assistant = role == "assistant";
    let text_type = if is_assistant {
        "output_text"
    } else {
        "input_text"
    };

    match c {
        MessageContent::Text(text) => {
            if !text.is_empty() {
                parts.push(json!({
                    "type": text_type,
                    "text": text,
                }));
            }
        }
        MessageContent::Parts(raw_parts) => {
            for part in raw_parts {
                match part {
                    ContentPart::Text { text }
                    | ContentPart::InputText { text }
                    | ContentPart::OutputText { text } => {
                        if !text.is_empty() {
                            parts.push(json!({
                                "type": text_type,
                                "text": text,
                            }));
                        }
                    }
                    ContentPart::ImageUrl { image_url } => {
                        if !is_assistant {
                            parts.push(json!({
                                "type": "input_image",
                                "image_url": &image_url.url,
                            }));
                        }
                    }
                    ContentPart::InputImage { image_url } => {
                        if let (false, Some(url)) = (is_assistant, image_url) {
                            parts.push(json!({
                                "type": "input_image",
                                "image_url": url,
                            }));
                        }
                    }
                    ContentPart::InputAudio { .. }
                    | ContentPart::File { .. }
                    | ContentPart::InputFile { .. }
                    | ContentPart::VideoUrl { .. }
                    | ContentPart::InputVideo { .. } => {
                        // Preserve unfamiliar media for the upstream provider to accept or reject.
                        if let Ok(value) = serde_json::to_value(part) {
                            parts.push(value);
                        }
                    }
                }
            }
        }
    }
    parts
}
