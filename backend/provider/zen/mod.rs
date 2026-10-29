/// HTTP routes for Zen models and chat completions.
pub(crate) mod routes;
/// Lifecycle management for the local Zen shim.
mod shim;

use axum::Json;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::ReceiverStream;

use crate::openai::{ChatCompletionRequest, MessageContent};
use crate::provider::quota::QuotaSnapshot;
use crate::server::settings::SettingsStore;
use crate::storage::{ProviderCredential, ProviderRecord, Secret};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::put,
};
use serde::Deserialize;
use std::time::Duration;

/// `OpenCode` Console credentials used to retrieve a workspace balance.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsoleConnection {
    /// Console workspace identifier sent as the organization header.
    pub(crate) workspace_id: String,
    /// Console session cookie value.
    pub(crate) session: String,
}

/// Check that a session is a bounded, valid cookie value.
fn session_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
}

/// Save or remove encrypted Console credentials for a Zen provider credential.
pub(crate) async fn connection(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    input: Option<ConsoleConnection>,
) -> Response {
    if let Some(ref input) = input
        && (!session_valid(&input.session)
            || input.workspace_id.is_empty()
            || input.workspace_id.len() > 256
            || reqwest::header::HeaderValue::from_str(&input.workspace_id).is_err())
    {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid Console session or workspace ID",
        )
            .into_response();
    }
    let session_name = format!("credential.{credential}.console_session");
    let workspace_name = format!("credential.{credential}.console_workspace");
    let encrypted = input
        .as_ref()
        .map(|input| {
            Ok::<_, String>((
                store.encrypt(&session_name, &input.session)?,
                store.encrypt(&workspace_name, &input.workspace_id)?,
            ))
        })
        .transpose();
    let Ok(encrypted) = encrypted else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let lock = crate::provider::login::credential_lock(credential).await;
    let _guard = lock.lock().await;
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            let p = ProviderRecord::filter_by_id(provider)
                .first()
                .exec(&mut tx)
                .await?;
            let c = ProviderCredential::filter_by_id(credential)
                .first()
                .exec(&mut tx)
                .await?;
            if !matches!(p, Some(p) if !p.deleted && p.kind == "zen")
                || !matches!(c, Some(c) if c.provider_id == provider)
            {
                return Ok(false);
            }
            if let Some((session, workspace)) = &encrypted {
                Secret::upsert_by_name(session_name.as_str())
                    .value(session.clone())
                    .exec(&mut tx)
                    .await?;
                Secret::upsert_by_name(workspace_name.as_str())
                    .value(workspace.clone())
                    .exec(&mut tx)
                    .await?;
            } else {
                Secret::delete_by_name(&mut tx, &session_name).await?;
                Secret::delete_by_name(&mut tx, &workspace_name).await?;
            }
            tx.commit().await?;
            Ok(true)
        })
        .await;
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Save a Console connection from the dashboard.
async fn save_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
    Json(input): Json<ConsoleConnection>,
) -> Response {
    connection(&store, &provider, &credential, Some(input)).await
}

/// Remove a Console connection from the dashboard.
async fn delete_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
) -> Response {
    connection(&store, &provider, &credential, None).await
}

/// Build dashboard routes for managing Console connections.
pub(crate) fn dashboard_router(app: crate::server::AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/console",
            put(save_console).delete(delete_console),
        )
        .with_state(app.settings)
}

/// System prompt required by the Zen request format.
const SYSTEM_PROMPT: &str = include_str!("../../../assets/opencode_system_prompt.txt");
/// Built-in tool definitions included in Zen requests.
const CORE_TOOLS: &str = include_str!("../../../assets/core_tools.json");
/// Convert Console microcents to a USD balance snapshot.
pub(crate) fn parse_balance(value: &Value) -> Result<QuotaSnapshot, String> {
    let amount = value
        .get("balanceMicroCents")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .filter(|v| v.is_finite())
        .ok_or("OpenCode Console did not return a balance")?
        / 100_000_000.0;
    Ok(QuotaSnapshot::Balance {
        amount,
        currency: "USD",
    })
}

/// Fetch the Console balance using stored workspace and session credentials.
pub(crate) async fn zen_balance(
    store: &SettingsStore,
    credential: &str,
) -> Result<QuotaSnapshot, String> {
    let session = store
        .secret_get(&format!("credential.{credential}.console_session"))
        .await?
        .ok_or("Connect an OpenCode Console session to see the balance")?;
    let workspace = store
        .secret_get(&format!("credential.{credential}.console_workspace"))
        .await?
        .ok_or("Connect an OpenCode Console workspace to see the balance")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Unable to create Console client")?;
    let response = client
        .get("https://opencode.ai/console/api/billing/status")
        .header(
            reqwest::header::COOKIE,
            format!("__Host-console_session={session}"),
        )
        .header("x-org-id", workspace)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Unable to contact OpenCode Console")?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenCode Console returned HTTP {}; reconnect the session if it expired",
            response.status().as_u16()
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| "Invalid OpenCode Console balance response")?;
    parse_balance(&value)
}

