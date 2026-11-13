//! Command Code Go plan access through the CLI's `/alpha/generate` endpoint.
//!
//! The Go plan has no Provider API entitlement; the official CLI streams through this
//! undocumented endpoint instead, so its wire format can change without notice.

/// `OpenAI` request conversion to the generate wire.
mod payload;
/// Generate stream decoding.
mod stream;

use std::sync::LazyLock;

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::{
    openai::{ChatCompletionRequest, ModelListResponse},
    provider::{
        common::{
            self, proxy::retry_api_keys, request_identity::RequestIdentity,
            session::RequestSession, translation,
        },
        openai,
        registry::Provider,
    },
    responses::ResponseCreateRequest,
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Command Code API root serving the CLI endpoints.
const API_ROOT: &str = "https://api.commandcode.ai";
/// CLI release whose wire format this backend reproduces.
const CLI_VERSION: &str = "1.88.5";
/// Catalog plan tier included in the Go subscription.
const GO_PLAN: &str = "individual-go";

/// Command Code Go backend.
pub(crate) struct GoBackend;

/// Read a string array, dropping the field when any entry is not a string.
fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

impl openai::ModelEndpoint for GoBackend {
    async fn request(_provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str> {
        openai::send(
            super::BASE_URL,
            key,
            "models",
            Some("view=client"),
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }

    fn models_from_list(value: &Value) -> Vec<crate::openai::Model> {
        value
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entry| entry.get("minimumPlan").and_then(Value::as_str) == Some(GO_PLAN))
            .filter_map(|entry| {
                let mut model =
                    serde_json::from_value::<crate::openai::Model>(entry.clone()).ok()?;
                model.context_window = model.context_window.or_else(|| {
                    entry
                        .get("context_length")
                        .and_then(Value::as_u64)
                        .filter(|&len| len > 0)
                });
                model.max_output_tokens = model.max_output_tokens.or_else(|| {
                    entry
                        .get("maxOutputTokens")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                });
                if model.input_modalities.is_none() {
                    model.input_modalities = strings(entry.get("inputModalities"));
                }
                if let Some(efforts) =
                    strings(entry.get("reasoningEfforts")).filter(|efforts| !efforts.is_empty())
                {
                    model.reasoning.get_or_insert(true);
                    model.supported_reasoning_efforts.get_or_insert(efforts);
                }
                Some(model)
            })
            .filter(|model| !model.id.is_empty())
            .collect()
    }
}

/// Builds an `OpenAI` error response.
fn error(status: StatusCode, message: &str, kind: &str, code: &str) -> Response {
    (
        status,
        Json(crate::server::error_body(message, kind, Some(code))),
    )
        .into_response()
}

/// Derive a stable UUID thread from the session identity, as the endpoint requires UUIDs.
fn thread_id(session: &str) -> String {
    if let Ok(id) = uuid::Uuid::parse_str(session) {
        return id.to_string();
    }
    let mut bytes = [0_u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(Sha256::digest(session.as_bytes())) {
        *slot = byte;
    }
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// Shared client for generate requests; streams have no overall deadline.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        // Never send the API key to a redirect target.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
});

/// Send one generate request with the CLI's identifying headers.
async fn send(
    root: &str,
    key: &str,
    thread: &str,
    body: Bytes,
) -> Result<reqwest::Response, &'static str> {
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize Command Code Go provider")?;
    client
        .post(format!("{}/alpha/generate", root.trim_end_matches('/')))
        .bearer_auth(key)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "cli")
        .header("x-command-code-version", CLI_VERSION)
        .header("x-cli-environment", "production")
        .header("x-project-slug", "project")
        .header("x-taste-learning", "false")
        .header("x-session-id", thread)
        .body(body)
        .send()
        .await
        // Reqwest errors can contain URLs; never expose them.
        .map_err(|_| "Failed to contact Command Code")
}

/// Report a rejected request with Command Code's own reason, such as a plan restriction.
async fn upstream_failure(response: reqwest::Response) -> Response {
    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    let reason = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| {
            stream::error_message(&value).map(|m| m.chars().take(300).collect::<String>())
        });
    let message = reason.map_or_else(
        || format!("Command Code returned {status}"),
        |reason| format!("Command Code returned {status}: {reason}"),
    );
    if status.is_client_error() {
        error(
            status,
            &message,
            "invalid_request_error",
            "upstream_rejected",
        )
    } else {
        error(
            StatusCode::BAD_GATEWAY,
            &message,
            "upstream_error",
            "bad_gateway",
        )
    }
}

/// Authorize, translate, and stream one turn through the generate endpoint.
async fn complete(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    session: RequestSession,
    identity: Option<RequestIdentity>,
    req: ChatCompletionRequest,
    responses: bool,
) -> Response {
    if !permissions.allows_model(&req.model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        );
    }
    let thread = thread_id(&session.id);
    let body = match payload::build(&req, &thread).and_then(|body| {
        serde_json::to_vec(&body).map_err(|_| "Failed to encode request".to_owned())
    }) {
        Ok(body) => Bytes::from(body),
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                &message,
                "invalid_request_error",
                "invalid_request",
            );
        }
    };
    let root = if provider.base_url.is_empty() {
        API_ROOT
    } else {
        provider.base_url.as_str()
    };
    let response = match retry_api_keys(
        &state.settings,
        &provider,
        "No connected Command Code Go credentials",
        identity.as_ref(),
        |key| {
            let body = body.clone();
            let thread = thread.as_str();
            async move { send(root, &key, thread, body).await }
        },
    )
    .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => return upstream_failure(response).await,
        Err(error) => return error.into_response(),
    };
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(stream::consume(response, tx));
    translation::respond(
        rx,
        req.model,
        req.stream.unwrap_or(false),
        responses,
        req.stream_options
            .and_then(|options| options.include_usage)
            .unwrap_or(false),
    )
    .await
}

