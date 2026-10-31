use std::future::Future;

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};

use crate::openai::ChatCompletionRequest;
use crate::provider::codex::CodexAuthManager;
use crate::provider::codex::auth::CodexCredentials;
use crate::provider::codex::models::fetch_codex_models;
use crate::provider::common::errors::{UpstreamError, retryable_status};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::session::RequestSession;
use crate::provider::registry::Provider;
use crate::responses::ResponseCreateRequest;
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

/// Builds the Codex API routes with virtual-key authentication.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "codex"),
            settings::virtual_key_auth,
        ))
}

/// Returns the virtual-key model permission error.
fn model_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(crate::server::error_body(
            "Model is not allowed for this virtual key",
            "permission_error",
            Some("model_not_allowed"),
        )),
    )
        .into_response()
}

/// Reports output limits unsupported by the `ChatGPT` Codex endpoint.
fn unsupported_output_limit(field: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(crate::server::error_body(
            &format!("{field} is not supported by the Codex upstream"),
            "invalid_request_error",
            Some("unsupported_parameter"),
        )),
    )
        .into_response()
}

/// Removes a recognized Codex provider prefix from a model identifier.
fn strip_codex_prefix(model: &str) -> String {
    model
        .strip_prefix("openai-codex/")
        .or_else(|| model.strip_prefix("codex/"))
        .unwrap_or(model)
        .to_string()
}

/// Describes a credential-pool failure and how it should be handled.
pub(crate) struct PoolError {
    /// Error message returned to the caller.
    pub(crate) message: String,
    /// Whether the failure originated locally rather than upstream.
    local: bool,
    /// HTTP status returned by the upstream, if one was received.
    status: Option<StatusCode>,
    /// Whether another credential may be tried.
    retryable: bool,
}

impl PoolError {
    /// Creates a non-retryable local error.
    fn local(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            local: true,
            status: None,
            retryable: false,
        }
    }

    /// Converts an upstream error, preserving its retryability.
    fn upstream(error: UpstreamError) -> Self {
        Self {
            retryable: error.retryable(),
            message: error.message,
            status: error.status,
            local: false,
        }
    }

    /// Converts a credential error, deriving retryability from its upstream status.
    fn credential(error: crate::provider::login::CredentialError) -> Self {
        let retryable = retryable_status(error.upstream_status);
        Self {
            message: error.message,
            local: error.upstream_status.is_none(),
            status: error.upstream_status,
            retryable,
        }
    }

    /// Converts the failure into a local or upstream error response.
    fn response(self) -> Response {
        let (status, kind, code) = if self.local {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "internal_error",
            )
        } else {
            (
                self.status.unwrap_or(StatusCode::BAD_GATEWAY),
                "upstream_error",
                "bad_gateway",
            )
        };
        (
            status,
            Json(crate::server::error_body(&self.message, kind, Some(code))),
        )
            .into_response()
    }
}

/// Runs a request against the provider's credentials, retrying eligible failures.
pub(crate) async fn with_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    request: F,
) -> Result<T, PoolError>
where
    F: Fn(CodexCredentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id)
        .await
        .map_err(PoolError::local)?;
    try_credentials(credentials, |credential| {
        let request = &request;
        async move {
            let auth = CodexAuthManager::load(state.settings.clone(), &provider.id, &credential.id)
                .await.map_err(PoolError::local)?
                .ok_or_else(|| PoolError::local("OpenAI Codex is not configured. Authenticate on the dashboard Providers page."))?;
            let creds = auth.get_credentials().await.map_err(PoolError::credential)?;
            let result = request(creds).await.map_err(PoolError::upstream)?;
            if let Some(identity) = identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            Ok(result)
        }
    }).await
}

/// Tries credentials in order until one succeeds or a non-retryable error occurs.
async fn try_credentials<I, T, F, Fut>(credentials: I, mut request: F) -> Result<T, PoolError>
where
    I: IntoIterator,
    F: FnMut(I::Item) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    let mut last = None;
    for credential in credentials {
        match request(credential).await {
            Ok(value) => return Ok(value),
            Err(e) if e.retryable => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        PoolError::local(
            "OpenAI Codex is not configured. Authenticate on the dashboard Providers page.",
        )
    }))
}

