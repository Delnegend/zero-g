use std::future::Future;

use axum::Extension;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};

use crate::openai::{ChatCompletionRequest, Model, ModelListResponse};
use crate::provider::codex::CodexAuthManager;
use crate::provider::codex::auth::Credentials;
use crate::provider::codex::models::fetch_account_models;
use crate::provider::common::cooldown;
use crate::provider::common::errors::UpstreamError;
use crate::provider::common::pool::{PoolError, eligible, rank_accounts, run_candidates};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::common::session::RequestSession;
use crate::provider::credentials::Credential;
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
            true,
            crate::openai::validate_translated_request,
        ))
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

/// Removes a recognized Codex provider prefix from a model identifier.
fn strip_codex_prefix(model: &str) -> String {
    model
        .strip_prefix("openai-codex/")
        .or_else(|| model.strip_prefix("codex/"))
        .unwrap_or(model)
        .to_string()
}

/// Message returned when no Codex account is configured.
const NOT_CONFIGURED: &str =
    "OpenAI Codex is not configured. Authenticate on the dashboard Providers page.";
/// Message returned when every Codex account is cooling down.
const RATE_LIMITED: &str = "All Codex accounts are rate limited";

/// Executes an operation across the supplied weighted Codex credentials.
async fn with_candidates<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    credentials: Vec<Credential>,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(String, Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    run_candidates(
        credentials,
        identity,
        NOT_CONFIGURED,
        |credential| {
            let id = credential.id.clone();
            async move {
                CodexAuthManager::credentials(&state.settings, &provider.id, &id)
                    .await
                    .map_err(PoolError::from_credential)
                    .and_then(|creds| {
                        creds.ok_or_else(|| PoolError::Internal(NOT_CONFIGURED.into()))
                    })
                    .map(|creds| Some((id, creds)))
            }
        },
        |(credential_id, creds)| operation(credential_id, creds),
    )
    .await
}

/// Discovers each eligible account's catalog independently, retaining weighted order.
async fn account_models(
    state: &AppState,
    provider: &Provider,
    credentials: &[Credential],
) -> Vec<Result<Vec<Model>, PoolError>> {
    let mut catalogs = Vec::with_capacity(credentials.len());
    for credential in credentials {
        catalogs.push(
            with_candidates(
                state,
                provider,
                None,
                vec![credential.clone()],
                |credential_id, creds| async move {
                    fetch_account_models(
                        &state.providers.codex_client,
                        Some(&credential_id),
                        &creds,
                    )
                    .await
                },
            )
            .await,
        );
    }
    catalogs
}

