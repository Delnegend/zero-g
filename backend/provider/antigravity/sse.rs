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

    while let Some(result) = upstream_events.recv().await {
        match result {
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

    if had_error {
        if let Ok(json_str) = serde_json::to_string(&crate::server::error_body(
            "Upstream request failed",
            "upstream_error",
            Some("bad_gateway"),
        )) {
            let _ = tx.send(Ok(Event::default().data(json_str))).await;
        }
    } else {
        for out in serializer.finish() {
            if tx.send(Ok(out)).await.is_err() {
                return;
            }
        }
    }

    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
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
