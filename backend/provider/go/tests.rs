use super::*;
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::openai::routes::{Upstream, models_handler, send};
use crate::server::settings::Permissions;
use axum::Extension;
use axum::Json;
use axum::body::Body;
use axum::extract::RawQuery;
use axum::http::StatusCode;
use serde_json::json;
use tokio::sync::mpsc;

#[test]
fn endpoint_selection() {
    let mut provider = Provider {
        id: "go".into(),
        name: "Go".into(),
        kind: "go".into(),
        base_url: String::new(),
        keyless: false,
    };
    assert_eq!(base(&provider, false), DEFAULT_BASE);
    assert_eq!(base(&provider, true), MODELS_BASE);
    provider.base_url = "https://example.com/v1".into();
    assert_eq!(base(&provider, true), "https://example.com/v1");
}

// Keep the authenticated upstream and both response scenarios together.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn restricted_model_listing_filters_and_rejects_invalid_upstream() {
    use crate::storage::{ProviderCredential, ProviderRecord, Secret};
    use tower::ServiceExt;

    let app = Router::new().route(
        "/v1/models",
        axum::routing::get(|headers: HeaderMap, RawQuery(query): RawQuery| async move {
            if headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                != Some("Bearer upstream-secret")
            {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error":"invalid credential"})),
                );
            }
            let body = if query.as_deref() == Some("invalid=1") {
                json!({"data":"not an array"})
            } else {
                json!({"data":[{"id":"allowed","extra":1},{"id":"other"}]})
            };
            (StatusCode::OK, Json(body))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let database = crate::database::Database::open(":memory:").await.unwrap();
    let store = SettingsStore::new(
        database.clone(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    toasty::create!(ProviderRecord {
        id: "go".to_string(),
        name: "Go".to_string(),
        kind: "go".to_string(),
        base_url: base_url.clone(),
        keyless: false,
        deleted: false
    })
    .exec(&mut (*database.db).clone())
    .await
    .unwrap();
    toasty::create!(ProviderCredential {
        id: "key".to_string(),
        provider_id: "go".to_string(),
        name: "key".to_string(),
        weight: 1,
        enabled: true
    })
    .exec(&mut (*database.db).clone())
    .await
    .unwrap();
    let secret_name = "credential.key.api_key";
    Secret::upsert_by_name(secret_name)
        .value(store.encrypt(secret_name, "upstream-secret").unwrap())
        .exec(&mut (*database.db).clone())
        .await
        .unwrap();
    let state = AppState {
        providers: crate::provider::Providers::new(),
        settings: store,
        logs: crate::server::observability::LogStore::new(database),
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
    };
    let provider = Provider {
        id: "go".into(),
        name: "Go".into(),
        kind: "go".into(),
        base_url,
        keyless: false,
    };
    let permissions = Permissions {
        providers: vec![],
        models: vec!["allowed".into()],
    };
    let router = Router::new()
        .route("/v1/models", axum::routing::get(models_handler))
        .layer(Extension(provider))
        .layer(Extension(permissions))
        .layer(Extension(Upstream::Go))
        .with_state(state);
    for (uri, expected) in [
        ("/v1/models", StatusCode::OK),
        ("/v1/models?invalid=1", StatusCode::BAD_GATEWAY),
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"data":[{"id":"allowed","extra":1}]})
            );
        }
    }
    server.abort();
}

#[tokio::test]
async fn forwards_auth_protocol_and_real_session_without_impersonation() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/{*path}",
        axum::routing::any(move |req: axum::extract::Request| {
            let tx = tx.clone();
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 1024).await.unwrap();
                tx.send((parts, body)).unwrap();
                (StatusCode::OK, "ok")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for path in ["chat/completions", "responses", "models"] {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer client-secret".parse().unwrap(),
        );
        headers.insert(header::USER_AGENT, "opencode/fake".parse().unwrap());
        headers.insert("x-opencode-session", "conversation-123".parse().unwrap());
        let method = if path == "models" {
            Method::GET
        } else {
            Method::POST
        };
        let response = send_upstream(
            &base,
            "upstream-secret",
            path,
            Some("a=1"),
            method,
            &headers,
            Some(Bytes::from_static(br#"{"model":"m"}"#)),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let (parts, body) = rx.recv().await.unwrap();
        assert_eq!(parts.uri.to_string(), format!("/v1/{path}?a=1"));
        assert_eq!(
            parts.headers[header::AUTHORIZATION],
            "Bearer upstream-secret"
        );
        assert!(
            parts.headers[header::USER_AGENT]
                .to_str()
                .unwrap()
                .starts_with("zero-g/")
        );
        assert_eq!(parts.headers["x-opencode-session"], "conversation-123");
        assert_eq!(body.as_ref(), br#"{"model":"m"}"#);
    }
    server.abort();
}

#[tokio::test]
async fn credential_failover_uses_next_key_for_auth_and_rate_limits() {
    use crate::storage::{ProviderCredential, ProviderRecord, Secret};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let store = SettingsStore::new(
        crate::database::Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    toasty::create!(ProviderRecord {
        id: "go".to_string(),
        name: "Go".to_string(),
        kind: "go".to_string(),
        base_url: String::new(),
        keyless: false,
        deleted: false
    })
    .exec(&mut (*store.database.db).clone())
    .await
    .unwrap();
    for id in ["first", "second"] {
        toasty::create!(ProviderCredential {
            id: id.to_string(),
            provider_id: "go".to_string(),
            name: id.to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        let name = format!("credential.{id}.api_key");
        Secret::upsert_by_name(name.as_str())
            .value(store.encrypt(&name, id).unwrap())
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
    }
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
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
                        tx.send(headers[header::AUTHORIZATION].clone()).unwrap();
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            status
                        } else {
                            StatusCode::OK
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Provider {
            id: "go".into(),
            name: "Go".into(),
            kind: "go".into(),
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            keyless: false,
        };
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let identity = RequestIdentity::default();
        let response = send(
            &store,
            &provider,
            &Upstream::Go,
            "responses",
            None,
            Method::POST,
            &HeaderMap::new(),
            None,
            Some(&identity),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_ne!(first, second);
        let expected = second.to_str().unwrap().trim_start_matches("Bearer ");
        let snapshot = identity.snapshot();
        assert_eq!(snapshot.credential_id.as_deref(), Some(expected));
        assert_eq!(snapshot.credential_name.as_deref(), Some(expected));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
