use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

/// Reject options that translated providers cannot represent without changing their meaning.
pub(crate) async fn validate_translated_request(
    axum::extract::State(codex): axum::extract::State<bool>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, 20_000_000).await else {
        return (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "Request body too large",
        )
            .into_response();
    };
    if let Ok(value) = serde_json::from_slice::<Value>(&body)
        && let Some(field) = unsupported_translated_option(&value, codex)
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(crate::server::error_body(
                &format!("{field} is not supported by this translated provider"),
                "invalid_request_error",
                Some("unsupported_parameter"),
            )),
        )
            .into_response();
    }
    next.run(axum::extract::Request::from_parts(
        parts,
        axum::body::Body::from(body),
    ))
    .await
}

/// Identify unsupported request options before serde can discard them.
fn unsupported_translated_option(value: &Value, codex: bool) -> Option<&'static str> {
    if value
        .get("n")
        .is_some_and(|v| !v.is_null() && v.as_u64() != Some(1))
    {
        return Some("n");
    }
    if value
        .get("response_format")
        .is_some_and(|v| !v.is_null() && !is_default_text_format(v))
    {
        return Some("response_format");
    }
    if value
        .pointer("/text/format")
        .is_some_and(|v| !v.is_null() && !is_default_text_format(v))
    {
        return Some("text.format");
    }
    if value
        .get("parallel_tool_calls")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(true))
    {
        return Some("parallel_tool_calls");
    }
    for field in ["stop", "previous_response_id"] {
        if value.get(field).is_some_and(|v| !v.is_null()) {
            return Some(field);
        }
    }
    if value
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("strict")
                    .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
                    || tool
                        .pointer("/function/strict")
                        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
            })
        })
    {
        return Some("function strict");
    }
    if !codex
        && value
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            })
    {
        return Some("reasoning input");
    }
    if codex && has_unsupported_codex_content(value) {
        return Some("Codex content");
    }
    None
}

/// Detect Codex content that its payload translator cannot preserve.
fn has_unsupported_codex_content(value: &Value) -> bool {
    ["messages", "input"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(Value::as_array))
        .flatten()
        .any(|message| {
            if message.get("type").and_then(Value::as_str) == Some("function_call_output") {
                return message.get("output").is_some_and(|output| {
                    output.is_array()
                        || output
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| {
                                matches!(
                                    kind,
                                    "image_url"
                                        | "input_image"
                                        | "input_audio"
                                        | "file"
                                        | "input_file"
                                        | "video_url"
                                        | "input_video"
                                )
                            })
                });
            }
            let role = message.get("role").and_then(Value::as_str);
            if role == Some("tool")
                && message.get("type").and_then(Value::as_str) == Some("message")
            {
                return true;
            }
            message
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|part| {
                        let kind = part.get("type").and_then(Value::as_str);
                        if matches!(role, Some("tool" | "system" | "developer" | "assistant")) {
                            !matches!(kind, Some("text" | "input_text" | "output_text"))
                                || !part.get("text").is_some_and(Value::is_string)
                        } else {
                            kind == Some("input_image")
                                && !part.get("image_url").is_some_and(Value::is_string)
                        }
                    })
                })
        })
}

/// The only response format equivalent to leaving the format unspecified.
pub(crate) fn is_default_text_format(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 1 && object.get("type").and_then(Value::as_str) == Some("text")
    })
}

/// OpenAI-compatible chat completion request.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionRequest {
    /// Requested model identifier.
    pub model: String,
    /// Conversation history in order.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Nucleus sampling threshold.
    pub top_p: Option<f32>,
    /// Legacy output token limit.
    pub max_tokens: Option<u32>,
    /// Completion token limit.
    pub max_completion_tokens: Option<u32>,
    /// Whether to stream completion chunks.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Additional streaming options.
    pub stream_options: Option<StreamOptions>,
    /// Tools available to the model.
    pub tools: Option<Vec<Tool>>,
    /// Requested tool selection policy.
    pub tool_choice: Option<Value>,
    /// Requested reasoning effort.
    pub reasoning_effort: Option<String>,
    /// Provider-specific reasoning options.
    pub reasoning: Option<Value>,
}

