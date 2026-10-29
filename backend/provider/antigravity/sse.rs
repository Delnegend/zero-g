use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::antigravity::{AntigravityEvent, spawn_event_stream};

/// Sender for infallible SSE event results.
type SseSender = mpsc::Sender<Result<Event, Infallible>>;

/// Converts decoded Antigravity events into API-specific SSE events.
pub(crate) trait SseSerializer {
    /// Produces events to send before upstream events arrive.
    fn initial(&mut self) -> Vec<Event> {
        Vec::new()
    }
    /// Converts one upstream event into zero or more SSE events.
    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event>;
    /// Produces final events after the upstream stream ends successfully.
    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
    /// Produces a consumer-visible upstream failure event.
    fn error(&mut self) -> Event {
        Event::default().data(
            serde_json::to_string(&crate::server::error_body(
                "Upstream request failed",
                "upstream_error",
                Some("bad_gateway"),
            ))
            .unwrap_or_default(),
        )
    }
}

/// Pumps serialized upstream events to the client and terminates the stream.
async fn pump<S>(tx: SseSender, upstream: reqwest::Response, mut serializer: S)
where
    S: SseSerializer,
{
    for event in serializer.initial() {
        if tx.send(Ok(event)).await.is_err() {
            return;
        }
    }

    let mut upstream_events = spawn_event_stream(upstream.bytes_stream());
    let mut had_error = false;
    let mut finish = None;

    while let Some(result) = upstream_events.recv().await {
        match result {
            Ok(AntigravityEvent::Finish { reason }) => finish = Some(reason),
            Ok(event) => {
                for out in serializer.handle(event) {
                    if tx.send(Ok(out)).await.is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                tracing::error!("Error reading upstream bytes: {error}");
                had_error = true;
                break;
            }
        }
    }
    had_error |= finish.is_none();

    if had_error {
        let _ = tx.send(Ok(serializer.error())).await;
    } else {
        if let Some(reason) = finish {
            for out in serializer.handle(AntigravityEvent::Finish { reason }) {
                if tx.send(Ok(out)).await.is_err() {
                    return;
                }
            }
        }
        for out in serializer.finish() {
            if tx.send(Ok(out)).await.is_err() {
                return;
            }
        }
    }

    if !had_error {
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    }
}

/// Spawns the SSE pump and returns a keep-alive response.
pub(super) fn spawn_sse<S>(serializer: S, upstream: reqwest::Response) -> Response
where
    S: SseSerializer + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(100);
    tokio::spawn(async move {
        pump(tx, upstream, serializer).await;
    });
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::antigravity::chat::ChatStreamSerializer;

    /// Runs the public SSE response against an isolated local upstream.
    async fn chat_from_upstream(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(
                move || async move { ([("content-type", "text/event-stream")], body) },
            ),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let upstream = reqwest::get(format!("http://{address}/")).await.unwrap();
        let serializer = ChatStreamSerializer {
            stream_id: "test".to_owned(),
            req_model: "model".to_owned(),
            created: 0,
            include_usage: false,
            sent_role: false,
            has_tools: false,
            finish_sent: false,
            tool_call_index: 0,
            usage: crate::openai::Usage::default(),
            saw_usage: false,
        };
        let response = spawn_sse(serializer, upstream);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        server.abort();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn incomplete_and_invalid_streams_never_claim_success() {
        for body in [
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n",
            "data: {\"response\":",
            "data: {\"error\":{\"message\":\"failed\"}}\n\n",
        ] {
            let output = chat_from_upstream(body).await;
            assert!(output.contains("upstream_error"));
            assert!(!output.contains("[DONE]"));
            assert!(!output.contains("\"finish_reason\":\"stop\""));
        }
    }

    #[tokio::test]
    async fn tool_signature_and_length_are_exposed_to_clients() {
        let output = chat_from_upstream(concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[",
            "{\"functionCall\":{\"id\":\"call_1\",\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},",
            "\"thoughtSignature\":\"secret-signature\"}]},\"finishReason\":\"MAX_TOKENS\"}]}}\n\n"
        ))
        .await;
        assert!(output.contains("thought_signature"));
        assert!(output.contains("secret-signature"));
        assert!(output.contains("\"finish_reason\":\"length\""));
        assert!(output.contains("[DONE]"));
    }
}
