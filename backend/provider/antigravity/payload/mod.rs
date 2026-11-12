use serde_json::{Value, json};

use crate::openai::{ChatCompletionRequest, Tool};
use crate::provider::antigravity::AntigravityClient;

mod media;
mod messages;
mod schema;

#[cfg(test)]
use media::parse_data_url;
pub(super) use media::validate_media;
use schema::gemini_schema;

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
                decl["parameters"] = gemini_schema(params);
            }
            decls.push(decl);
        }
        decls
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
