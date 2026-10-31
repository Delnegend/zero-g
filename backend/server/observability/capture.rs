use std::time::Instant;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use tracing::error;

use crate::provider::common::request_identity::RequestIdentity;

use super::{LogEntry, LogStore, now};

/// Capture POST request and response bodies for asynchronous logging.
pub(in crate::server) async fn capture(
    State(store): State<LogStore>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::POST {
        return next.run(req).await;
    }
    let start = Instant::now();
    let path = req.uri().path().to_owned();
    let (mut parts, body) = req.into_parts();
    let identity = RequestIdentity::default();
    parts.extensions.insert(identity.clone());
    let Ok(body) = to_bytes(body, crate::server::inference::INFERENCE_BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let request_body = String::from_utf8_lossy(&body).into_owned();
    let namespaced_model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let (provider, model) = if let Some((provider, model)) = namespaced_model.split_once('/') {
        (provider.to_owned(), model.to_owned())
    } else {
        (String::new(), namespaced_model)
    };
    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    let identity = identity.snapshot();
    let status = i64::from(response.status().as_u16());
    let (parts, body) = response.into_parts();
    let stream = FinalizeStream {
        inner: Box::pin(body.into_data_stream()),
        finish: Some(Box::new(move |bytes, completed| {
            let response_body = String::from_utf8_lossy(&bytes).into_owned();
            let (input_tokens, output_tokens) = usage(&response_body);
            let log = LogEntry {
                id: 0,
                created_at: now(),
                provider,
                model,
                path,
                status: if completed { status } else { 499 },
                latency_ms: i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX),
                input_tokens,
                output_tokens,
                virtual_key_id: identity.virtual_key_id,
                virtual_key_name: identity.virtual_key_name,
                credential_id: identity.credential_id,
                credential_name: identity.credential_name,
                request_body,
                response_body,
            };
            tokio::spawn(async move {
                if let Err(err) = store.insert(log).await {
                    error!("Could not store request log: {err}");
                }
            });
        })),
        captured: Vec::new(),
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Callback receiving captured bytes and whether the response completed.
type Completion = Box<dyn FnOnce(Vec<u8>, bool) + Send>;

/// Response stream that records bytes and finalizes on completion or drop.
struct FinalizeStream<S> {
    /// Wrapped response body stream.
    inner: std::pin::Pin<Box<S>>,
    /// One-shot completion callback.
    finish: Option<Completion>,
    /// Response bytes observed so far.
    captured: Vec<u8>,
}
impl<S, E> futures_util::Stream for FinalizeStream<S>
where
    S: futures_util::Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        match &result {
            std::task::Poll::Ready(Some(Ok(bytes))) => self.captured.extend_from_slice(bytes),
            std::task::Poll::Ready(None) => {
                if let Some(finish) = self.finish.take() {
                    finish(std::mem::take(&mut self.captured), true);
                }
            }
            _ => {}
        }
        result
    }
}
impl<S> Drop for FinalizeStream<S> {
    fn drop(&mut self) {
        if let Some(finish) = self.finish.take() {
            finish(std::mem::take(&mut self.captured), false);
        }
    }
}

/// Extract token counts from JSON or server-sent event response bodies.
fn usage(body: &str) -> (Option<i64>, Option<i64>) {
    let mut result = (None, None);
    for part in std::iter::once(body).chain(
        body.lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim)),
    ) {
        if let Ok(json) = serde_json::from_str::<Value>(part) {
            let value = json
                .get("usage")
                .or_else(|| json.pointer("/response/usage"));
            if let Some(value) = value {
                result.0 = value
                    .get("input_tokens")
                    .or_else(|| value.get("prompt_tokens"))
                    .and_then(Value::as_i64)
                    .or(result.0);
                result.1 = value
                    .get("output_tokens")
                    .or_else(|| value.get("completion_tokens"))
                    .and_then(Value::as_i64)
                    .or(result.1);
            }
        }
    }
    result
}

