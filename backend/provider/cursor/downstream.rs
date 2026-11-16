use axum::{
    Extension, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};

use crate::provider::common::{
    cooldown,
    pool::{PoolError, eligible, rank_accounts},
};
use crate::{
    openai::{ChatCompletionRequest, ModelListResponse},
    provider::{common::request_identity::RequestIdentity, registry::Provider},
    responses::ResponseCreateRequest,
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Cursor's virtual-key protected OpenAI-compatible endpoints.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
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
            (settings.clone(), "cursor"),
            settings::virtual_key_auth,
        ))
}

/// Builds an OpenAI-compatible error response.
fn error(status: StatusCode, message: &str, kind: &str, code: &str) -> Response {
    (
        status,
        Json(crate::server::error_body(message, kind, Some(code))),
    )
        .into_response()
}

/// Removes the optional Cursor provider prefix.
fn strip_prefix(model: &str) -> &str {
    model.strip_prefix("cursor/").unwrap_or(model)
}

/// Lists discovered models permitted by the virtual key.
async fn models(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    match super::models::fetch_catalog_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(mut data) => {
            data.retain(|model| permissions.allows_model(strip_prefix(&model.id)));
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
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    Box::pin(complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req,
        false,
    ))
    .await
}

/// Converts a Responses request and delegates to the shared completion path.
async fn responses(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    Box::pin(complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req.into_chat_request(),
        true,
    ))
    .await
}

