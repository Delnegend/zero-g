//! ChatJimmy inference provider integration.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{
    IntoResponse, Json, Response,
    sse::{Event, Sse},
};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::ReceiverStream;

use crate::openai::{
    AssistantMessage, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice,
    ChunkChoice, ChunkDelta, MessageContent, Model, Usage,
};
use crate::provider::common::catalog::{CatalogInfo, CatalogPrices};
use crate::provider::common::proxy::{
    ProxyError, check_model, error, filter_models, retry_api_keys,
};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::{LegacyPolicy, Provider, ProviderDescriptor};
use crate::provider::registry;
use crate::server::{
    AppState,
    settings::{Permissions, SettingsStore},
};

/// Default `chatjimmy.ai` base URL.
pub(crate) const DEFAULT_BASE: &str = "https://chatjimmy.ai";
/// Upstream chat endpoint path.
const CHAT_PATH: &str = "/api/chat";
/// Upstream models endpoint path.
const MODELS_PATH: &str = "/api/models";
/// Default single model supported by `ChatJimmy` hardware.
const DEFAULT_MODEL: &str = "llama3.1-8B";

/// Shared client for `ChatJimmy` requests with keepalive and connection pooling.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
});

/// `ChatJimmy` backend.
pub(crate) struct Backend;

impl Provider for Backend {
    const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
        value: "chatjimmy",
        label: None,
        oauth: false,
        api_key: true,
        fixed_id: Some("chatjimmy"),
        fixed_name: Some("ChatJimmy"),
        keyless: true,
        default_base_url: Some(DEFAULT_BASE),
        legacy: LegacyPolicy {
            key: None,
            same_base: false,
        },
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        Router::new()
            .route("/v1/models", get(models_handler))
            .route("/v1/models/", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .route("/v1/chat/completions/", post(chat_handler))
            .layer(axum::middleware::from_fn_with_state(
                (settings.clone(), "chatjimmy"),
                crate::server::settings::virtual_key_auth,
            ))
    }

    async fn discover(
        _state: &AppState,
        _provider: &registry::Provider,
    ) -> Result<Vec<Model>, String> {
        discover_models().await
    }
}

/// Provider-supplied model details and reference pricing for `ChatJimmy` models.
pub(crate) fn catalog_info(_model: &Model) -> CatalogInfo {
    CatalogInfo {
        context_window: Some(6064),
        max_output_tokens: Some(2048),
        reasoning: Some(false),
        tool_call: Some(false),
        input_modalities: Some(vec!["text".to_string()]),
        reasoning_efforts: None,
        pricing: Some(CatalogPrices {
            input: Some(0.0),
            output: Some(0.0),
            cache_write: None,
            cache_read: None,
            source: "chatjimmy-reference",
            raw: serde_json::json!({
                "input_cost_per_token": 0.0,
                "output_cost_per_token": 0.0,
                "info": "Free public demo on Taalas HC1 silicon"
            }),
        }),
    }
}

/// Discovers models available from the `ChatJimmy` endpoint.
async fn discover_models() -> Result<Vec<Model>, String> {
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize ChatJimmy client")?;
    let resp = client
        .get(format!("{DEFAULT_BASE}{MODELS_PATH}"))
        .send()
        .await;
    if let Ok(r) = resp
        && r.status().is_success()
        && let Ok(value) = r.json::<Value>().await
    {
        let models = crate::provider::openai::models_from_list(&value);
        if !models.is_empty() {
            return Ok(models);
        }
    }
    Ok(vec![Model {
        id: DEFAULT_MODEL.to_string(),
        object: Some("model".to_string()),
        created: Some(1_700_000_000),
        owned_by: Some("Taalas Inc.".to_string()),
        name: Some("Llama 3.1 8B".to_string()),
        display_name: Some("Llama 3.1 8B (Taalas HC1)".to_string()),
        context_window: Some(6064),
        ..Default::default()
    }])
}

/// Upstream chat request body sent to `ChatJimmy`.
#[derive(Serialize)]
struct ChatJimmyRequest {
    /// Turn messages.
    messages: Vec<ChatJimmyMessage>,
    /// Model and system prompt options.
    #[serde(rename = "chatOptions")]
    chat_options: ChatJimmyOptions,
    /// Optional attachment.
    attachment: Option<Value>,
}

/// Message payload accepted by `ChatJimmy`.
#[derive(Serialize)]
struct ChatJimmyMessage {
    /// Role name.
    role: String,
    /// Message content.
    content: String,
}

/// Sampling and model options for `ChatJimmy`.
#[derive(Serialize)]
struct ChatJimmyOptions {
    /// Selected model ID.
    #[serde(rename = "selectedModel")]
    selected_model: String,
    /// System prompt extracted from input.
    #[serde(rename = "systemPrompt")]
    system_prompt: String,
    /// Top-K sampling parameter.
    #[serde(rename = "topK")]
    top_k: u32,
}

