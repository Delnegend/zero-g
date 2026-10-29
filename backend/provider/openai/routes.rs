use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::provider::common::proxy::{
    ProxyError, check_model, error, filter_models, settings_error,
};
use crate::provider::openai;
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{self, Permissions, SettingsStore},
};

/// Build authenticated routes for an OpenAI-compatible provider.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "openai"),
            settings::virtual_key_auth,
        ))
}

/// Resolve credentials and send a request upstream.
async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, ProxyError> {
    let credentials = crate::provider::credentials::list(settings, &provider.id)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    for credential in credentials {
        let key = settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = openai::send(
            &provider.base_url,
            &key,
            path,
            query,
            method.clone(),
            headers,
            body.clone(),
        )
        .await
        .map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if !matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No connected OpenAI-compatible credentials",
            "upstream_error",
            "no_credentials",
        )
    })
}

/// Fetch and filter models allowed by the virtual key.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let response = match send(
        &state.settings,
        &provider,
        "models",
        query.as_deref(),
        Method::GET,
        &headers,
        None,
    )
    .await
    {
        Ok(response) => response,
        Err(response) => return response.into_response(),
    };
    if !response.status().is_success() || permissions.models.is_empty() {
        return openai::forward(response);
    }
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(mut value) = response.json::<Value>().await else {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    };
    if !filter_models(&mut value, &permissions) {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    }
    let mut result = Response::new(Body::from(value.to_string()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

/// Forward an inference request after checking model permissions.
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    match send(
        &state.settings,
        &provider,
        path,
        query.as_deref(),
        Method::POST,
        &headers,
        Some(body),
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(response) => response.into_response(),
    }
}

/// Handle chat completion requests.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "chat/completions",
    )
    .await
}

/// Handle responses API requests.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "responses",
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
        for kind in ["openai", "zen"] {
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
                    if kind == "zen" {
                        let req = serde_json::from_value(
                            json!({"model":"beam-free","messages":[],"stream":true}),
                        )
                        .unwrap();
                        let response =
                            crate::provider::zen::routes::send(&store, &provider, &req).await;
                        assert_eq!(
                            response.status(),
                            if all_fail { failure } else { StatusCode::OK }
                        );
                    } else {
                        let response = send(
                            &store,
                            &provider,
                            "models",
                            None,
                            Method::GET,
                            &HeaderMap::new(),
                            None,
                        )
                        .await
                        .unwrap();
                        assert_eq!(
                            response.status(),
                            if all_fail { failure } else { StatusCode::OK }
                        );
                    }
                    let first = rx.recv().await.unwrap();
                    let second = rx.recv().await.unwrap();
                    assert_ne!(first, second);
                    assert!(first.starts_with(&format!("Bearer {}-", provider.id)));
                    assert!(second.starts_with(&format!("Bearer {}-", provider.id)));
                    assert_eq!(count.load(Ordering::SeqCst), 2);
                    server.abort();
                }
            }
        }
        for (id, kind) in [("anonymous-open", "openai"), ("anonymous-zen", "zen")] {
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
            if kind == "zen" {
                let req = serde_json::from_value(
                    json!({"model":"beam-free","messages":[],"stream":true}),
                )
                .unwrap();
                assert_eq!(
                    crate::provider::zen::routes::send(&store, &provider, &req)
                        .await
                        .status(),
                    StatusCode::OK
                );
            } else {
                assert_eq!(
                    send(
                        &store,
                        &provider,
                        "models",
                        None,
                        Method::GET,
                        &HeaderMap::new(),
                        None
                    )
                    .await
                    .unwrap()
                    .status(),
                    StatusCode::OK
                );
            }
            assert!(!rx.recv().await.unwrap());
            assert!(rx.try_recv().is_err());
            server.abort();
        }
    }
}