/// A message in an OpenAI-compatible conversation.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    /// Speaker role.
    pub role: String,
    /// Text or multimodal content, if present.
    #[serde(default)]
    pub content: Option<MessageContent>,
    /// Optional participant name.
    pub name: Option<String>,
    /// Tool calls emitted by the assistant.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Call identifier associated with a tool result.
    pub tool_call_id: Option<String>,
    /// Provider-specific ordered assistant parts used for signature replay.
    pub provider_parts: Option<Vec<Value>>,
}

/// Message content as plain text or typed parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    /// Plain text content.
    Text(String),
    /// Ordered multimodal content parts.
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Concatenates text parts, ignoring images.
    pub(crate) fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        ContentPart::Text { text }
                        | ContentPart::InputText { text }
                        | ContentPart::OutputText { text } => out.push_str(text),
                        _ => {}
                    }
                }
                out
            }
        }
    }
}

/// Typed text and multimodal content carried by a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentPart {
    /// Chat-style text content.
    #[serde(rename = "text")]
    Text {
        /// Text payload.
        text: String,
    },
    /// Responses API input text.
    #[serde(rename = "input_text")]
    InputText {
        /// Text payload.
        text: String,
    },
    /// Responses API output text.
    #[serde(rename = "output_text")]
    OutputText {
        /// Text payload.
        text: String,
    },
    /// Chat-style image URL.
    #[serde(rename = "image_url")]
    ImageUrl {
        /// Image location.
        image_url: ImageUrl,
    },
    /// Responses API input image.
    #[serde(rename = "input_image")]
    InputImage {
        /// Optional image location.
        #[serde(default)]
        image_url: Option<String>,
    },
    /// Chat audio input.
    #[serde(rename = "input_audio")]
    InputAudio {
        /// Original fields, retained for provider-specific forwarding.
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    },
    /// Chat inline file.
    #[serde(rename = "file")]
    File {
        /// Original fields, retained for provider-specific forwarding.
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    },
    /// Responses inline file.
    #[serde(rename = "input_file")]
    InputFile {
        /// Original fields, retained for provider-specific forwarding.
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    },
    /// OpenRouter-style video URL.
    #[serde(rename = "video_url")]
    VideoUrl {
        /// Original fields, retained for provider-specific forwarding.
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    },
    /// Responses-style video input.
    #[serde(rename = "input_video")]
    InputVideo {
        /// Original fields, retained for provider-specific forwarding.
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    },
}

/// Image location in chat content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImageUrl {
    /// URL or data URL for the image.
    pub url: String,
}

/// Tool definition in chat completion format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Tool {
    /// Tool type discriminator.
    pub r#type: String,
    /// Function exposed to the model.
    pub function: FunctionDefinition,
}

/// Callable function advertised to the model.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionDefinition {
    /// Function name.
    pub name: String,
    /// Human-readable function description.
    pub description: Option<String>,
    /// JSON schema for function arguments.
    pub parameters: Option<Value>,
}

/// Assistant request to invoke a tool.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolCall {
    /// Tool call identifier.
    pub id: String,
    /// Tool type discriminator.
    pub r#type: String,
    /// Function name and serialized arguments.
    pub function: FunctionCall,
    /// Opaque provider thought signature for replaying this tool call.
    pub thought_signature: Option<String>,
}

/// Function invocation emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionCall {
    /// Function name.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// Complete non-streaming chat response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    /// Completion identifier.
    pub id: String,
    /// Wire object type.
    pub object: String,
    /// Creation time as a Unix timestamp.
    pub created: u64,
    /// Model that generated the response.
    pub model: String,
    /// Generated choices.
    pub choices: Vec<Choice>,
    /// Token accounting, if supplied.
    pub usage: Option<Usage>,
}

/// One complete chat response choice.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Choice {
    /// Choice position.
    pub index: u32,
    /// Generated assistant message.
    pub message: AssistantMessage,
    /// Reason generation stopped.
    pub finish_reason: Option<String>,
}

/// Assistant message in a complete response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AssistantMessage {
    /// Message role.
    pub role: String,
    /// Generated text, if present.
    pub content: Option<String>,
    /// Generated reasoning text, if present.
    pub reasoning_content: Option<String>,
    /// Requested tool invocations.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Provider-specific ordered assistant parts used for signature replay.
    pub provider_parts: Option<Vec<Value>>,
}

