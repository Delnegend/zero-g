use crate::provider::common::cooldown;
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::session::RequestSession;
use axum::http::StatusCode;
use axum::{
    Extension, Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::Value;

use crate::{
    openai::{ChatCompletionRequest, ModelListResponse},
    provider::{
        claude::{downstream, models, payload, stream::Decoder, upstream},
        registry::Provider,
    },
    responses::ResponseCreateRequest,
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Build virtual-key-authenticated Claude routes.
pub(super) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list))
        .route("/v1/models/", get(list))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/chat/completions/", post(chat))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/", post(responses))
        .layer(axum::middleware::from_fn_with_state(
            false,
            crate::openai::validate_translated_request,
        ))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "claude"),
            settings::virtual_key_auth,
        ))
}
/// Construct a gateway error response.
fn failure(status: StatusCode, message: &str) -> Response {
    let kind = if status == StatusCode::TOO_MANY_REQUESTS {
        "rate_limit_error"
    } else if status.is_client_error() {
        "invalid_request_error"
    } else {
        "upstream_error"
    };
    (status, Json(crate::server::error_body(message, kind, None))).into_response()
}

/// A 429 answered by the gateway because every Claude account is cooling down.
fn cooling_down(wait: std::time::Duration) -> Response {
    let mut response = failure(
        StatusCode::TOO_MANY_REQUESTS,
        "All Claude accounts are rate limited; retry after the Retry-After delay",
    );
    if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

/// Report the known billing restriction without exposing upstream response bodies.
fn upstream_failure(status: StatusCode, body: &[u8]) -> String {
    const BILLING: &str = "Third-party apps now draw from extra usage, not plan limits. Ask your workspace admin to add more and keep going.";
    let billing_rejected = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(|message| message == BILLING)
        })
        .unwrap_or(false);
    if billing_rejected {
        format!("Claude upstream returned {status}: {BILLING}")
    } else {
        format!("Claude upstream returned {status}")
    }
}
/// Return models visible to the virtual key.
async fn list(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    match models::discover(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(mut models) => {
            models.retain(|m| permissions.allows_model(&m.id));
            Json(ModelListResponse {
                object: "list".into(),
                data: models,
            })
            .into_response()
        }
        Err(e) => failure(StatusCode::BAD_GATEWAY, &e),
    }
}
/// Handle chat completion requests.
async fn chat(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    inference(
        state,
        provider,
        permissions,
        session,
        identity.map(|Extension(identity)| identity),
        req,
        false,
    )
    .await
}
/// Handle Responses API requests.
async fn responses(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    if let crate::responses::ResponseInput::Items(items) = &req.input
        && items.iter().any(|item| {
            matches!(
                item,
                crate::responses::ResponseInputItem::Tagged(
                    crate::responses::ResponseTaggedItem::Reasoning { .. }
                )
            )
        })
    {
        return failure(
            StatusCode::BAD_REQUEST,
            "Claude reasoning must be replayed in message provider_parts",
        );
    }
    inference(
        state,
        provider,
        permissions,
        session,
        identity.map(|Extension(identity)| identity),
        req.into_chat_request(),
        true,
    )
    .await
}

/// Select a credential and invoke the upstream.
async fn inference(
    state: AppState,
    provider: Provider,
    permissions: Permissions,
    session: RequestSession,
    request_identity: Option<RequestIdentity>,
    mut req: ChatCompletionRequest,
    responses: bool,
) -> Response {
    if let Some(model) = req.model.strip_prefix("claude/") {
        req.model = model.to_owned();
    }
    if !permissions.allows_model(&req.model) {
        return failure(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
        );
    }
    let payload = match payload::build(&req) {
        Ok(v) => v,
        Err(e) => return failure(StatusCode::BAD_REQUEST, &e),
    };
    let attempt = upstream::inference(&state.settings, &provider, &req.model, &session.id, &payload).await;
    let (response, credential) = match attempt {
        Ok(accepted) => accepted,
        Err(upstream::Refusal::CoolingDown(wait)) => return cooling_down(wait),
        Err(upstream::Refusal::Failed(status, message)) => return failure(status, &message),
    };
    if !response.status().is_success() {
        let status = response.status();
        let reported = if status.is_redirection() {
            StatusCode::BAD_GATEWAY
        } else {
            status
        };
        let headers = response.headers().clone();
        let body = response.bytes().await.unwrap_or_default();
        let mut reply = failure(reported, &upstream_failure(status, &body));
        cooldown::copy_rate_limit_headers(&headers, reply.headers_mut());
        if status == StatusCode::TOO_MANY_REQUESTS && !reply.headers().contains_key("retry-after") {
            let wait = cooldown::wait_for(&headers);
            if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
                reply.headers_mut().insert("retry-after", value);
            }
        }
        return reply;
    }
    let decoder = Decoder::new(req.model.clone());
    if req.stream.unwrap_or(false) {
        if let Some(identity) = &request_identity {
            identity.set_credential(&credential.id, &credential.name);
        }
        return downstream::stream_response(
            response,
            decoder,
            responses,
            req.stream_options
                .is_some_and(|v| v.include_usage.unwrap_or(false)),
        );
    }
    match downstream::consume(response, decoder, None).await {
        Ok(done) => {
            if let Some(identity) = &request_identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            if responses {
                Json(downstream::response_object(&done)).into_response()
            } else {
                Json(downstream::chat_object(&done)).into_response()
            }
        }
        Err(e) => failure(StatusCode::BAD_GATEWAY, &e),
    }
}