/// Request capture tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::dashboard;
    use super::*;
    use crate::database::Database;
    use axum::{
        Router, middleware,
        routing::{get, post},
    };
    use futures_util::StreamExt;
    use tower::ServiceExt;

    /// Capture completed and abandoned responses through the HTTP routes.
    // Keep the completed and interrupted requests with their persisted results.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn capture_persists_complete_and_interrupted_responses() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = LogStore::new(database);
        let complete_body =
            r#"{"answer":"hello","usage":{"prompt_tokens":7,"completion_tokens":4}}"#;
        let partial_body =
            "data: {\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":5}}}\n\n";
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let wait = std::sync::Arc::new(tokio::sync::Mutex::new(Some(wait)));
        let app = Router::new()
            .route("/complete", post(move || async move { complete_body }))
            .route(
                "/stream",
                post(move || {
                    let wait = wait.clone();
                    async move {
                        let receiver = wait.lock().await.take().unwrap();
                        let first = futures_util::stream::once(async move {
                            Ok::<_, std::convert::Infallible>(Bytes::from_static(
                                partial_body.as_bytes(),
                            ))
                        });
                        let remainder = futures_util::stream::once(async move {
                            let _ = receiver.await;
                            Ok::<_, std::convert::Infallible>(Bytes::from_static(
                                b"unexpected remainder",
                            ))
                        });
                        Body::from_stream(first.chain(remainder))
                    }
                }),
            )
            .route_layer(middleware::from_fn_with_state(store.clone(), capture))
            .route("/api/observability", get(dashboard))
            .with_state(store.clone());

        let request = |uri, body: &'static str| {
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .body(Body::from(body))
                .unwrap()
        };
        let complete_request = r#"{"model":"codex/model-a","input":"ordinary"}"#;
        let response = app
            .clone()
            .oneshot(request("/complete", complete_request))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            complete_body
        );

        let stream_request = r#"{"model":"antigravity/model-b","input":"stream"}"#;
        let response = app
            .clone()
            .oneshot(request("/stream", stream_request))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), partial_body);
        drop(body);
        drop(release);

        let logs = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/api/observability")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                let dashboard: Value = serde_json::from_slice(&bytes).unwrap();
                if dashboard["summary"]["requests"] == 2 {
                    break dashboard["logs"].as_array().unwrap().clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both capture logs must be persisted");
        for (request, provider, model, route, status, response, input, output) in [
            (
                complete_request,
                "codex",
                "model-a",
                "/complete",
                200,
                complete_body,
                7,
                4,
            ),
            (
                stream_request,
                "antigravity",
                "model-b",
                "/stream",
                499,
                partial_body,
                3,
                5,
            ),
        ] {
            let matching: Vec<_> = logs
                .iter()
                .filter(|log| log["request_body"] == request)
                .collect();
            assert_eq!(matching.len(), 1);
            let log = matching[0];
            assert_eq!(log["provider"], provider);
            assert_eq!(log["model"], model);
            assert_eq!(log["path"], route);
            assert_eq!(log["status"], status);
            assert_eq!(log["response_body"], response);
            assert_eq!(log["input_tokens"], input);
            assert_eq!(log["output_tokens"], output);
        }
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Concurrent and streamed requests retain only their own identity snapshots.
    #[allow(clippy::literal_string_with_formatting_args)]
    #[tokio::test]
    async fn capture_keeps_concurrent_identities_isolated() {
        let path =
            std::env::temp_dir().join(format!("zero-g-identity-test-{}.db", uuid::Uuid::new_v4()));
        let store = LogStore::new(Database::open(path.to_str().unwrap()).await.unwrap());
        let app = Router::new()
            .route("/{id}", post(|axum::extract::Path(id): axum::extract::Path<String>,
                axum::Extension(identity): axum::Extension<RequestIdentity>| async move {
                if id != "anonymous" {
                    identity.set_virtual_key(&id, &format!("name-{id}"));
                    if id != "failed" {
                        identity.set_credential(&id, &format!("credential-{id}"));
                    }
                }
                tokio::task::yield_now().await;
                Body::from_stream(futures_util::stream::once(async { Ok::<_, std::convert::Infallible>(Bytes::from_static(b"response")) }))
            }))
            .route_layer(middleware::from_fn_with_state(store.clone(), capture))
            .with_state(store.clone());
        let request = |id| {
            Request::builder()
                .method(Method::POST)
                .uri(format!("/{id}"))
                .body(Body::from("{}"))
                .unwrap()
        };
        let responses = tokio::join!(
            app.clone().oneshot(request("a")),
            app.clone().oneshot(request("b")),
            app.clone().oneshot(request("failed")),
            app.clone().oneshot(request("anonymous"))
        );
        for response in <[_; 4]>::from(responses) {
            to_bytes(response.unwrap().into_body(), usize::MAX)
                .await
                .unwrap();
        }
        let logs = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let result = store
                    .dashboard(&super::super::DashboardQuery::default())
                    .await
                    .unwrap();
                if result.logs.len() == 4 {
                    break result.logs;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for log in logs {
            let id = log.path.trim_start_matches('/');
            if id == "anonymous" {
                assert!(log.virtual_key_id.is_none());
                assert!(log.credential_id.is_none());
            } else {
                assert_eq!(log.virtual_key_id.as_deref(), Some(id));
                assert_eq!(
                    log.virtual_key_name.as_deref(),
                    Some(format!("name-{id}").as_str())
                );
                if id == "failed" {
                    assert!(log.credential_id.is_none());
                } else {
                    assert_eq!(log.credential_id.as_deref(), Some(id));
                    assert_eq!(
                        log.credential_name.as_deref(),
                        Some(format!("credential-{id}").as_str())
                    );
                }
            }
        }
        drop(app);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Read usage counts from JSON and streaming responses.
    #[test]
    fn extracts_usage_from_streamed_and_nonstreamed_responses() {
        assert_eq!(
            usage(r#"{"usage":{"prompt_tokens":7,"completion_tokens":4}}"#),
            (Some(7), Some(4))
        );
        assert_eq!(
            usage("data: {\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":5}}}\n\n"),
            (Some(3), Some(5))
        );
    }
}