/// Aggregates normalized models across all eligible Codex accounts, deduplicating by ID.
pub(super) async fn aggregate_models(
    state: &AppState,
    provider: &Provider,
) -> Result<ModelListResponse, PoolError> {
    let credentials = eligible(&state.settings, provider).await?;
    let catalogs = account_models(state, provider, &credentials).await;
    let mut models: Vec<Model> = Vec::new();
    let mut first_error = None;
    let mut succeeded = false;
    for catalog in catalogs {
        match catalog {
            Ok(account_models) => {
                succeeded = true;
                for model in account_models {
                    if !models.iter().any(|existing| existing.id == model.id) {
                        models.push(model);
                    }
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if succeeded {
        Ok(ModelListResponse {
            object: "list".to_string(),
            data: models,
        })
    } else {
        Err(first_error.unwrap_or_else(|| PoolError::Internal(NOT_CONFIGURED.into())))
    }
}

/// Runs `operation` on accounts advertising `model` (plus failed lookups), or all accounts if
/// none advertise it.
pub(super) async fn with_model_pool<T, F, Fut>(
    state: &AppState,
    provider: &Provider,
    identity: Option<&RequestIdentity>,
    model: &str,
    operation: F,
) -> Result<T, PoolError>
where
    F: Fn(String, Credentials) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, UpstreamError>> + Send,
{
    let credentials = eligible(&state.settings, provider).await?;
    let catalogs = account_models(state, provider, &credentials).await;
    let mut earliest = None;
    let eligible_accounts: Vec<_> = credentials
        .into_iter()
        .zip(catalogs)
        .filter(|(credential, _)| {
            cooldown::remaining(&credential.id).is_none_or(|wait| {
                earliest = Some(earliest.map_or(wait, |old: std::time::Duration| old.min(wait)));
                false
            })
        })
        .collect();
    if eligible_accounts.is_empty()
        && let Some(wait) = earliest
    {
        return Err(PoolError::Cooling(wait));
    }
    let candidates = rank_accounts(eligible_accounts, model);
    with_candidates(state, provider, identity, candidates, operation).await
}

/// Fetches models and filters them according to virtual-key permissions.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
) -> Response {
    match aggregate_models(&state, &provider).await {
        Ok(mut models) => {
            models
                .data
                .retain(|model| permissions.allows_model(&strip_codex_prefix(&model.id)));
            Json(models).into_response()
        }
        Err(e) => e.into_response(RATE_LIMITED, true),
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
    // Codex upstream does not honor OpenAI output-token limits; the payload
    // builder already omits them. Accept and ignore for client compatibility.
    let client = &state.providers.codex_client;
    let req = &req;
    let session = &session.id;
    match with_model_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
        &req.model,
        |credential_id, creds| async move {
            if req.stream.unwrap_or(false) {
                client
                    .stream_chat_for(Some(&credential_id), &creds, req, session)
                    .await
            } else {
                client
                    .complete_chat_non_streaming_for(Some(&credential_id), &creds, req, session)
                    .await
                    .map(|r| Json(r).into_response())
            }
        },
    )
    .await
    {
        Ok(response) => response,
        Err(e) => e.into_response(RATE_LIMITED, true),
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
    // Codex upstream does not honor OpenAI output-token limits; the payload
    // builder already omits them. Accept and ignore for client compatibility.
    let client = &state.providers.codex_client;
    let req = &req;
    let session = &session.id;
    match with_model_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
        &req.model,
        |credential_id, creds| async move {
            if req.stream.unwrap_or(false) {
                client
                    .stream_responses_for(Some(&credential_id), &creds, req, session)
                    .await
            } else {
                client
                    .complete_responses_non_streaming_for(
                        Some(&credential_id),
                        &creds,
                        req,
                        session,
                    )
                    .await
                    .map(|r| Json(r).into_response())
            }
        },
    )
    .await
    {
        Ok(response) => response,
        Err(e) => e.into_response(RATE_LIMITED, true),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::{ProviderCredential, ProviderRecord, Secret};
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn try_credentials<C, T, F, Fut>(candidates: Vec<C>, attempt: F) -> Result<T, PoolError>
    where
        F: FnMut(C) -> Fut,
        Fut: Future<Output = Result<T, PoolError>>,
    {
        crate::provider::common::pool::try_candidates(candidates, NOT_CONFIGURED, attempt).await
    }

    #[test]
    fn cooldown_and_upstream_responses_keep_retry_hints() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("retry-after", "17".parse().unwrap());
        let response = PoolError::Cooling(std::time::Duration::from_millis(1200))
            .into_response(RATE_LIMITED, true);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "2");
        let response = PoolError::Upstream(UpstreamError {
            status: Some(StatusCode::TOO_MANY_REQUESTS),
            message: "limited".into(),
            headers: Box::new(headers),
        })
        .into_response(RATE_LIMITED, true);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "17");
    }

    #[tokio::test]
    async fn exhausted_pool_returns_last_upstream_status() {
        let mut attempts = Vec::new();
        let result: Result<(), _> = try_credentials(
            vec![StatusCode::UNAUTHORIZED, StatusCode::TOO_MANY_REQUESTS],
            |status| {
                attempts.push(status);
                async move {
                    Err(PoolError::Upstream(UpstreamError {
                        status: Some(status),
                        message: "upstream failure".into(),
                        headers: Box::default(),
                    }))
                }
            },
        )
        .await;
        assert_eq!(
            attempts,
            [StatusCode::UNAUTHORIZED, StatusCode::TOO_MANY_REQUESTS]
        );
        let response = result.err().unwrap().into_response(RATE_LIMITED, true);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["message"], "upstream failure");
        assert_eq!(
            PoolError::Upstream(UpstreamError {
                status: None,
                headers: Box::default(),
                message: "network".into()
            })
            .into_response(RATE_LIMITED, true)
            .status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn pool_retries_only_upstream_credential_statuses() {
        for status in [401, 403, 429] {
            let mut attempts = Vec::new();
            let result = try_credentials(vec![0, 1], |id| {
                attempts.push(id);
                async move {
                    if id == 0 {
                        Err(PoolError::Upstream(UpstreamError {
                            status: Some(StatusCode::from_u16(status).unwrap()),
                            message: "upstream".into(),
                            headers: Box::default(),
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
            let result: Result<(), _> = try_credentials(vec![0, 1], |id| {
                attempts.push(id);
                async move {
                    Err(PoolError::Upstream(UpstreamError {
                        status,
                        message: "failure".into(),
                        headers: Box::default(),
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
            let result = try_credentials(vec![0, 1], |id| {
                attempts.push(id);
                async move {
                    if id == 0 {
                        Err(PoolError::from_credential(
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
        let result: Result<(), _> = try_credentials(vec![0, 1], |id| {
            attempts.push(id);
            async move { Err(PoolError::Internal("local 401".into())) }
        })
        .await;
        assert_eq!(result.err().unwrap().to_string(), "local 401");
        assert_eq!(attempts, [0]);

        let result: Result<(), _> = try_credentials(vec![0, 1], |id| async move {
            Err(PoolError::Upstream(UpstreamError {
                status: Some(StatusCode::TOO_MANY_REQUESTS),
                message: if id == 0 {
                    "first failure"
                } else {
                    "last failure"
                }
                .into(),
                headers: Box::default(),
            }))
        })
        .await;
        assert_eq!(result.err().unwrap().to_string(), "last failure");
        let result: Result<(), _> = try_credentials(Vec::<u8>::new(), |_| async { Ok(()) }).await;
        assert!(matches!(result.err().unwrap(), PoolError::Internal(_)));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn multi_account_routes_select_advertising_account_and_aggregate_models() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
        let fake = Router::new()
            .route(
                "/codex/models",
                get(move |headers: axum::http::HeaderMap| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default();
                    let slug = if auth == "Bearer tok-b" {
                        "model-y"
                    } else {
                        "model-x"
                    };
                    Json(json!({
                        "models": [{
                            "slug": slug,
                            "display_name": slug,
                            "visibility": "list"
                        }]
                    }))
                }),
            )
            .route(
                "/codex/responses",
                post(
                    move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                        let tx = tx.clone();
                        async move {
                            let auth = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_string();
                            let model = body["model"].as_str().unwrap_or_default().to_string();
                            tx.send((auth, model)).unwrap();
                            let sse = concat!(
                                "event: response.output_text.delta\n",
                                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                                "event: response.completed\n",
                                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
                            );
                            ([("content-type", "text/event-stream")], sse)
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            kind: "codex".to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        for (id, token, weight) in [("acc-a", "tok-a", 100), ("acc-b", "tok-b", 1)] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: "codex".to_string(),
                name: id.to_string(),
                weight,
                enabled: true
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
            let name = format!("credential.{id}.credential");
            let secret = json!({"access_token": token}).to_string();
            Secret::upsert_by_name(&name)
                .value(store.encrypt(&name, &secret).unwrap())
                .exec(&mut (*store.database.db).clone())
                .await
                .unwrap();
        }

        let mut client = crate::provider::codex::CodexClient::new();
        client.set_base_url_for_test(format!("http://{address}"));
        let mut providers = crate::provider::Providers::new();
        providers.codex_client = Arc::new(client);
        let state = AppState {
            providers,
            settings: store,
            database,
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let app = Router::new()
            .route("/v1/models", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .route("/v1/responses", post(responses_handler))
            .layer(Extension(Permissions {
                providers: vec![],
                models: vec![],
            }))
            .layer(Extension(Provider {
                id: "codex".into(),
                name: "Codex".into(),
                kind: "codex".into(),
                base_url: String::new(),
                keyless: false,
            }))
            .layer(Extension(RequestSession {
                id: "session".into(),
            }))
            .with_state(state);

        // /v1/models aggregates across both accounts.
        let models_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models_resp.status(), StatusCode::OK);
        let models_bytes = axum::body::to_bytes(models_resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let models_json: Value = serde_json::from_slice(&models_bytes).unwrap();
        let ids: Vec<&str> = models_json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["model-x", "model-y"]);

        // Request for model-y routes to acc-b even though acc-a has higher weight.
        for (path, body) in [
            (
                "/v1/chat/completions",
                json!({"model": "codex/model-y", "messages": [{"role": "user", "content": "hi"}], "stream": false}),
            ),
            (
                "/v1/responses",
                json!({"model": "model-y", "input": "hi", "stream": false}),
            ),
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                rx.recv().await.unwrap(),
                ("Bearer tok-b".to_string(), "model-y".to_string())
            );
        }

        // Unlisted model-z still reaches the pool (starting at highest-weighted acc-a).
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model": "model-z", "messages": [{"role": "user", "content": "hi"}], "stream": false})
                    .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            rx.recv().await.unwrap(),
            ("Bearer tok-a".to_string(), "model-z".to_string())
        );

        server.abort();
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
