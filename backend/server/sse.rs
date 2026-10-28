use std::convert::Infallible;

use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::antigravity::{AntigravityEvent, spawn_event_stream};

type SseSender = mpsc::Sender<Result<Event, Infallible>>;

pub(crate) trait SseSerializer {
    fn initial(&mut self) -> Vec<Event> {
        Vec::new()
    }
    fn handle(&mut self, event: AntigravityEvent) -> Vec<Event>;
    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

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

pub(super) fn bad_gateway_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(crate::server::error_body(
            "Upstream request failed",
            "upstream_error",
            Some("bad_gateway"),
        )),
    )
        .into_response()
}
