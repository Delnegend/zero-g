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

/// Maximum response bytes retained for a single log entry.
const RESPONSE_CAPTURE_LIMIT: usize = 256 * 1024;

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
    let identity = parts
        .extensions
        .get::<RequestIdentity>()
        .cloned()
        .unwrap_or_default();
    parts.extensions.insert(identity.clone());
    let Ok(body) = to_bytes(body, crate::server::inference::INFERENCE_BODY_LIMIT).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            axum::Json(crate::server::error_body(
                "Request body too large",
                "invalid_request_error",
                Some("body_too_large"),
            )),
        )
            .into_response();
    };
    // Defer extracting attacker-controlled content until authentication has succeeded.
    let response = next
        .run(Request::from_parts(parts, Body::from(body.clone())))
        .await;
    let identity = identity.snapshot();
    let authenticated = identity.virtual_key_id.is_some();
    let (request_body, provider, model) = if authenticated {
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
        (request_body, provider, model)
    } else {
        (String::new(), String::new(), String::new())
    };
    let status = i64::from(response.status().as_u16());
    let (parts, body) = response.into_parts();
    let stream = FinalizeStream {
        inner: Box::pin(body.into_data_stream()),
        finish: Some(Box::new(move |bytes, completed, stream_usage| {
            // Anonymous responses can contain reflected request data as well.
            let response_body = if authenticated {
                let mut text = String::from_utf8_lossy(&bytes).into_owned();
                if text.len() > RESPONSE_CAPTURE_LIMIT {
                    let mut end = RESPONSE_CAPTURE_LIMIT;
                    while !text.is_char_boundary(end) {
                        end = end.saturating_sub(1);
                    }
                    text.truncate(end);
                }
                text
            } else {
                String::new()
            };
            let body_usage = usage(&response_body);
            let (input_tokens, output_tokens) = (
                stream_usage.0.or(body_usage.0),
                stream_usage.1.or(body_usage.1),
            );
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
        capture_body: authenticated,
        stream_usage: StreamUsage::default(),
        failed: false,
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Callback receiving captured bytes and whether the response completed.
type Completion = Box<dyn FnOnce(Vec<u8>, bool, (Option<i64>, Option<i64>)) + Send>;

/// Response stream that records bytes and finalizes on completion or drop.
struct FinalizeStream<S> {
    /// Wrapped response body stream.
    inner: std::pin::Pin<Box<S>>,
    /// One-shot completion callback.
    finish: Option<Completion>,
    /// Response bytes observed so far.
    captured: Vec<u8>,
    /// Only retain response content for authenticated requests.
    capture_body: bool,
    /// Bounded parser for usage events beyond the retained body prefix.
    stream_usage: StreamUsage,
    /// A stream error makes the response incomplete even if EOF follows.
    failed: bool,
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
            std::task::Poll::Ready(Some(Ok(bytes))) if self.capture_body => {
                let remaining = RESPONSE_CAPTURE_LIMIT.saturating_sub(self.captured.len());
                if let Some(prefix) = bytes.get(..bytes.len().min(remaining)) {
                    self.captured.extend_from_slice(prefix);
                }
                self.stream_usage.push(bytes);
            }
            std::task::Poll::Ready(Some(Err(_))) => self.failed = true,
            std::task::Poll::Ready(None) => {
                if let Some(finish) = self.finish.take() {
                    let completed = !self.failed;
                    let stream_usage = self.stream_usage.finish();
                    finish(std::mem::take(&mut self.captured), completed, stream_usage);
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
            let stream_usage = self.stream_usage.finish();
            finish(std::mem::take(&mut self.captured), false, stream_usage);
        }
    }
}

/// Parse complete SSE data lines while limiting memory for oversized events.
#[derive(Default)]
struct StreamUsage {
    /// Bytes accumulated for the current SSE line.
    line: Vec<u8>,
    /// Whether the current line exceeded the capture limit.
    oversized: bool,
    /// Most recently observed input and output token counts.
    counts: (Option<i64>, Option<i64>),
}

impl StreamUsage {
    /// Accumulate complete lines across response chunks.
    fn push(&mut self, bytes: &[u8]) {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            let content = part.strip_suffix(b"\n").unwrap_or(part);
            let newline = content.len() != part.len();
            if !self.oversized {
                if content.len() > RESPONSE_CAPTURE_LIMIT.saturating_sub(self.line.len()) {
                    self.line.clear();
                    self.oversized = true;
                } else {
                    self.line.extend_from_slice(content);
                }
            }
            if newline {
                self.finish_line();
            }
        }
    }

    /// Extract usage from the current line and reset its buffer.
    fn finish_line(&mut self) {
        if !self.oversized
            && self.line.starts_with(b"data:")
            && let Ok(line) = std::str::from_utf8(&self.line)
        {
            let counts = usage(line);
            self.counts.0 = counts.0.or(self.counts.0);
            self.counts.1 = counts.1.or(self.counts.1);
        }
        self.line.clear();
        self.oversized = false;
    }

    /// Process the final unterminated line and return observed counts.
    fn finish(&mut self) -> (Option<i64>, Option<i64>) {
        self.finish_line();
        self.counts
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
            .route_layer(middleware::from_fn(
                |request: Request, next: Next| async move {
                    request
                        .extensions()
                        .get::<RequestIdentity>()
                        .unwrap()
                        .set_virtual_key("test", "test");
                    next.run(request).await
                },
            ))
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
                assert_eq!(log.request_body, "");
                assert_eq!(log.response_body, "");
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

    /// Captures are bounded without affecting forwarded chunks, errors or anonymous requests.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn capture_bounds_streams_and_redacts_anonymous_bodies() {
        let path =
            std::env::temp_dir().join(format!("zero-g-capture-limit-{}.db", uuid::Uuid::new_v4()));
        let store = LogStore::new(Database::open(path.to_str().unwrap()).await.unwrap());
        let app = Router::new()
            .route("/{kind}", post(
                |axum::extract::Path(kind): axum::extract::Path<String>,
                 axum::Extension(identity): axum::Extension<RequestIdentity>| async move {
                    if kind != "anonymous" {
                        identity.set_virtual_key("id", "name");
                    }
                    if kind == "anonymous" {
                        let first = futures_util::stream::once(async {
                            Ok::<_, std::io::Error>(Bytes::from_static(b"reflected secret"))
                        });
                        return (StatusCode::UNAUTHORIZED, Body::from_stream(first.chain(futures_util::stream::pending()))).into_response();
                    }
                    if kind == "error" {
                        let chunks = vec![
                            Ok(Bytes::from_static(b"partial")),
                            Err(std::io::Error::other("upstream failed")),
                        ];
                        return Body::from_stream(futures_util::stream::iter(chunks)).into_response();
                    }
                    let mut first = vec![b'a'; RESPONSE_CAPTURE_LIMIT - 1];
                    first.extend_from_slice("€".as_bytes());
                    let chunks = vec![
                        Ok::<_, std::io::Error>(Bytes::from(first)),
                        Ok(Bytes::from_static(b"\ndata: {\"usage\":{\"input_")),
                        Ok(Bytes::from_static(b"tokens\":9,\"output_tokens\":4}}\n\n")),
                    ];
                    Body::from_stream(futures_util::stream::iter(chunks)).into_response()
                }
            ))
            .route_layer(middleware::from_fn_with_state(store.clone(), capture))
            .with_state(store.clone());
        let request = |kind| {
            Request::builder()
                .method(Method::POST)
                .uri(format!("/{kind}"))
                .body(Body::from(
                    r#"{"model":"evil/model","secret":"attacker payload"}"#,
                ))
                .unwrap()
        };

        let response = app.clone().oneshot(request("large")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let forwarded = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            &forwarded[..RESPONSE_CAPTURE_LIMIT - 1],
            vec![b'a'; RESPONSE_CAPTURE_LIMIT - 1]
        );
        assert!(
            forwarded.ends_with(b"data: {\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}\n\n")
        );
        assert_eq!(
            &forwarded[RESPONSE_CAPTURE_LIMIT - 1..RESPONSE_CAPTURE_LIMIT + 2],
            "€".as_bytes()
        );

        let response = app.clone().oneshot(request("anonymous")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "reflected secret");
        drop(body);

        let response = app.clone().oneshot(request("error")).await.unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "partial");
        assert!(body.next().await.unwrap().is_err());
        assert!(body.next().await.is_none());
        let identity = RequestIdentity::default();
        identity.set_virtual_key("id", "name");
        let oversized = Request::builder()
            .method(Method::POST)
            .uri("/large")
            .extension(identity)
            .body(Body::from(vec![
                b'x';
                crate::server::inference::INFERENCE_BODY_LIMIT
                    + 1
            ]))
            .unwrap();
        let response = app.clone().oneshot(oversized).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: Value = serde_json::from_slice(&response_body).unwrap();
        assert_eq!(error["error"]["message"], "Request body too large");
        assert_eq!(error["error"]["type"], "invalid_request_error");
        assert_eq!(error["error"]["code"], "body_too_large");

        drop(body);

        let logs = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let logs = store
                    .dashboard(&super::super::DashboardQuery::default())
                    .await
                    .unwrap()
                    .logs;
                if logs.len() == 3 {
                    break logs;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let large = logs.iter().find(|log| log.path == "/large").unwrap();
        assert_eq!(large.status, 200);
        assert!(large.response_body.len() <= RESPONSE_CAPTURE_LIMIT);
        assert!(
            large
                .response_body
                .starts_with(&"a".repeat(RESPONSE_CAPTURE_LIMIT - 1))
        );
        assert_eq!(large.input_tokens, Some(9));
        assert_eq!(large.output_tokens, Some(4));
        let anonymous = logs.iter().find(|log| log.path == "/anonymous").unwrap();
        assert_eq!(anonymous.status, 499);
        assert_eq!(anonymous.request_body, "");
        assert_eq!(anonymous.response_body, "");
        assert_eq!(anonymous.provider, "");
        assert_eq!(anonymous.model, "");
        let error = logs.iter().find(|log| log.path == "/error").unwrap();
        assert_eq!(error.status, 499);
        assert_eq!(error.response_body, "partial");
        assert!(error.input_tokens.is_none());
        assert!(error.output_tokens.is_none());
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