/// Performance and token statistics returned by `ChatJimmy`.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Deserialize, Default)]
struct ChatJimmyStats {
    /// Input tokens prefilled.
    prefill_tokens: Option<u32>,
    /// Output tokens decoded.
    decode_tokens: Option<u32>,
    /// Total tokens processed.
    total_tokens: Option<u32>,
}

/// Converts decoded `ChatJimmyStats` into an `OpenAI` usage struct.
fn usage_from_stats(stats: ChatJimmyStats) -> Usage {
    Usage {
        prompt_tokens: stats.prefill_tokens.unwrap_or(0),
        completion_tokens: stats.decode_tokens.unwrap_or(0),
        total_tokens: stats.total_tokens.unwrap_or(0),
    }
}

/// Splits the upstream raw text into message content and optional `ChatJimmy` statistics.
fn parse_upstream(raw: &str) -> (String, Option<ChatJimmyStats>) {
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

/// Splits generated text into natural phrase-sized chunks for smooth streaming.
fn split_stream_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_inclusive(|c: char| c.is_whitespace()) {
        current.push_str(word);
        if current.len() >= 30 {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Encodes a typed `ChatCompletionChunk` into an SSE event.
fn chunk_event(
    id: &str,
    created: u64,
    model: &str,
    delta: ChunkDelta,
    finish_reason: Option<&str>,
    usage: Option<Usage>,
) -> Option<Event> {
    let is_pure_usage = usage.is_some()
        && delta.role.is_none()
        && delta.content.is_none()
        && finish_reason.is_none();
    let chunk = ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: if is_pure_usage {
            vec![]
        } else {
            vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason: finish_reason.map(str::to_owned),
            }]
        },
        usage,
    };
    serde_json::to_string(&chunk)
        .ok()
        .map(|data| Event::default().data(data))
}

/// Returns the models list filtered by permissions.
async fn models_handler(Extension(permissions): Extension<Permissions>) -> Response {
    let models = discover_models().await.unwrap_or_default();
    let mut value = serde_json::json!({
        "object": "list",
        "data": models,
    });
    if filter_models(&mut value, &permissions) {
        Json(value).into_response()
    } else {
        (
            StatusCode::BAD_GATEWAY,
            Json(crate::server::error_body(
                "Invalid ChatJimmy model list",
                "upstream_error",
                Some("bad_gateway"),
            )),
        )
            .into_response()
    }
}

/// Sends a request upstream to `ChatJimmy` and parses the response.
async fn fetch_chatjimmy(
    settings: &SettingsStore,
    provider: &registry::Provider,
    payload_bytes: Bytes,
    identity: Option<&RequestIdentity>,
) -> Result<(String, Option<ChatJimmyStats>), ProxyError> {
    let base_url = if provider.base_url.is_empty() {
        DEFAULT_BASE
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!("{base_url}{CHAT_PATH}");

    let response = retry_api_keys(
        settings,
        provider,
        "No connected ChatJimmy credentials",
        identity,
        |_key| {
            let url = url.clone();
            let payload = payload_bytes.clone();
            async move {
                let client = CLIENT
                    .as_ref()
                    .map_err(|_| "Failed to initialize ChatJimmy client")?;
                client
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::USER_AGENT,
                        concat!("zero-g/", env!("CARGO_PKG_VERSION")),
                    )
                    .body(payload)
                    .send()
                    .await
                    .map_err(|_| "Failed to contact ChatJimmy upstream")
            }
        },
    )
    .await?;

    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "ChatJimmy upstream returned error status",
            "upstream_error",
            "bad_gateway",
        ));
    }

    let Ok(raw_text) = response.text().await else {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Failed to read ChatJimmy response",
            "upstream_error",
            "bad_gateway",
        ));
    };

    if raw_text.trim().is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "ChatJimmy returned empty response (input likely exceeded the ~6,000 token limit)",
            "invalid_request_error",
            "context_length_exceeded",
        ));
    }

    Ok(parse_upstream(&raw_text))
}

