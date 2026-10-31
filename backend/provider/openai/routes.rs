use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method};
#[cfg(test)]
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
use crate::provider::common::proxy::filter_models;
use crate::provider::common::proxy::{ProxyError, check_model, model_response, retry_api_keys};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::openai;
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};

/// Build authenticated routes for an OpenAI-compatible provider.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    router_for(settings, "openai", None)
}

/// Optional fixed upstream endpoint for a compatible provider.
#[derive(Clone)]
struct FixedBase(Option<std::sync::Arc<str>>);

/// Build routes for an OpenAI-compatible provider kind.
pub(crate) fn router_for(
    settings: &SettingsStore,
    kind: &'static str,
    fixed_base: Option<&str>,
) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(Extension(FixedBase(fixed_base.map(std::sync::Arc::from))))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), kind),
            settings::virtual_key_auth,
        ))
}

/// Resolve credentials and send a request upstream.
#[allow(clippy::too_many_arguments)]
async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    fixed_base: Option<&str>,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
    identity: Option<&RequestIdentity>,
) -> Result<reqwest::Response, ProxyError> {
    retry_api_keys(
        settings,
        &provider.id,
        "No connected OpenAI-compatible credentials",
        identity,
        |key| {
            let method = method.clone();
            let body = body.clone();
            async move {
                openai::send(
                    fixed_base.unwrap_or(&provider.base_url),
                    &key,
                    path,
                    query,
                    method,
                    headers,
                    body,
                )
                .await
            }
        },
    )
    .await
}

/// Fetch and filter models allowed by the virtual key.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(FixedBase(fixed_base)): Extension<FixedBase>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let response = match send(
        &state.settings,
        &provider,
        fixed_base.as_deref(),
        "models",
        query.as_deref(),
        Method::GET,
        &headers,
        None,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(response) => response,
        Err(response) => return response.into_response(),
    };
    model_response(response, &permissions).await
}

/// Forward an inference request after checking model permissions.
#[allow(clippy::too_many_arguments)]
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    fixed_base: Option<&str>,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
    identity: Option<&RequestIdentity>,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    match send(
        &state.settings,
        &provider,
        fixed_base,
        path,
        query.as_deref(),
        Method::POST,
        &headers,
        Some(body),
        identity,
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(response) => response.into_response(),
    }
}

/// Handle chat completion requests.
#[allow(clippy::too_many_arguments)]
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(FixedBase(fixed_base)): Extension<FixedBase>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        fixed_base.as_deref(),
        query,
        headers,
        body,
        "chat/completions",
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
}