/// Zen models exposed by the local provider.
const ZEN_MODELS: &[&str] = &[
    "beam-free",
    "big-pickle",
    "deepseek-v4.1-flash-free",
    "flint-40b-free",
    "glm-5.3-flash-free",
    "mimo-v2.6-flash-free",
    "nemotron-3.5-lightning-free",
    "nemotron-3.5-super-vl-free",
];

/// Generate an OpenCode-shaped session identifier for request attestation.
fn session_id() -> String {
    const ALNUM: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let value = !ts.wrapping_mul(0x1000).wrapping_add(1) & 0xffff_ffff_ffff;
    let hex = format!("{value:012x}");
    let mut rand = String::with_capacity(14);
    // deterministic-ish pseudo-random using timestamp; good enough for attestation
    let mut seed = ts;
    for _ in 0..14 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let idx = usize::try_from(seed % 62).unwrap_or_default();
        if let Some(&byte) = ALNUM.as_bytes().get(idx) {
            rand.push(char::from(byte));
        }
    }
    format!("ses_{hex}{rand}")
}

/// Convert an `OpenAI` chat request into the Zen request and headers.
// serde_json object indexing inserts optional fields into the object constructed below.
#[allow(clippy::indexing_slicing)]
fn transform(req: &ChatCompletionRequest, zen_key: &str) -> (Value, Vec<(String, String)>) {
    let caller_system = req
        .messages
        .iter()
        .filter(|m| m.role == "system" || m.role == "developer")
        .map(|m| {
            m.content
                .as_ref()
                .map(MessageContent::as_text)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages = vec![json!({"role": "system", "content": SYSTEM_PROMPT})];
    if !caller_system.trim().is_empty() {
        messages.push(json!({"role": "user", "content": caller_system}));
    }
    for m in &req.messages {
        if m.role != "system" && m.role != "developer" {
            messages.push(serde_json::to_value(m).unwrap_or(Value::Null));
        }
    }

    let core_tools: Value = serde_json::from_str(CORE_TOOLS).unwrap_or_else(|_| json!([]));
    let core_names: std::collections::HashSet<String> = core_tools
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                })
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let mut tools = core_tools.as_array().cloned().unwrap_or_default();
    if let Some(req_tools) = &req.tools {
        for t in req_tools {
            let name = t.function.name.clone();
            if !core_names.contains(&name)
                && let Ok(v) = serde_json::to_value(t)
            {
                tools.push(v);
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "tools": tools,
        "stream": true,
    });
    if let Some(so) = &req.stream_options {
        body["stream_options"] = serde_json::to_value(so).unwrap_or(Value::Null);
    }
    for (src, dst) in [
        ("max_tokens", "max_completion_tokens"),
        ("max_completion_tokens", "max_completion_tokens"),
    ] {
        if let Some(v) = match src {
            "max_tokens" => req.max_tokens,
            "max_completion_tokens" => req.max_completion_tokens,
            _ => None,
        } {
            body[dst] = json!(v);
        }
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(tc) = &req.tool_choice {
        body["tool_choice"] = tc.clone();
    }

    let s = session_id();
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "User-Agent".to_string(),
            "opencode/latest/2.1.4/cli".to_string(),
        ),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), s.clone()),
        ("x-opencode-session-id".to_string(), s.clone()),
        ("x-session-affinity".to_string(), s.clone()),
        ("x-session-id".to_string(), s),
    ];
    if !zen_key.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {zen_key}")));
    }
    (body, headers)
}