/// Gateway stream behavior tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use super::super::{downstream::{consume, response_object, stream_response}, upstream as client};
    use axum::body::to_bytes;

    #[test]
    fn preserves_anthropic_billing_errors() {
        let status = StatusCode::BAD_REQUEST;
        let body = br#"{"type":"error","error":{"type":"invalid_request_error","message":"Third-party apps now draw from extra usage, not plan limits. Ask your workspace admin to add more and keep going."}}"#;
        assert!(
            upstream_failure(status, body).contains("Third-party apps now draw from extra usage")
        );
        assert_eq!(
            upstream_failure(
                status,
                br#"{"error":{"message":"private upstream content"}}"#
            ),
            "Claude upstream returned 400 Bad Request"
        );
        assert_eq!(
            upstream_failure(status, b"not json"),
            "Claude upstream returned 400 Bad Request"
        );
    }

    /// Preserve tool calls and signed reasoning through both stream formats.
    #[tokio::test]
    async fn streams_tool_calls_and_signed_reasoning_from_anthropic() {
        let upstream = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool1\",\"name\":\"_search\",\"input\":{}}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        for responses in [false, true] {
            let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
            let response = stream_response(upstream, Decoder::new("test".into()), responses, true);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let output = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(output.contains("tool1"));
            assert!(output.contains("search"));
            assert!(output.contains("sig"));
            if responses {
                assert!(output.contains("response.function_call_arguments.delta"));
                assert!(output.contains("response.completed"));
            } else {
                assert!(output.contains("[DONE]"));
                assert!(output.contains("tool_calls"));
            }
        }
    }

    /// A completed text block must not produce a reasoning delta.
    #[tokio::test]
    async fn text_only_responses_stream_has_no_reasoning_delta() {
        let upstream = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
        let response = stream_response(upstream, Decoder::new("test".into()), true, false);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("hello"));
        assert!(output.contains("response.completed"));
        assert!(!output.contains("response.reasoning_text.delta"));
    }

    /// Empty tool arguments and cached input counts survive both stream formats.
    #[tokio::test]
    async fn tool_only_stream_has_no_phantom_message() {
        let app = Router::new().route("/", get(|| async {
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":5}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"_search\",\"input\":{}}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        for responses in [false, true] {
            let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
            let response = stream_response(upstream, Decoder::new("test".into()), responses, true);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(
                body.contains("\\\"arguments\\\":\\\"{}\\\"")
                    || body.contains("\"arguments\":\"{}\"")
            );
            if responses {
                assert!(!body.contains("\"type\":\"message\""));
                assert!(body.contains("\"input_tokens\":10"));
            } else {
                assert!(body.contains("\"prompt_tokens\":10"));
            }
        }
    }

    /// A Responses output can be replayed without changing native block order.
    #[tokio::test]
    async fn responses_replay_retains_interleaved_blocks() {
        use axum::{body::Bytes, routing::post};
        let app = Router::new()
            .route("/events", get(|| async {
                concat!(
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"before\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"_search\",\"input\":{}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"thought\",\"signature\":\"signed\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n"
                )
            }))
            .route("/messages", post(|body: Bytes| async move {
                let value: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value.pointer("/messages/0/content/0/type").and_then(Value::as_str), Some("text"));
                assert_eq!(value.pointer("/messages/0/content/1/type").and_then(Value::as_str), Some("tool_use"));
                assert_eq!(value.pointer("/messages/0/content/2/signature").and_then(Value::as_str), Some("signed"));
                "ok"
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = reqwest::get(format!("http://{address}/events"))
            .await
            .unwrap();
        let done = consume(upstream, Decoder::new("test".into()), None)
            .await
            .unwrap();
        let output = response_object(&done);
        let request: ResponseCreateRequest =
            serde_json::from_value(json!({"model":"test","input":output.get("output").unwrap()}))
                .unwrap();
        let payload = payload::build(&request.into_chat_request()).unwrap();
        let result = client::send(
            &client::http().unwrap(),
            &format!("http://{address}/messages"),
            "secret",
            &payload,
        )
        .await
        .unwrap();
        assert!(result.status().is_success());
    }
}