/// Authorizes the model and tries eligible Cursor credentials.
#[allow(clippy::too_many_lines)]
async fn complete(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    identity: Option<RequestIdentity>,
    mut req: ChatCompletionRequest,
    responses: bool,
) -> Response {
    req.model = strip_prefix(&req.model).to_string();
    if !permissions.allows_model(&req.model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        );
    }
    // Validate before opening a stream or attempting credentials.
    let mut replay = match super::replay::build(&req) {
        Ok(replay) => replay,
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                &message,
                "invalid_request_error",
                "invalid_request",
            );
        }
    };
    let credentials = match eligible(&state.settings, &provider).await {
        Ok(credentials) => credentials,
        Err(PoolError::Cooling(wait)) => {
            let mut response = error(
                StatusCode::TOO_MANY_REQUESTS,
                "All Cursor accounts are rate limited",
                "rate_limit_error",
                "rate_limited",
            );
            if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            return response;
        }
        Err(PoolError::Internal(message)) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &message,
                "server_error",
                "internal_error",
            );
        }
        Err(other) => {
            return other.into_response("All Cursor accounts are rate limited", false);
        }
    };
    let mut discovered = Vec::with_capacity(credentials.len());
    for credential in credentials {
        let token = super::auth::access_token(&state.settings, &provider.id, &credential.id).await;
        let catalog = match &token {
            Ok(token) => super::models::fetch_account_models(&provider, &credential.id, token)
                .await
                .map_err(|failure| failure.message),
            Err(message) => Err(message.clone()),
        };
        discovered.push(((credential, token), catalog));
    }
    let candidates = rank_accounts(discovered, &req.model);
    let mut last =
        "Cursor is not configured. Authenticate on the dashboard Providers page.".to_string();
    let mut headers = reqwest::header::HeaderMap::new();
    let mut limited = false;
    for (credential, token) in candidates {
        let token = match token {
            Ok(token) => token,
            Err(message) => {
                headers.clear();
                limited = false;
                last = message;
                continue;
            }
        };
        match super::upstream::start(&provider.base_url, &token, replay).await {
            Ok(rx) => {
                let response = crate::provider::common::translation::respond(
                    rx,
                    req.model,
                    req.stream.unwrap_or(false),
                    responses,
                    req.stream_options
                        .as_ref()
                        .and_then(|options| options.include_usage)
                        .unwrap_or(false),
                )
                .await;
                if response.status().is_success() {
                    cooldown::clear(&credential.id);
                }
                if response.status().is_success()
                    && let Some(identity) = &identity
                {
                    identity.set_credential(&credential.id, &credential.name);
                }
                return response;
            }
            Err(mut failure) => {
                super::models::invalidate_catalog_on_auth_error(&credential.id, failure.status)
                    .await;
                let retry = failure.retryable();
                limited = failure.status == Some(StatusCode::TOO_MANY_REQUESTS);
                if limited {
                    let wait = cooldown::wait_for(&failure.headers);
                    cooldown::start(&credential.id, wait);
                    if !failure
                        .headers
                        .contains_key(axum::http::header::RETRY_AFTER)
                    {
                        let value = cooldown::retry_after_secs(wait).to_string();
                        if let Ok(value) = value.parse() {
                            failure
                                .headers
                                .insert(axum::http::header::RETRY_AFTER, value);
                        }
                    }
                }
                headers = *failure.headers;
                last = failure.message;
                if !retry {
                    break;
                }
                replay = match super::replay::build(&req) {
                    Ok(replay) => replay,
                    Err(message) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            &message,
                            "invalid_request_error",
                            "invalid_request",
                        );
                    }
                };
            }
        }
    }
    let (status, kind, code) = if limited {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate_limited",
        )
    } else {
        (StatusCode::BAD_GATEWAY, "upstream_error", "bad_gateway")
    };
    let mut response = error(status, &last, kind, code);
    cooldown::copy_rate_limit_headers(&headers, response.headers_mut());
    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::State as AxumState,
        http::{HeaderMap, Request},
        routing::post,
    };
    use prost::Message;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    use crate::provider::cursor::proto;
    use crate::storage::{ProviderCredential, ProviderRecord, Secret, VirtualKey};

    #[derive(Clone, PartialEq, Message)]
    struct UsableResponse {
        #[prost(message, repeated, tag = "1")]
        models: Vec<UsableModel>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct UsableModel {
        #[prost(string, tag = "1")]
        model_id: String,
        #[prost(string, tag = "2")]
        display_name: String,
    }

    fn connect_frame(data: &[u8]) -> Vec<u8> {
        let len = u32::try_from(data.len()).unwrap();
        let mut out = Vec::with_capacity(data.len().saturating_add(5));
        out.push(0);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    fn auth_token(headers: &HeaderMap) -> String {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_string()
    }

    async fn save_cursor_account(store: &SettingsStore, id: &str, name: &str, token: &str) {
        let mut db = (*store.database.db).clone();
        toasty::create!(ProviderCredential {
            id: id.to_string(),
            provider_id: "cursor".to_string(),
            name: name.to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        let secret_name = format!("credential.{id}.credential");
        let payload = json!({
            "access_token": token,
            "refresh_token": "refresh",
            "expires_at": i64::MAX / 2,
        })
        .to_string();
        Secret::upsert_by_name(&secret_name)
            .value(store.encrypt(&secret_name, &payload).unwrap())
            .exec(&mut db)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn routes_to_advertising_account_aggregates_models_and_allows_unlisted() {
        let used_tokens: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mock_tokens = used_tokens.clone();
        let fake_upstream =
            Router::new()
                .route(
                    "/agent.v1.AgentService/GetUsableModels",
                    post(|headers: HeaderMap| async move {
                        let models = match auth_token(&headers).as_str() {
                            "token-a" => vec![UsableModel {
                                model_id: "model-x".into(),
                                display_name: "Model X".into(),
                            }],
                            "token-b" => vec![UsableModel {
                                model_id: "model-y".into(),
                                display_name: "Model Y".into(),
                            }],
                            _ => Vec::new(),
                        };
                        UsableResponse { models }.encode_to_vec()
                    }),
                )
                .route(
                    "/aiserver.v1.AiService/AvailableModels",
                    post(|| async { StatusCode::NOT_FOUND }),
                )
                .route(
                    "/agent.v1.AgentService/RunSSE",
                    post(
                        |AxumState(tokens): AxumState<Arc<Mutex<Vec<String>>>>,
                         headers: HeaderMap| async move {
                            tokens.lock().unwrap().push(auth_token(&headers));
                            let update = proto::AgentServerMessage {
                                message: Some(
                                    proto::agent_server_message::Message::InteractionUpdate(
                                        proto::InteractionUpdate {
                                            message: Some(
                                                proto::interaction_update::Message::TextDelta(
                                                    proto::TextDeltaUpdate { text: "ok".into() },
                                                ),
                                            ),
                                        },
                                    ),
                                ),
                            };
                            let mut body = connect_frame(&update.encode_to_vec());
                            body.extend_from_slice(&[2, 0, 0, 0, 2]);
                            body.extend_from_slice(b"{}");
                            (StatusCode::OK, body)
                        },
                    ),
                )
                .route(
                    "/aiserver.v1.BidiService/BidiAppend",
                    post(|| async { StatusCode::OK }),
                )
                .with_state(mock_tokens);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server =
            tokio::spawn(async move { axum::serve(listener, fake_upstream).await.unwrap() });

        let database = crate::database::Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*database.db).clone();
        toasty::create!(ProviderRecord {
            id: "cursor".to_string(),
            name: "Cursor".to_string(),
            kind: "cursor".to_string(),
            base_url: base_url.clone(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        save_cursor_account(&store, "cursor-acc-a", "Account A", "token-a").await;
        save_cursor_account(&store, "cursor-acc-b", "Account B", "token-b").await;

        let key = "test-cursor-pool-key";
        let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        toasty::create!(VirtualKey {
            id: "vk-cursor".to_string(),
            name: "test".to_string(),
            key_hash: digest.as_ref().to_vec(),
            providers: "[]".to_string(),
            models: "[]".to_string(),
            created_at: 0_i64,
            expires_at: None,
            revoked_at: None
        })
        .exec(&mut db)
        .await
        .unwrap();

        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings: store.clone(),
            database,
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let app = Router::new()
            .nest("/cursor/{id}", router(&store))
            .with_state(state);

        // /v1/models aggregates both accounts' catalogs.
        let models_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/cursor/cursor/v1/models")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models_resp.status(), StatusCode::OK);
        let models_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(models_resp.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = models_json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"model-x"));
        assert!(ids.contains(&"model-y"));

        // Request for model-y routes to Account B even though Account A was registered first.
        let resp_y = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/cursor/cursor/v1/chat/completions")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "cursor/model-y",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_y.status(), StatusCode::OK);
        assert_eq!(used_tokens.lock().unwrap().as_slice(), &["token-b"]);

        // Request for unlisted model-z still reaches the pool.
        used_tokens.lock().unwrap().clear();
        let resp_z = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/cursor/cursor/v1/chat/completions")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "model-z",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_z.status(), StatusCode::OK);
        assert_eq!(used_tokens.lock().unwrap().len(), 1);

        server.abort();
    }
}