/// Fetches models and filters them according to virtual-key permissions.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    match with_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
        |creds| async move { fetch_codex_models(&creds).await },
    )
    .await
    {
        Ok(mut models) => {
            models
                .data
                .retain(|model| permissions.allows_model(&strip_codex_prefix(&model.id)));
            Json(models).into_response()
        }
        Err(e) => e.response(),
    }
}

/// Authorizes and forwards a chat completion request through the credential pool.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
    req.model = strip_codex_prefix(&req.model);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }
    if req.max_completion_tokens.is_some() {
        return unsupported_output_limit("max_completion_tokens");
    }
    if req.max_tokens.is_some() {
        return unsupported_output_limit("max_tokens");
    }
    let client = &state.providers.codex_client;
    let req = &req;
    let session = &session.id;
    match with_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
        |creds| async move {
            if req.stream.unwrap_or(false) {
                client.stream_chat(&creds, req, session).await
            } else {
                client
                    .complete_chat_non_streaming(&creds, req, session)
                    .await
                    .map(|r| Json(r).into_response())
            }
        },
    )
    .await
    {
        Ok(response) => response,
        Err(e) => e.response(),
    }
}

/// Authorizes and forwards a responses request through the credential pool.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(session): Extension<RequestSession>,
    identity: Option<Extension<RequestIdentity>>,
    Json(mut req): Json<ResponseCreateRequest>,
) -> Response {
    req.model = strip_codex_prefix(&req.model);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }
    if req.max_output_tokens.is_some() {
        return unsupported_output_limit("max_output_tokens");
    }
    let client = &state.providers.codex_client;
    let req = &req;
    let session = &session.id;
    match with_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
        |creds| async move {
            if req.stream.unwrap_or(false) {
                client.stream_responses(&creds, req, session).await
            } else {
                client
                    .complete_responses_non_streaming(&creds, req, session)
                    .await
                    .map(|r| Json(r).into_response())
            }
        },
    )
    .await
    {
        Ok(response) => response,
        Err(e) => e.response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_limits_are_rejected_for_both_api_modes() {
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let settings = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings,
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let permissions = Permissions {
            providers: vec![],
            models: vec![],
        };
        let provider = Provider {
            id: "codex".into(),
            name: "Codex".into(),
            kind: "codex".into(),
            base_url: String::new(),
        };
        let session = RequestSession { id: "test".into() };
        for stream in [false, true] {
            for field in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
                let mut input = if field == "max_output_tokens" {
                    serde_json::json!({"model":"test","input":"hello","stream":stream})
                } else {
                    serde_json::json!({"model":"test","messages":[{"role":"user","content":"hello"}],"stream":stream})
                };
                input[field] = serde_json::json!(10);
                let response = if field == "max_output_tokens" {
                    responses_handler(
                        State(state.clone()),
                        Extension(permissions.clone()),
                        Extension(provider.clone()),
                        Extension(session.clone()),
                        None,
                        Json(serde_json::from_value(input).unwrap()),
                    )
                    .await
                } else {
                    chat_handler(
                        State(state.clone()),
                        Extension(permissions.clone()),
                        Extension(provider.clone()),
                        Extension(session.clone()),
                        None,
                        Json(serde_json::from_value(input).unwrap()),
                    )
                    .await
                };
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
                let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["error"]["code"], "unsupported_parameter");
                assert!(body["error"]["message"].as_str().unwrap().contains(field));
            }
        }
    }

    #[tokio::test]
    async fn exhausted_pool_returns_last_upstream_status() {
        let mut attempts = Vec::new();
        let result: Result<(), _> = try_credentials(
            [StatusCode::UNAUTHORIZED, StatusCode::TOO_MANY_REQUESTS],
            |status| {
                attempts.push(status);
                async move {
                    Err(PoolError::upstream(UpstreamError {
                        status: Some(status),
                        message: "upstream failure".into(),
                    }))
                }
            },
        )
        .await;
        assert_eq!(
            attempts,
            [StatusCode::UNAUTHORIZED, StatusCode::TOO_MANY_REQUESTS]
        );
        let response = result.err().unwrap().response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["message"], "upstream failure");
        assert_eq!(
            PoolError::upstream(UpstreamError {
                status: None,
                message: "network".into()
            })
            .response()
            .status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn pool_retries_only_upstream_credential_statuses() {
        for status in [401, 403, 429] {
            let mut attempts = Vec::new();
            let result = try_credentials([0, 1], |id| {
                attempts.push(id);
                async move {
                    if id == 0 {
                        Err(PoolError::upstream(UpstreamError {
                            status: Some(StatusCode::from_u16(status).unwrap()),
                            message: "upstream".into(),
                        }))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;
            assert!(result.is_ok());
            assert_eq!(attempts, [0, 1]);
        }
        for status in [
            None,
            Some(StatusCode::BAD_REQUEST),
            Some(StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let mut attempts = Vec::new();
            let result: Result<(), _> = try_credentials([0, 1], |id| {
                attempts.push(id);
                async move {
                    Err(PoolError::upstream(UpstreamError {
                        status,
                        message: "failure".into(),
                    }))
                }
            })
            .await;
            assert!(result.is_err());
            assert_eq!(attempts, [0]);
        }
    }

    #[tokio::test]
    async fn refresh_status_drives_failover_but_local_errors_do_not() {
        for status in [
            Some(StatusCode::UNAUTHORIZED),
            Some(StatusCode::FORBIDDEN),
            Some(StatusCode::TOO_MANY_REQUESTS),
            None,
            Some(StatusCode::BAD_REQUEST),
        ] {
            let mut attempts = Vec::new();
            let result = try_credentials([0, 1], |id| {
                attempts.push(id);
                async move {
                    if id == 0 {
                        Err(PoolError::credential(
                            crate::provider::login::CredentialError {
                                message: "refresh error".into(),
                                upstream_status: status,
                            },
                        ))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;
            if matches!(
                status,
                Some(
                    StatusCode::UNAUTHORIZED
                        | StatusCode::FORBIDDEN
                        | StatusCode::TOO_MANY_REQUESTS
                )
            ) {
                assert!(result.is_ok());
                assert_eq!(attempts, [0, 1]);
            } else {
                assert!(result.is_err());
                assert_eq!(attempts, [0]);
            }
        }
    }

    #[tokio::test]
    async fn pool_stops_on_local_failure_and_returns_last_exhausted_error() {
        let mut attempts = Vec::new();
        let result: Result<(), _> = try_credentials([0, 1], |id| {
            attempts.push(id);
            async move { Err(PoolError::local("local 401")) }
        })
        .await;
        assert_eq!(result.err().unwrap().message, "local 401");
        assert_eq!(attempts, [0]);

        let result: Result<(), _> = try_credentials([0, 1], |id| async move {
            Err(PoolError::upstream(UpstreamError {
                status: Some(StatusCode::TOO_MANY_REQUESTS),
                message: if id == 0 {
                    "first failure"
                } else {
                    "last failure"
                }
                .into(),
            }))
        })
        .await;
        assert_eq!(result.err().unwrap().message, "last failure");
        let result: Result<(), _> = try_credentials(Vec::<u8>::new(), |_| async { Ok(()) }).await;
        assert!(result.err().unwrap().local);
    }

    #[test]
    fn policy_checks_effective_model_for_all_aliases() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["gpt-5.5".into()],
        };
        for model in ["gpt-5.5", "codex/gpt-5.5", "openai-codex/gpt-5.5"] {
            assert!(permissions.allows_model(&strip_codex_prefix(model)));
        }
        assert!(!permissions.allows_model(&strip_codex_prefix("codex/gpt-5.6")));
    }

    #[test]
    fn test_strip_codex_prefix() {
        assert_eq!(strip_codex_prefix("openai-codex/gpt-5.5"), "gpt-5.5");
        assert_eq!(strip_codex_prefix("codex/gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(strip_codex_prefix("gpt-5.5"), "gpt-5.5");
    }
}