/// Lists Go plan models permitted by the virtual key.
async fn models(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
) -> Response {
    match openai::discover::<GoBackend>(&state.settings, &provider).await {
        Ok(mut data) => {
            data.retain(|model| permissions.allows_model(&model.id));
            Json(ModelListResponse {
                object: "list".into(),
                data,
            })
            .into_response()
        }
        Err(message) => error(
            StatusCode::BAD_GATEWAY,
            &message,
            "upstream_error",
            "bad_gateway",
        ),
    }
}

/// Handles an `OpenAI` chat completion.
async fn chat(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    complete(
        state,
        permissions,
        provider,
        session,
        identity.map(|Extension(identity)| identity),
        req,
        false,
    )
    .await
}

/// Converts a Responses request and delegates to the shared completion path.
async fn responses(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    complete(
        state,
        permissions,
        provider,
        session,
        identity.map(|Extension(identity)| identity),
        req.into_chat_request(),
        true,
    )
    .await
}

impl common::Provider for GoBackend {
    const DESCRIPTOR: common::ProviderDescriptor = common::ProviderDescriptor {
        value: "commandcode-go",
        label: None,
        oauth: false,
        api_key: true,
        fixed_id: Some("commandcode-go"),
        fixed_name: Some("Command Code Go"),
        keyless: false,
        default_base_url: Some(API_ROOT),
        legacy: common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        Router::new()
            .route("/v1/models", get(models))
            .route("/v1/models/", get(models))
            .route("/v1/chat/completions", post(chat))
            .route("/v1/chat/completions/", post(chat))
            .route("/v1/responses", post(responses))
            .route("/v1/responses/", post(responses))
            .layer(axum::middleware::from_fn_with_state(
                false,
                crate::openai::validate_translated_request,
            ))
            .layer(axum::middleware::from_fn_with_state(
                (settings.clone(), "commandcode-go"),
                settings::virtual_key_auth,
            ))
    }

    async fn discover(
        state: &AppState,
        provider: &Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        openai::discover::<Self>(&state.settings, provider).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::provider::openai::ModelEndpoint;
    use axum::body::to_bytes;
    use serde_json::json;

    /// Only Go-tier models are listed, with the CLI catalog's limits and efforts.
    #[test]
    fn discovery_lists_go_plan_models() {
        let models = GoBackend::models_from_list(&json!({"data": [
            {"id": "claude-sonnet-5-5", "minimumPlan": "individual-goat"},
            {"id": "deepseek/deepseek-v4.1-flash", "minimumPlan": "individual-go",
             "context_length": 1_000_000, "maxOutputTokens": 65_536,
             "inputModalities": ["text", "image"], "reasoningEfforts": ["off", "high"]},
            {"id": "zai-org/GLM-5.2", "minimumPlan": "individual-go", "reasoningEfforts": []}
        ]}));
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["deepseek/deepseek-v4.1-flash", "zai-org/GLM-5.2"]);
        let flash = &models[0];
        assert_eq!(flash.context_window, Some(1_000_000));
        assert_eq!(flash.max_output_tokens, Some(65_536));
        assert_eq!(
            flash.input_modalities.as_deref().unwrap(),
            ["text", "image"]
        );
        assert_eq!(flash.reasoning, Some(true));
        assert_eq!(models[1].reasoning, None);
    }

    /// Session identities map to stable UUID threads; UUIDs pass through.
    #[test]
    fn thread_ids_are_stable_uuids() {
        let derived = thread_id("hmac-session");
        assert_eq!(derived, thread_id("hmac-session"));
        assert!(uuid::Uuid::parse_str(&derived).is_ok());
        let id = "6f1c6a8e-4b1a-4f5e-9d2b-0c6f3b1a2d4e";
        assert_eq!(thread_id(id), id);
    }

    /// A generate stream split across chunks becomes one chat completion with tools and usage.
    #[tokio::test]
    async fn generate_stream_becomes_chat_completion() {
        let body = concat!(
            "{\"type\":\"start\"}\n{\"type\":\"reasoning-delta\",\"text\":\"plan\"}\n",
            "{\"type\":\"text-delta\",\"text\":\"Hel\"}\n{\"type\":\"text-del",
            "ta\",\"text\":\"lo\"}\n",
            "{\"type\":\"tool-call\",\"toolCallId\":\"c1\",\"toolName\":\"read\",\"input\":{\"p\":1}}\n",
            "{\"type\":\"finish\",\"finishReason\":\"tool-calls\",\"totalUsage\":{\"inputTokens\":5,\"outputTokens\":2}}\n",
        );
        let response = reqwest::Response::from(axum::http::Response::new(body));
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(stream::consume(response, tx));
        let response = translation::respond(rx, "m".into(), false, false, false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let message = &value["choices"][0]["message"];
        assert_eq!(message["content"], "Hello");
        assert_eq!(message["reasoning_content"], "plan");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"p\":1}"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(value["usage"]["total_tokens"], 7);
    }

    /// A truncated stream is a gateway error, not a successful empty answer.
    #[tokio::test]
    async fn truncated_stream_is_an_error() {
        let body = "{\"type\":\"text-delta\",\"text\":\"partial\"}\n";
        let response = reqwest::Response::from(axum::http::Response::new(body));
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(stream::consume(response, tx));
        let response = translation::respond(rx, "m".into(), false, false, false).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
