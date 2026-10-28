use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::storage::RequestLog;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::error;

static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

#[derive(Clone)]
pub(crate) struct LogStore {
    pub(crate) db: Arc<toasty::Db>,
    retention_days: i64,
    pub(crate) lock: Arc<Mutex<()>>,
}
impl LogStore {
    #[cfg(test)]
    pub(crate) async fn open(path: &str, retention_days: i64) -> Result<Self, String> {
        let mut store = Self::open_unpruned(path).await?;
        store.set_retention(retention_days).await?;
        Ok(store)
    }

    pub(crate) async fn open_unpruned(path: &str) -> Result<Self, String> {
        if path != ":memory:" {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            if let Err(err) = options.open(path)
                && err.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(err.to_string());
            }
        }
        let db = toasty::Db::builder()
            .models(toasty::models!(
                crate::storage::RequestLog,
                crate::storage::Setting,
                crate::storage::Secret,
                crate::storage::VirtualKey
            ))
            .connect(&format!("turso:{path}"))
            .await
            .map_err(|e| e.to_string())?;
        MIGRATIONS.apply(&db).await.map_err(|e| e.to_string())?;
        Ok(Self {
            db: Arc::new(db),
            retention_days: 30,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) async fn set_retention(&mut self, retention_days: i64) -> Result<(), String> {
        self.retention_days = retention_days;
        self.prune().await
    }

    async fn prune(&self) -> Result<(), String> {
        RequestLog::filter(
            RequestLog::fields()
                .created_at()
                .lt(now().saturating_sub(self.retention_days.saturating_mul(86400))),
        )
        .delete()
        .exec(&mut (*self.db).clone())
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn insert(&self, log: LogEntry) -> Result<(), String> {
        let _guard = self.lock.lock().await;
        toasty::create!(RequestLog {
            created_at: log.created_at,
            provider: log.provider,
            model: log.model,
            path: log.path,
            status: log.status,
            latency_ms: log.latency_ms,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            request_body: log.request_body,
            response_body: log.response_body,
        })
        .exec(&mut (*self.db).clone())
        .await
        .map_err(|e| e.to_string())?;
        self.prune().await
    }

    async fn dashboard(&self) -> Result<Value, String> {
        let _guard = self.lock.lock().await;
        let rows = RequestLog::all()
            .select((
                RequestLog::fields().status(),
                RequestLog::fields().latency_ms(),
                RequestLog::fields().input_tokens(),
                RequestLog::fields().output_tokens(),
            ))
            .exec(&mut (*self.db).clone())
            .await
            .map_err(|e| e.to_string())?;
        let count = rows.len();
        let successes = rows.iter().filter(|r| (200..400).contains(&r.0)).count();
        let errors = rows.iter().filter(|r| r.0 >= 400).count();
        let input_tokens = rows.iter().filter_map(|r| r.2).reduce(i64::saturating_add);
        let output_tokens = rows.iter().filter_map(|r| r.3).reduce(i64::saturating_add);
        // A floating-point average necessarily approximates large integer values.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let avg_latency_ms = if count == 0 {
            0.0
        } else {
            rows.iter().map(|r| r.1 as f64).sum::<f64>() / count as f64
        };
        let summary = json!({"requests": count, "successes": successes, "errors": errors, "input_tokens": input_tokens, "output_tokens": output_tokens, "avg_latency_ms": avg_latency_ms});
        let logs: Vec<LogEntry> = RequestLog::all()
            .order_by(RequestLog::fields().id().desc())
            .limit(200)
            .exec(&mut (*self.db).clone())
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| LogEntry {
                id: r.id,
                created_at: r.created_at,
                provider: r.provider,
                model: r.model,
                path: r.path,
                status: r.status,
                latency_ms: r.latency_ms,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                request_body: r.request_body,
                response_body: r.response_body,
            })
            .collect();
        Ok(json!({"summary": summary, "logs": logs}))
    }
}

#[derive(Serialize)]
struct LogEntry {
    id: i64,
    created_at: i64,
    provider: String,
    model: String,
    path: String,
    status: i64,
    latency_ms: i64,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    request_body: String,
    response_body: String,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub(crate) async fn dashboard(State(store): State<LogStore>) -> Response {
    match store.dashboard().await {
        Ok(data) => Json(data).into_response(),
        Err(err) => {
            error!("Log query failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn capture(State(store): State<LogStore>, req: Request, next: Next) -> Response {
    if req.method() != Method::POST {
        return next.run(req).await;
    }
    let start = Instant::now();
    let path = req.uri().path().to_owned();
    let provider = path.split('/').nth(1).unwrap_or_default().to_owned();
    let (parts, body) = req.into_parts();
    let Ok(body) = to_bytes(body, 2 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let request_body = String::from_utf8_lossy(&body).into_owned();
    let model = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
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

type Completion = Box<dyn FnOnce(Vec<u8>, bool) + Send>;

struct FinalizeStream<S> {
    inner: std::pin::Pin<Box<S>>,
    finish: Option<Completion>,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_and_expires_private_request_logs() {
        let path =
            std::env::temp_dir().join(format!("zero-g-log-test-{}.db", uuid::Uuid::new_v4()));
        let store = LogStore::open(path.to_str().unwrap(), 30).await.unwrap();
        let entry = |created_at| LogEntry {
            id: 0,
            created_at,
            provider: "codex".into(),
            model: "model".into(),
            path: "/codex/v1/responses".into(),
            status: 200,
            latency_ms: 42,
            input_tokens: Some(10),
            output_tokens: Some(3),
            request_body: "private prompt".into(),
            response_body: "private response".into(),
        };
        store
            .insert(entry(now().saturating_sub(31 * 86400)))
            .await
            .unwrap();
        store.insert(entry(now())).await.unwrap();
        let result = store.dashboard().await.unwrap();
        assert_eq!(result["summary"]["requests"], 1);
        assert_eq!(result["summary"]["input_tokens"], 10);
        assert_eq!(result["logs"][0]["request_body"], "private prompt");
        assert_eq!(result["logs"][0]["response_body"], "private response");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

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
