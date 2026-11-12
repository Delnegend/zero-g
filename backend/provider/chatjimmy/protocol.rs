use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openai::Usage;

/// Upstream chat request body sent to `ChatJimmy`.
#[derive(Serialize)]
pub(super) struct ChatJimmyRequest {
    /// Turn messages.
    pub(super) messages: Vec<ChatJimmyMessage>,
    /// Model and system prompt options.
    #[serde(rename = "chatOptions")]
    pub(super) chat_options: ChatJimmyOptions,
    /// Optional attachment.
    pub(super) attachment: Option<Value>,
}

/// Message payload accepted by `ChatJimmy`.
#[derive(Serialize)]
pub(super) struct ChatJimmyMessage {
    /// Role name.
    pub(super) role: String,
    /// Message content.
    pub(super) content: String,
}

/// Sampling and model options for `ChatJimmy`.
#[derive(Serialize)]
pub(super) struct ChatJimmyOptions {
    /// Selected model ID.
    #[serde(rename = "selectedModel")]
    pub(super) selected_model: String,
    /// System prompt extracted from input.
    #[serde(rename = "systemPrompt")]
    pub(super) system_prompt: String,
    /// Top-K sampling parameter.
    #[serde(rename = "topK")]
    pub(super) top_k: u32,
}

/// Performance and token statistics returned by `ChatJimmy`.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Deserialize, Default)]
pub(super) struct ChatJimmyStats {
    /// Input tokens prefilled.
    pub(super) prefill_tokens: Option<u32>,
    /// Output tokens decoded.
    pub(super) decode_tokens: Option<u32>,
    /// Total tokens processed.
    pub(super) total_tokens: Option<u32>,
}

/// Converts decoded `ChatJimmyStats` into an `OpenAI` usage struct.
pub(super) fn usage_from_stats(stats: ChatJimmyStats) -> Usage {
    Usage {
        prompt_tokens: stats.prefill_tokens.unwrap_or(0),
        completion_tokens: stats.decode_tokens.unwrap_or(0),
        total_tokens: stats.total_tokens.unwrap_or(0),
    }
}

/// Splits the upstream raw text into message content and optional `ChatJimmy` statistics.
pub(super) fn parse_upstream(raw: &str) -> (String, Option<ChatJimmyStats>) {
    const STATS_START: &str = "<|stats|>";
    const STATS_END: &str = "<|/stats|>";
    if let Some((content, rest)) = raw.split_once(STATS_START) {
        let content = content.trim_end().to_string();
        if let Some((json_str, _)) = rest.split_once(STATS_END) {
            let stats = serde_json::from_str::<ChatJimmyStats>(json_str).ok();
            return (content, stats);
        }
        (content, None)
    } else {
        (raw.to_string(), None)
    }
}