/// One streaming chat completion chunk.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatCompletionChunk {
    /// Completion identifier shared by chunks.
    pub id: String,
    /// Wire object type.
    pub object: String,
    /// Creation time as a Unix timestamp.
    pub created: u64,
    /// Model generating the response.
    pub model: String,
    /// Incremental choices in this chunk.
    pub choices: Vec<ChunkChoice>,
    /// Token accounting, if supplied.
    pub usage: Option<Usage>,
}

/// Incremental content for one streaming choice.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkChoice {
    /// Choice position.
    pub index: u32,
    /// Incremental message fields.
    pub delta: ChunkDelta,
    /// Reason generation stopped, if finished.
    pub finish_reason: Option<String>,
}

/// Incremental assistant message fields.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChunkDelta {
    /// Message role, when supplied.
    pub role: Option<String>,
    /// Generated text fragment.
    pub content: Option<String>,
    /// Reasoning text fragment.
    pub reasoning_content: Option<String>,
    /// Incremental tool calls.
    pub tool_calls: Option<Vec<ChunkToolCall>>,
    /// Provider-specific incremental assistant parts for signature replay.
    pub provider_parts: Option<Vec<Value>>,
}

/// Incremental tool call in a streaming response.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkToolCall {
    /// Tool call position within the choice.
    pub index: u32,
    /// Tool call identifier, when supplied.
    pub id: Option<String>,
    /// Tool type discriminator, when supplied.
    pub r#type: Option<String>,
    /// Incremental function invocation.
    pub function: Option<ChunkFunctionCall>,
    /// Opaque provider thought signature for replaying this tool call.
    pub thought_signature: Option<String>,
}

/// Incremental function name and arguments.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkFunctionCall {
    /// Function name, when supplied.
    pub name: Option<String>,
    /// Serialized argument fragment.
    pub arguments: Option<String>,
}

/// Token usage reported by a chat completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// These names are the OpenAI usage schema.
#[allow(clippy::struct_field_names)]
pub(crate) struct Usage {
    /// Input token count.
    pub prompt_tokens: u32,
    /// Generated token count.
    pub completion_tokens: u32,
    /// Combined token count.
    pub total_tokens: u32,
}

/// Optional settings for streamed completions.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamOptions {
    /// Whether the stream should include token usage.
    pub include_usage: Option<bool>,
}

/// OpenAI-compatible model listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelListResponse {
    /// Wire object type.
    pub object: String,
    /// Available model entries.
    pub data: Vec<Model>,
}

