use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::ReceiverStream;

use crate::openai::{ChatCompletionRequest, MessageContent};

const SYSTEM_PROMPT: &str = include_str!("../assets/opencode_system_prompt.txt");
const CORE_TOOLS: &str = include_str!("../assets/core_tools.json");

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

pub(crate) fn is_zen_model(model: &str) -> bool {
    ZEN_MODELS.contains(&model)
}

pub(crate) fn zen_models() -> &'static [&'static str] {
    ZEN_MODELS
}

fn zen_base_url() -> String {
    std::env::var("ZERO_G_ZEN_URL").unwrap_or_else(|_| "http://127.0.0.1:8090/v1".to_string())
}

fn session_id() -> String {
    const ALNUM: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis()) as u64;
    let value = (!(ts * 0x1000 + 1)) & 0xffff_ffff_ffff;
    let hex = format!("{value:012x}");
    let mut rand = String::with_capacity(14);
    // deterministic-ish pseudo-random using timestamp; good enough for attestation
    let mut seed = ts;
    for _ in 0..14 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let idx = (seed as usize) % ALNUM.len();
        rand.push(ALNUM.as_bytes()[idx] as char);
    }
    format!("ses_{hex}{rand}")
}

pub(crate) fn transform(req: &ChatCompletionRequest) -> (Value, Vec<(String, String)>) {
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
    let headers = vec![
        ("Authorization".to_string(), "Bearer public".to_string()),
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

    (body, headers)
}

fn aggregate_sse(sse: &str) -> Value {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish = None;
    let mut id = String::new();
    let mut model = String::new();
    let mut usage = None;
    let mut tool_calls: Vec<Value> = Vec::new();

    for line in sse.lines() {
        if !line.starts_with("data: ") {
            continue;
        }
        let payload = line[6..].trim();
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
                let i = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while tool_calls.len() <= i {
                    tool_calls.push(
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                    );
                }
                if let Some(idv) = tc.get("id").and_then(Value::as_str) {
                    tool_calls[i]["id"] = json!(idv);
                }
                if let Some(n) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    tool_calls[i]["function"]["name"] = json!(format!(
                        "{}{}",
                        tool_calls[i]["function"]["name"].as_str().unwrap_or(""),
                        n
                    ));
                }
                if let Some(a) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    tool_calls[i]["function"]["arguments"] = json!(format!(
                        "{}{}",
                        tool_calls[i]["function"]["arguments"]
                            .as_str()
                            .unwrap_or(""),
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

pub(crate) async fn handle_chat(req: ChatCompletionRequest) -> Response {
    let (body, headers) = transform(&req);
    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", zen_base_url().trim_end_matches('/'));

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
