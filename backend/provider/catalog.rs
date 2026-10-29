use axum::http::{HeaderMap, Method};
use futures_util::StreamExt;
use serde_json::Value;

use crate::provider::{
    antigravity::routes::{fetch_models, to_model_entry},
    codex::models::fetch_catalog_pool,
    go, openai,
    registry::Provider,
    zen,
};
use crate::server::{AppState, settings::SettingsStore};

const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

async fn openai_models(store: &SettingsStore, provider: &Provider) -> Result<Vec<Value>, String> {
    let credentials = crate::provider::credentials::list(store, &provider.id).await?;
    let mut response = None;
    for credential in credentials {
        let key = store
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| "Failed to load provider credentials")?
            .ok_or("Provider credential is not connected")?;
        let current = if provider.kind == "go" {
            go::send_upstream(
                go::base(provider, true),
                &key,
                "models",
                None,
                Method::GET,
                &HeaderMap::new(),
                None,
            )
            .await
        } else {
            openai::send(
                &provider.base_url,
                &key,
                "models",
                None,
                Method::GET,
                &HeaderMap::new(),
                None,
            )
            .await
        }
        .map_err(str::to_owned)?;
        let status = current.status();
        response = Some(current);
        if !matches!(status.as_u16(), 401 | 403 | 429) {
            break;
        }
    }
    let response = response.ok_or("Provider has no connected credentials")?;
    if !response.status().is_success() {
        return Err(format!(
            "Provider model discovery returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Failed to read provider model list")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("Provider model list is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid provider model list")?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Invalid provider model list")?;
    Ok(models
        .iter()
        .filter(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .collect())
}

pub(crate) async fn discover(state: &AppState, provider: &Provider) -> Result<Vec<Value>, String> {
    match provider.kind.as_str() {
        "openai" | "go" => openai_models(&state.settings, provider).await,
        "zen" => zen::models_json()
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "Invalid Zen model list".into()),
        "agy" => {
            let models = fetch_models(state, provider).await?;
            models
                .into_iter()
                .map(|(id, info)| {
                    let mut raw = serde_json::to_value(&info)
                        .map_err(|_| "Failed to serialize Antigravity model")?;
                    let normalized = serde_json::to_value(to_model_entry(id, info))
                        .map_err(|_| "Failed to serialize Antigravity model")?;
                    if let (Some(raw), Value::Object(normalized)) =
                        (raw.as_object_mut(), normalized)
                    {
                        raw.extend(normalized.into_iter().filter(|(_, value)| !value.is_null()));
                    }
                    Ok(raw)
                })
                .collect()
        }
        "codex" => fetch_catalog_pool(state, provider)
            .await
            .map(|(_, catalog)| catalog),
        _ => Err("Unsupported provider kind".into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{database::Database, provider::registry};
    use axum::{
        Json, Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::get,
    };
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn discovery_uses_instance_key_and_preserves_metadata() {
        let app = Router::new().route("/v1/models", get(|headers: HeaderMap| async move {
            assert_eq!(headers.get(header::AUTHORIZATION).unwrap(), "Bearer instance-secret");
            Json(json!({"data":[{"id":"model-a","capabilities":{"vision":true},"context_window":1234},{"name":"invalid"}]}))
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
        let models = openai_models(&store, &provider).await.unwrap();
        assert_eq!(
            models,
            vec![json!({"id":"model-a","capabilities":{"vision":true},"context_window":1234})]
        );
        server.abort();
    }

    #[tokio::test]
    async fn discovery_retries_only_credential_failures() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_REQUEST,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let app = Router::new().route(
                "/v1/models",
                get(move || {
                    let seen = seen.clone();
                    async move {
                        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
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
            let result = openai_models(&store, &provider).await;
            let retryable = matches!(status.as_u16(), 401 | 403 | 429);
            assert_eq!(result.is_ok(), retryable, "status {status}");
            assert_eq!(calls.load(Ordering::SeqCst), if retryable { 2 } else { 1 });
            server.abort();
        }
    }
}