/// Model metadata returned by the listing endpoint.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct Model {
    /// Model identifier used in requests.
    pub id: String,
    /// Wire object type.
    pub object: Option<String>,
    /// Creation time as a Unix timestamp.
    pub created: Option<u64>,
    /// Model owner identifier.
    pub owned_by: Option<String>,
    /// Optional model name.
    pub name: Option<String>,
    /// Optional human-readable display name.
    pub display_name: Option<String>,
    /// Whether the model supports reasoning.
    pub reasoning: Option<bool>,
    /// Default reasoning effort, if specified.
    pub default_reasoning_effort: Option<String>,
    /// Supported reasoning effort values.
    pub supported_reasoning_efforts: Option<Vec<String>>,
    /// Maximum context size in tokens.
    pub context_window: Option<u64>,
    /// Maximum output size in tokens.
    pub max_output_tokens: Option<u32>,
    /// Provider-specific metadata retained across discovery.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod model_tests {
    use super::Model;
    use serde_json::json;

    #[test]
    fn model_roundtrip_preserves_provider_metadata() {
        let input = json!({
            "id": "example",
            "name": "Example",
            "provider_metadata": {"nested": [1, true, null]}
        });
        let model: Model = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(model).unwrap(), input);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod translated_request_tests {
    use super::*;
    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use serde_json::json;
    use tower::ServiceExt;

    async fn chat(Json(req): Json<ChatCompletionRequest>) -> Json<ChatCompletionRequest> {
        Json(req)
    }

    async fn responses(
        Json(req): Json<crate::responses::ResponseCreateRequest>,
    ) -> Json<crate::responses::ResponseCreateRequest> {
        Json(req)
    }

    fn app(codex: bool) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/responses", post(responses))
            .layer(axum::middleware::from_fn_with_state(
                codex,
                validate_translated_request,
            ))
    }

    async fn send(app: Router, path: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn translated_endpoints_reject_constraints_before_deserialization() {
        for codex in [false, true] {
            for (path, base) in [
                (
                    "/v1/chat/completions",
                    json!({"model":"m","messages":[{"role":"user","content":"hello"}]}),
                ),
                ("/v1/responses", json!({"model":"m","input":"hello"})),
            ] {
                for (field, value) in [
                    ("response_format", json!({"type":"json_object"})),
                    ("response_format", json!({"type":"text","extra":true})),
                    ("stop", json!(["end"])),
                    ("n", json!(2)),
                    ("parallel_tool_calls", json!(false)),
                    ("previous_response_id", json!("resp_1")),
                ] {
                    let mut body = base.clone();
                    body[field] = value;
                    let (status, error) = send(app(codex), path, body).await;
                    assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
                    assert_eq!(error["error"]["code"], "unsupported_parameter");
                }
                for format in [
                    json!({"type":"json_schema","name":"schema","schema":{"type":"object"}}),
                    json!({"type":"json_object"}),
                    json!({"type":"text","extra":true}),
                ] {
                    let mut body = base.clone();
                    body["text"] = json!({"format":format});
                    let (status, error) = send(app(codex), path, body).await;
                    assert_eq!(status, StatusCode::BAD_REQUEST);
                    assert_eq!(error["error"]["code"], "unsupported_parameter");
                }
                for tool in [
                    json!({"type":"function","function":{"name":"f","strict":true}}),
                    json!({"type":"function","name":"f","strict":true}),
                    json!({"type":"function","function":{"name":"f","strict":"false"}}),
                    json!({"type":"function","name":"f","strict":"false"}),
                ] {
                    let mut body = base.clone();
                    body["tools"] = json!([tool]);
                    assert_eq!(
                        send(app(codex), path, body).await.0,
                        StatusCode::BAD_REQUEST
                    );
                }
                let mut body = base.clone();
                body["tools"] = json!([{"type":"function","function":{"name":"f","strict":false}}]);
                let (status, result) = send(app(codex), path, body).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(result["tools"][0]["function"]["name"], "f");
                if path == "/v1/responses" {
                    let mut body = base.clone();
                    body["tools"] = json!([{"type":"function","name":"f","strict":false}]);
                    let (status, result) = send(app(codex), path, body).await;
                    assert_eq!(status, StatusCode::OK);
                    assert_eq!(result["tools"][0]["function"]["name"], "f");
                }
                let mut body = base;
                body["stop"] = Value::Null;
                body["n"] = json!(1);
                body["response_format"] = json!({"type":"text"});
                body["parallel_tool_calls"] = json!(true);
                body["text"] = json!({"format":{"type":"text"}});
                let (status, result) = send(app(codex), path, body).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(result["model"], "m");
            }
        }
    }

    #[tokio::test]
    async fn codex_rejects_media_in_tool_results_without_affecting_text() {
        for input in [
            json!({"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]}]}),
            json!({"model":"m","input":[{"role":"tool","tool_call_id":"c","content":[{"type":"input_image","image_url":"https://example.com/a.png"}]}]}),
            json!({"model":"m","input":[{"type":"function_call_output","call_id":"c","output":[{"type":"input_image","image_url":"https://example.com/a.png"}]}]}),
        ] {
            let path = if input.get("messages").is_some() {
                "/v1/chat/completions"
            } else {
                "/v1/responses"
            };
            assert_eq!(
                send(app(true), path, input).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        for (path, input) in [
            (
                "/v1/chat/completions",
                json!({"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"text","text":"ok"}]}]}),
            ),
            (
                "/v1/responses",
                json!({"model":"m","input":[{"type":"function_call_output","call_id":"c","output":"ok"}]}),
            ),
        ] {
            assert_eq!(send(app(true), path, input).await.0, StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn codex_preserves_reasoning_and_user_image_inputs() {
        let body = json!({"model":"m","input":[
            {"type":"reasoning","id":"r","summary":[],"encrypted_content":"opaque"},
            {"type":"message","role":"user","content":[{"type":"input_image","image_url":"https://example.com/a.png"}]}
        ]});
        let (status, result) = send(app(true), "/v1/responses", body.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["input"], body["input"]);
        assert_eq!(
            send(app(false), "/v1/responses", body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