/// Combine streamed chat completion events into a single response object.
// serde_json object indexing updates the known object shapes constructed here.
#[allow(clippy::indexing_slicing)]
#[allow(clippy::too_many_lines)]
fn aggregate_sse(sse: &str) -> Value {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish = None;
    let mut id = String::new();
    let mut model = String::new();
    let mut usage = None;
    let mut tool_calls: Vec<Value> = Vec::new();

    for line in sse.lines() {
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" || payload.is_empty() {
            continue;
        }
        let j: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if id.is_empty() {
            id = j
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        if model.is_empty() {
            model = j
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        if let Some(u) = j.get("usage") {
            usage = Some(u.clone());
        }
        let Some(ch) = j
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            continue;
        };
        if let Some(fr) = ch.get("finish_reason").and_then(Value::as_str) {
            finish = Some(fr.to_string());
        }
        let d = ch.get("delta").cloned().unwrap_or(Value::Null);
        if let Some(c) = d.get("content").and_then(Value::as_str) {
            content.push_str(c);
        }
        if let Some(r) = d.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(r);
        }
        if let Some(tcs) = d.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                let Some(i) = tc
                    .get("index")
                    .and_then(Value::as_u64)
                    .map_or(Some(0), |i| usize::try_from(i).ok())
                else {
                    continue;
                };
                while tool_calls.len() <= i {
                    tool_calls.push(
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                    );
                }
                let Some(call) = tool_calls.get_mut(i) else {
                    continue;
                };
                if let Some(idv) = tc.get("id").and_then(Value::as_str) {
                    call["id"] = json!(idv);
                }
                if let Some(n) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    call["function"]["name"] = json!(format!(
                        "{}{}",
                        call["function"]["name"].as_str().unwrap_or(""),
                        n
                    ));
                }
                if let Some(a) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    call["function"]["arguments"] = json!(format!(
                        "{}{}",
                        call["function"]["arguments"].as_str().unwrap_or(""),
                        a
                    ));
                }
            }
        }
    }

    let mut message = json!({"role":"assistant","content": content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    let tcs: Vec<Value> = tool_calls
        .into_iter()
        .filter(|v| {
            !v["id"].as_str().unwrap_or("").is_empty()
                || !v["function"]["name"].as_str().unwrap_or("").is_empty()
        })
        .collect();
    if !tcs.is_empty() {
        message["tool_calls"] = json!(tcs);
    }

    let mut resp = json!({
        "id": if id.is_empty() { "chatcmpl-zen" } else { &id },
        "object": "chat.completion",
        "created": SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        "model": model,
        "choices": [{"index":0,"message":message,"finish_reason": finish.unwrap_or_else(|| "stop".to_string())}],
    });
    if let Some(u) = usage {
        resp["usage"] = u;
    }
    resp
}

/// Send a chat request through Zen and return the resulting response.
pub(crate) async fn handle_chat(
    req: &ChatCompletionRequest,
    zen_base_url: &str,
    zen_key: &str,
) -> Response {
    let (body, headers) = transform(req, zen_key);
    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", zen_base_url.trim_end_matches('/'));

    let mut rb = client.post(&url);
    for (k, v) in &headers {
        rb = rb.header(k, v);
    }
    let want_stream = req.stream.unwrap_or(false);

    let up = match rb.json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Zen upstream failed: {e}");
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(json!({"error":{"message": msg,"type":"upstream_error","param":null,"code":"bad_gateway"}})),
            )
                .into_response();
        }
    };
    if !up.status().is_success() {
        let status = up.status();
        let text = up.text().await.unwrap_or_default();
        return (
            axum::http::StatusCode::from_u16(status.as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY),
            Json(
                json!({"error":{"message": text},"type":"upstream_error","param":null,"code":null}),
            ),
        )
            .into_response();
    }

    if want_stream {
        // pass through SSE
        let body = up.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(100);
        tokio::spawn(async move {
            let mut stream = std::pin::pin!(body);
            while let Some(chunk) = stream.next().await {
                if let Ok(bytes) = chunk {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if line.starts_with("data: ") || line.starts_with("data:") {
                            let _ = tx
                                .send(Ok(Event::default()
                                    .data(line.trim_start_matches("data:").trim_start())))
                                .await;
                        } else if !line.trim().is_empty() {
                            // forward non-data lines as-is? skip
                        }
                    }
                }
            }
        });
        Sse::new(ReceiverStream::new(rx))
            .keep_alive(KeepAlive::default())
            .into_response()
    } else {
        let text = up.text().await.unwrap_or_default();
        let agg = aggregate_sse(&text);
        Json(agg).into_response()
    }
}

/// Build the OpenAI-compatible list of supported Zen models.
pub(crate) fn models_json() -> Value {
    json!({
        "object": "list",
        "data": ZEN_MODELS.iter().map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "opencode-zen"})).collect::<Vec<_>>()
    })
}

/// Zen provider backend.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "zen",
        label: "Zen (OpenCode)",
        oauth: false,
        api_key: true,
        fixed_id: Some("zen"),
        fixed_name: Some("OpenCode Zen"),
        keyless: true,
        default_base_url: None,
        legacy: super::common::LegacyPolicy {
            key: Some("public"),
            same_base: true,
        },
    };
    fn router(settings: &SettingsStore) -> axum::Router<crate::server::AppState> {
        routes::router(settings)
    }
    fn discover(
        _state: &crate::server::AppState,
        _provider: &super::registry::Provider,
    ) -> impl Future<Output = Result<Vec<Value>, String>> {
        std::future::ready(
            models_json()
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| "Invalid Zen model list".into()),
        )
    }
}

impl super::common::QuotaProvider for Backend {
    async fn quota(
        store: &SettingsStore,
        _provider: &str,
        credential: &str,
    ) -> Result<QuotaSnapshot, String> {
        zen_balance(store, credential).await
    }
}

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> Router {
        dashboard_router(state)
    }
}

/// Start the local Zen shim if no instance is already responding.
pub(super) async fn start() -> Option<tokio::process::Child> {
    shim::ensure_zen_shim("http://127.0.0.1:8090/v1").await
}