/// Handles `OpenAI` chat completions translated to `ChatJimmy`.
#[allow(clippy::too_many_lines)]
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<registry::Provider>,
    identity: Option<Extension<RequestIdentity>>,
    body: Bytes,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(crate::server::error_body(
                    &format!("Invalid request: {e}"),
                    "invalid_request_error",
                    Some("invalid_payload"),
                )),
            )
                .into_response();
        }
    };

    let mut system_prompts = Vec::new();
    let mut chat_messages = Vec::new();
    for msg in &req.messages {
        let text = msg
            .content
            .as_ref()
            .map(MessageContent::as_text)
            .unwrap_or_default();
        if msg.role == "system" || msg.role == "developer" {
            system_prompts.push(text);
        } else {
            chat_messages.push(ChatJimmyMessage {
                role: msg.role.clone(),
                content: text,
            });
        }
    }
    let selected_model = if req.model.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        req.model.clone()
    };
    let payload = ChatJimmyRequest {
        messages: chat_messages,
        chat_options: ChatJimmyOptions {
            selected_model,
            system_prompt: system_prompts.join("\n\n"),
            top_k: 8,
        },
        attachment: None,
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &format!("Failed to serialize request: {e}"),
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };

    let is_stream = req.stream.unwrap_or(false);
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if is_stream {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);
        let model = req.model.clone();
        let settings = state.settings.clone();
        let provider_clone = provider.clone();
        let id_for_task = id.clone();
        let identity_ref = identity.map(|Extension(id)| id);

        tokio::spawn(async move {
            let first = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: Some(String::new()),
                    ..Default::default()
                },
                None,
                None,
            );
            if let Some(event) = first
                && tx.send(Ok(event)).await.is_err()
            {
                return;
            }

            let result = fetch_chatjimmy(
                &settings,
                &provider_clone,
                payload_bytes,
                identity_ref.as_ref(),
            )
            .await;

            let (content, token_stats) = match result {
                Ok(data) => data,
                Err(err) => {
                    let err_data = serde_json::json!({
                        "error": {
                            "message": err.message(),
                            "type": err.kind(),
                            "code": err.code()
                        }
                    });
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("error")
                            .data(err_data.to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            };

            let usage = token_stats.map(usage_from_stats);

            for chunk_text in split_stream_chunks(&content) {
                let chunk = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta {
                        content: Some(chunk_text),
                        ..Default::default()
                    },
                    None,
                    None,
                );
                if let Some(event) = chunk
                    && tx.send(Ok(event)).await.is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let stop = chunk_event(
                &id_for_task,
                now,
                &model,
                ChunkDelta::default(),
                Some("stop"),
                None,
            );
            if let Some(event) = stop {
                let _ = tx.send(Ok(event)).await;
            }

            if let Some(usage) = usage {
                let usage_event = chunk_event(
                    &id_for_task,
                    now,
                    &model,
                    ChunkDelta::default(),
                    None,
                    Some(usage),
                );
                if let Some(event) = usage_event {
                    let _ = tx.send(Ok(event)).await;
                }
            }

            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        });

        Sse::new(ReceiverStream::new(rx)).into_response()
    } else {
        let (content, token_stats) = match fetch_chatjimmy(
            &state.settings,
            &provider,
            payload_bytes,
            identity.as_ref().map(|Extension(id)| id),
        )
        .await
        {
            Ok(data) => data,
            Err(err) => return err.into_response(),
        };

        let response = ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created: now,
            model: req.model,
            choices: vec![Choice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    reasoning_content: None,
                    tool_calls: None,
                    provider_parts: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: token_stats.map(usage_from_stats),
        };
        Json(response).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_upstream_with_stats() {
        let raw = "Hello world!\n<|stats|>{\"prefill_tokens\":16,\"decode_tokens\":4,\"total_tokens\":20}<|/stats|>";
        let (content, stats) = parse_upstream(raw);
        assert_eq!(content, "Hello world!");
        let stats = stats.expect("expected stats");
        assert_eq!(stats.prefill_tokens, Some(16));
        assert_eq!(stats.decode_tokens, Some(4));
        assert_eq!(stats.total_tokens, Some(20));
    }

    #[test]
    fn parse_upstream_without_stats() {
        let raw = "Just pure text";
        let (content, stats) = parse_upstream(raw);
        assert_eq!(content, "Just pure text");
        assert!(stats.is_none());
    }

    #[test]
    fn chatjimmy_request_serialization() {
        let req = ChatJimmyRequest {
            messages: vec![ChatJimmyMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            chat_options: ChatJimmyOptions {
                selected_model: "llama3.1-8B".to_string(),
                system_prompt: "You are a helpful assistant".to_string(),
                top_k: 8,
            },
            attachment: None,
        };
        let val = serde_json::to_value(&req).expect("serialize");
        assert_eq!(val["chatOptions"]["selectedModel"], "llama3.1-8B");
        assert_eq!(
            val["chatOptions"]["systemPrompt"],
            "You are a helpful assistant"
        );
        assert_eq!(val["messages"][0]["content"], "Hello");
    }

    #[test]
    fn split_stream_chunks_roundtrip() {
        let sample = "The article discusses a new regulation in Vietnam. Chuyện máy hỏng khi test!";
        let chunks = split_stream_chunks(sample);
        assert_ne!(chunks.len(), 0);
        assert_eq!(chunks.concat(), sample);
    }

    #[test]
    fn split_stream_chunks_multibyte_utf8() {
        let sample = "Chuyện máy hỏng khi test và nguyên tắc không khen điều tệ của Ngọc Vy là hai tình huống mà quy định mới chạm tới.";
        let chunks = split_stream_chunks(sample);
        assert_eq!(chunks.concat(), sample);
    }
}
