use crate::openai::Model;
use crate::provider::{ProviderKind, registry::Provider};
use crate::server::AppState;

/// Discovers models using the configured provider's backend.
pub(crate) async fn discover(state: &AppState, provider: &Provider) -> Result<Vec<Model>, String> {
    ProviderKind::from_str(&provider.kind)
        .ok_or_else(|| "Unsupported provider kind".to_string())?
        .discover(state, provider)
        .await
}

/// Discovers models permitted by the virtual key.
pub(crate) async fn discover_permitted(
    state: &AppState,
    provider: &Provider,
    permissions: &crate::server::settings::Permissions,
) -> Result<Vec<Model>, String> {
    ProviderKind::from_str(&provider.kind)
        .ok_or_else(|| "Unsupported provider kind".to_string())?
        .discover_permitted(state, provider, permissions)
        .await
}

/// Model discovery integration checks.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::provider::openai::{self, Backend};
    use crate::server::settings::SettingsStore;
    use crate::{database::Database, provider::registry};
    use axum::http::HeaderMap;
    use axum::{
        Json, Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::get,
    };
    use serde_json::json;
    use tower::ServiceExt;

    /// Discovery uses the instance credential and retains model metadata.
    #[tokio::test]
    async fn discovery_uses_instance_key_and_preserves_metadata() {
        let app = Router::new().route("/v1/models", get(|headers: HeaderMap| async move {
            assert_eq!(headers.get(header::AUTHORIZATION).unwrap(), "Bearer instance-secret");
            Json(json!({"data":[{"id":"model-a","capabilities":{"vision":true},"context_window":1234},{"id":"model-b","context_window":"unknown"},{"name":"invalid"},{"id":""},{"id":12}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let base = format!("http://{addr}/v1");
        let response = registry::router(store.clone()).oneshot(Request::builder().method("POST").uri("/api/providers")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"id":"instance", "name":"Instance", "kind":"openai", "base_url":base}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let response = crate::provider::credentials::router(store.clone()).oneshot(Request::builder()
            .method("POST").uri("/api/providers/instance/credentials")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"name":"Primary","weight":1,"enabled":true,"api_key":"instance-secret"}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let provider = registry::get(&store.database, "instance")
            .await
            .unwrap()
            .unwrap();
        let models = openai::discover::<Backend>(&store, &provider)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(models).unwrap(),
            json!([{"id":"model-a","capabilities":{"vision":true},"context_window":1234}])
        );
        server.abort();
    }

    /// Discovery retries authentication and rate-limit failures only.
    #[tokio::test]
    async fn discovery_retries_only_credential_failures() {
        use std::sync::{Arc, Mutex};

        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_REQUEST,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let seen = Arc::new(Mutex::new(Vec::<String>::new()));
            let requests = seen.clone();
            let app = Router::new().route(
                "/v1/models",
                get(move |headers: HeaderMap| {
                    let requests = requests.clone();
                    async move {
                        let credential =
                            headers[header::AUTHORIZATION].to_str().unwrap().to_owned();
                        let mut requests = requests.lock().unwrap();
                        let rejected = requests
                            .first()
                            .cloned()
                            .unwrap_or_else(|| credential.clone());
                        requests.push(credential.clone());
                        drop(requests);
                        if credential == rejected {
                            (status, Json(json!({"error":"failed"})))
                        } else {
                            (StatusCode::OK, Json(json!({"data":[{"id":"model-a"}]})))
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let database = Database::open(":memory:").await.unwrap();
            let store =
                SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                    .await
                    .unwrap();
            let response = registry::router(store.clone()).oneshot(Request::builder().method("POST").uri("/api/providers")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"id":"instance","name":"Instance","kind":"openai","base_url":base}).to_string())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            for name in ["first", "second"] {
                let response = crate::provider::credentials::router(store.clone())
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/api/providers/instance/credentials")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(
                                json!({"name":name,"weight":1,"enabled":true,"api_key":name})
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::CREATED);
            }
            let provider = registry::get(&store.database, "instance")
                .await
                .unwrap()
                .unwrap();
            let result = openai::discover::<Backend>(&store, &provider).await;
            let retryable = matches!(status.as_u16(), 401 | 403 | 429);
            assert_eq!(result.is_ok(), retryable, "status {status}");
            let requests = seen.lock().unwrap();
            assert_eq!(requests.len(), if retryable { 2 } else { 1 });
            if retryable {
                assert_ne!(requests[0], requests[1]);
                assert_eq!(
                    serde_json::to_value(result.unwrap()).unwrap(),
                    json!([{"id":"model-a"}])
                );
            }
            server.abort();
        }
    }
}
