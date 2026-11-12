use super::*;
use axum::{Router, routing::get};

#[tokio::test]
async fn model_responses_preserve_raw_failures_and_unrestricted_lists() {
    let app = Router::new()
        .route(
            "/raw",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/plain"),
                        ("x-upstream".parse().unwrap(), "kept"),
                    ],
                    "not json",
                )
            }),
        )
        .route(
            "/failure",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "denied",
                )
            }),
        )
        .route(
            "/list",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    r#"{"data":[{"id":"ok"},{"id":3},{}],"extra":1}"#,
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let unrestricted = Permissions {
        providers: vec![],
        models: vec![],
    };
    let restricted = Permissions {
        providers: vec![],
        models: vec!["ok".into()],
    };
    let client = reqwest::Client::new();

    let raw = model_response(
        client.get(format!("{base}/raw")).send().await.unwrap(),
        &unrestricted,
    )
    .await;
    assert_eq!(raw.status(), StatusCode::OK);
    assert_eq!(raw.headers()["x-upstream"], "kept");
    assert_eq!(
        axum::body::to_bytes(raw.into_body(), 1024).await.unwrap(),
        "not json"
    );
    let invalid = model_response(
        client.get(format!("{base}/raw")).send().await.unwrap(),
        &restricted,
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_GATEWAY);
    let failure = model_response(
        client.get(format!("{base}/failure")).send().await.unwrap(),
        &restricted,
    )
    .await;
    assert_eq!(failure.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        axum::body::to_bytes(failure.into_body(), 1024)
            .await
            .unwrap(),
        "denied"
    );
    let filtered = model_response(
        client.get(format!("{base}/list")).send().await.unwrap(),
        &restricted,
    )
    .await;
    assert_eq!(filtered.headers()[header::CONTENT_TYPE], "application/json");
    let body = axum::body::to_bytes(filtered.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        serde_json::json!({"data":[{"id":"ok"}],"extra":1})
    );
    server.abort();
}

#[tokio::test]
async fn missing_credentials_return_provider_specific_error() {
    let database = crate::database::Database::open(":memory:").await.unwrap();
    let settings = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .await
        .unwrap();
    let provider = crate::provider::registry::Provider {
        id: "missing".into(),
        name: "Missing".into(),
        kind: "zen".into(),
        base_url: String::new(),
        keyless: false,
    };
    let result = retry_api_keys(
        &settings,
        &provider,
        "No connected Zen credentials",
        None,
        |_| async { panic!("no request should be sent") },
    )
    .await
    .unwrap_err()
    .into_response();
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(result.into_body(), 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["message"], "No connected Zen credentials");
    assert_eq!(value["error"]["code"], "no_credentials");
}

#[tokio::test]
async fn keyless_provider_executes_without_credentials() {
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/ok", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let database = crate::database::Database::open(":memory:").await.unwrap();
    let settings = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .await
        .unwrap();
    let provider = crate::provider::registry::Provider {
        id: "ollama".into(),
        name: "Ollama".into(),
        kind: "openai".into(),
        base_url: String::new(),
        keyless: true,
    };
    let identity = crate::provider::common::request_identity::RequestIdentity::default();
    let mut called = false;
    let response = retry_api_keys(
        &settings,
        &provider,
        "No connected credentials",
        Some(&identity),
        |key| {
            assert_eq!(key, "");
            called = true;
            let url = url.clone();
            async move { reqwest::get(&url).await.map_err(|_| "failed") }
        },
    )
    .await
    .unwrap();
    assert!(called);
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = identity.snapshot();
    assert_eq!(snapshot.credential_id.as_deref(), Some("keyless"));
    assert_eq!(snapshot.credential_name.as_deref(), Some("Keyless"));
    server.abort();
}