/// Handle responses API requests.
#[allow(clippy::too_many_arguments)]
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(FixedBase(fixed_base)): Extension<FixedBase>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        fixed_base.as_deref(),
        query,
        headers,
        body,
        "responses",
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_checks_model_for_streaming_and_non_streaming_requests() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        for stream in [true, false] {
            assert!(
                check_model(
                    json!({"model":"allowed", "stream":stream})
                        .to_string()
                        .as_bytes(),
                    &permissions
                )
                .is_ok()
            );
            assert_eq!(
                check_model(
                    json!({"model":"other", "stream":stream})
                        .to_string()
                        .as_bytes(),
                    &permissions
                )
                .unwrap_err()
                .into_response()
                .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            check_model(br#"{"stream":true}"#, &permissions)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn model_listing_preserves_metadata_and_filters_unknown_models() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        let mut value = json!({"object":"list", "data":[{"id":"allowed","extra":1},{"id":"other"},{"name":"missing"}], "next":"page"});
        assert!(filter_models(&mut value, &permissions));
        assert_eq!(
            value,
            json!({"object":"list", "data":[{"id":"allowed","extra":1}], "next":"page"})
        );
        assert!(!filter_models(&mut json!({"unexpected":[]}), &permissions));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    async fn fixed_compatible_router_enforces_scope_and_forwards_nested_models() {
        use crate::{
            database::Database,
            server::{dashboard_auth_config, observability::LogStore},
            storage::{ProviderCredential, ProviderRecord, Secret, VirtualKey},
        };
        use axum::{body::Body, extract::Request, response::Json};
        use ring::digest;
        use tokio::sync::mpsc;
        use tower::ServiceExt;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let upstream = Router::new().route("/{*path}", axum::routing::any(move |req: Request| {
            let tx = tx.clone();
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                let value: Value = serde_json::from_slice(&body).unwrap();
                tx.send((parts.uri.path().to_string(), parts.headers, value)).unwrap();
                let result = if parts.uri.path() == "/v1/chat/completions" {
                    json!({"id":"chat-result","choices":[{"message":{"role":"assistant","content":"chat answer"}}]})
                } else {
                    json!({"id":"response-result","output":[{"type":"message","content":[{"type":"output_text","text":"response answer"}]}]})
                };
                Json(result)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream_task =
            tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*database.db).clone();
        for (id, kind) in [
            ("openrouter", "openrouter"),
            ("other", "openrouter"),
            ("wrong-kind", "openai"),
        ] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: kind.to_string(),
                base_url: "http://127.0.0.1:1/untrusted".to_string(),
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
        }
        toasty::create!(ProviderCredential {
            id: "cred".to_string(),
            provider_id: "openrouter".to_string(),
            name: "account".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        let name = "credential.cred.api_key";
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, "upstream-key").unwrap())
            .exec(&mut db)
            .await
            .unwrap();
        let token = "virtual-key";
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "restricted".to_string(),
            key_hash: digest::digest(&digest::SHA256, token.as_bytes())
                .as_ref()
                .to_vec(),
            providers: "[\"openrouter\"]".to_string(),
            models: "[\"vendor/nested-model\"]".to_string(),
            created_at: 0
        })
        .exec(&mut db)
        .await
        .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings: store.clone(),
            logs: LogStore::new(database),
            dashboard_auth: dashboard_auth_config::test_auth().await,
        };
        let gateway = Router::new()
            .nest(
                "/openrouter/{id}",
                router_for(&store, "openrouter", Some(&base)),
            )
            .with_state(state);
        let request = |provider: &str, endpoint: &str, model: &str, bearer: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/openrouter/{provider}/v1/{endpoint}"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"model":model,"input":"hello"}).to_string(),
                ))
                .unwrap()
        };
        for (provider, model, bearer, status) in [
            (
                "wrong-kind",
                "vendor/nested-model",
                token,
                StatusCode::NOT_FOUND,
            ),
            ("other", "vendor/nested-model", token, StatusCode::FORBIDDEN),
            ("openrouter", "vendor/denied", token, StatusCode::FORBIDDEN),
            (
                "openrouter",
                "vendor/nested-model",
                "invalid",
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            assert_eq!(
                gateway
                    .clone()
                    .oneshot(request(provider, "responses", model, bearer))
                    .await
                    .unwrap()
                    .status(),
                status
            );
        }
        assert!(rx.try_recv().is_err());
        for (endpoint, expected_id) in [
            ("chat/completions", "chat-result"),
            ("responses", "response-result"),
        ] {
            let response = gateway
                .clone()
                .oneshot(request(
                    "openrouter",
                    endpoint,
                    "vendor/nested-model",
                    token,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["id"], expected_id);
            let (path, headers, body) = rx.recv().await.unwrap();
            assert_eq!(path, format!("/v1/{endpoint}"));
            assert_eq!(headers[header::AUTHORIZATION], "Bearer upstream-key");
            assert_eq!(body["model"], "vendor/nested-model");
        }
        upstream_task.abort();
    }
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn credentials_fail_over_before_forwarding_for_both_providers() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::sync::mpsc;

        let store = SettingsStore::new(
            crate::database::Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        for (id, kind) in [("open", "openai"), ("zen", "zen"), ("other", "openai")] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
            for n in 0..2 {
                let credential_id = format!("{id}-{n}");
                toasty::create!(ProviderCredential {
                    id: credential_id.clone(),
                    provider_id: id.to_string(),
                    name: credential_id.clone(),
                    weight: 1,
                    enabled: true
                })
                .exec(&mut (*store.database.db).clone())
                .await
                .unwrap();
                let name = format!("credential.{credential_id}.api_key");
                Secret::upsert_by_name(name.as_str())
                    .value(store.encrypt(&name, &credential_id).unwrap())
                    .exec(&mut (*store.database.db).clone())
                    .await
                    .unwrap();
            }
        }
        for kind in ["openai"] {
            for failure in [
                StatusCode::UNAUTHORIZED,
                StatusCode::FORBIDDEN,
                StatusCode::TOO_MANY_REQUESTS,
            ] {
                for all_fail in [false, true] {
                    let count = Arc::new(AtomicUsize::new(0));
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    let app = Router::new().route(
                        "/{*path}",
                        axum::routing::any({
                            let count = count.clone();
                            move |headers: HeaderMap| {
                                let count = count.clone();
                                let tx = tx.clone();
                                async move {
                                    tx.send(
                                        headers
                                            .get(header::AUTHORIZATION)
                                            .unwrap()
                                            .to_str()
                                            .unwrap()
                                            .to_owned(),
                                    )
                                    .unwrap();
                                    if count.fetch_add(1, Ordering::SeqCst) == 0 || all_fail {
                                        (failure, "rejected").into_response()
                                    } else {
                                        (StatusCode::OK, "data: [DONE]\n\n").into_response()
                                    }
                                }
                            }
                        }),
                    );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let provider = Provider {
                        id: if kind == "zen" { "zen" } else { "open" }.into(),
                        name: kind.into(),
                        kind: kind.into(),
                        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
                    };
                    let server =
                        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                    let identity = RequestIdentity::default();
                    let response = send(
                        &store,
                        &provider,
                        None,
                        "models",
                        None,
                        Method::GET,
                        &HeaderMap::new(),
                        None,
                        Some(&identity),
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        response.status(),
                        if all_fail { failure } else { StatusCode::OK }
                    );
                    let first = rx.recv().await.unwrap();
                    let second = rx.recv().await.unwrap();
                    assert_ne!(first, second);
                    assert!(first.starts_with(&format!("Bearer {}-", provider.id)));
                    assert!(second.starts_with(&format!("Bearer {}-", provider.id)));
                    let snapshot = identity.snapshot();
                    let expected = (!all_fail).then(|| second.trim_start_matches("Bearer "));
                    assert_eq!(snapshot.credential_id.as_deref(), expected);
                    assert_eq!(snapshot.credential_name.as_deref(), expected);
                    assert_eq!(count.load(Ordering::SeqCst), 2);
                    server.abort();
                }
            }
        }
        for (id, kind) in [("anonymous-open", "openai")] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: id.to_string(),
                name: id.to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
            let name = format!("credential.{id}.api_key");
            Secret::upsert_by_name(name.as_str())
                .value(store.encrypt(&name, "").unwrap())
                .exec(&mut (*store.database.db).clone())
                .await
                .unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let app = Router::new().route(
                "/{*path}",
                axum::routing::any(move |headers: HeaderMap| {
                    let tx = tx.clone();
                    async move {
                        tx.send(headers.contains_key(header::AUTHORIZATION))
                            .unwrap();
                        (StatusCode::OK, "data: [DONE]\n\n")
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider = Provider {
                id: id.into(),
                name: id.into(),
                kind: kind.into(),
                base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            };
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            assert_eq!(
                send(
                    &store,
                    &provider,
                    None,
                    "models",
                    None,
                    Method::GET,
                    &HeaderMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap()
                .status(),
                StatusCode::OK
            );
            assert!(!rx.recv().await.unwrap());
            assert!(rx.try_recv().is_err());
            server.abort();
        }
    }
}
