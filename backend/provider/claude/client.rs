use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use super::stream::Decoder;

/// Encode bytes as lowercase hexadecimal without intermediate allocations.
pub(super) fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        encoded.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    encoded
}

/// Create a bounded client that never forwards credentials through redirects.
pub(super) fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to build Claude HTTP client: {e}"))
}

/// Serialize and attest the exact request bytes sent to Claude Code's API.
pub(super) fn attest(payload: &Value) -> Result<String, String> {
    let mut body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    let marker = "\"system\":[{\"text\":\"x-anthropic-billing-header:";
    let start = body.find(marker).ok_or("Missing Claude billing header")?;
    let relative = body
        .get(start..)
        .and_then(|s| s.find("cch=00000"))
        .ok_or("Missing Claude attestation placeholder")?;
    let offset = start
        .checked_add(relative)
        .ok_or("Invalid attestation offset")?;
    let end = offset.checked_add(9).ok_or("Invalid attestation offset")?;
    let hash = xxhash_rust::xxh64::xxh64(body.as_bytes(), 0x4d65_9218_e32a_3268) & 0x000f_ffff;
    body.replace_range(offset..end, &format!("cch={hash:05x}"));
    Ok(body)
}

/// Send an attested request with Claude Code OAuth transport headers.
pub(super) async fn send(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    payload: &Value,
) -> Result<reqwest::Response, String> {
    let body = attest(payload)?;
    client.post(url).bearer_auth(token)
        .header("content-type", "application/json").header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,effort-2025-11-24")
        .header("user-agent", format!("claude-cli/{} (external, cli)", super::payload::CLAUDE_VERSION))
        .header("x-app", "cli").body(body).send().await.map_err(|e| format!("Claude request failed: {e}"))
}

/// Dispatches a single upstream event with bounded buffering.
pub(super) async fn consume(
    response: reqwest::Response,
    mut decoder: Decoder,
    tx: Option<mpsc::Sender<Result<Value, String>>>,
) -> Result<Decoder, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Claude stream failed: {e}"))?;
        if buffer.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("Claude stream event too large".into());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&bytes)
                .map_err(|_| "Invalid UTF-8 in Claude stream")?
                .trim_end();
            if let Some(data) = line.strip_prefix("data:") {
                let event: Value = serde_json::from_str(data.trim())
                    .map_err(|e| format!("Invalid Claude stream event: {e}"))?;
                for output in decoder.event(&event)? {
                    if let Some(tx) = &tx {
                        tx.send(Ok(output)).await.map_err(|_| "Downstream closed")?;
                    }
                }
            }
        }
    }
    if !decoder.finished {
        return Err("Claude stream ended before message_stop".into());
    }
    Ok(decoder)
}

/// HTTP transport and stream tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    /// Send a signed replay and tool history to a mock upstream.
    #[tokio::test]
    async fn sends_attested_oauth_request_with_tool_history() {
        use axum::{body::Bytes, http::HeaderMap, routing::post};
        let app = Router::new().route(
            "/",
            post(|headers: HeaderMap, body: Bytes| async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer secret");
                assert!(
                    headers
                        .get("anthropic-beta")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .contains("oauth-2025-04-20")
                );
                assert_eq!(headers.get("x-app").unwrap(), "cli");
                let body = String::from_utf8(body.to_vec()).unwrap();
                assert!(body.contains("_search"));
                assert!(!body.contains("cch=00000"));
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(
                    value
                        .pointer("/messages/0/content/0/text")
                        .and_then(Value::as_str),
                    Some("hello")
                );
                assert_eq!(
                    value.pointer("/tools/0/name").and_then(Value::as_str),
                    Some("_search")
                );
                assert_eq!(
                    value
                        .pointer("/messages/1/content/0/type")
                        .and_then(Value::as_str),
                    Some("text")
                );
                assert_eq!(
                    value
                        .pointer("/messages/1/content/1/type")
                        .and_then(Value::as_str),
                    Some("tool_use")
                );
                assert_eq!(
                    value
                        .pointer("/messages/1/content/2/signature")
                        .and_then(Value::as_str),
                    Some("signed")
                );
                assert_eq!(
                    value
                        .pointer("/messages/1/content/3/type")
                        .and_then(Value::as_str),
                    Some("text")
                );
                assert_eq!(
                    value
                        .pointer("/messages/2/content/0/tool_use_id")
                        .and_then(Value::as_str),
                    Some("call-1")
                );
                "ok"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let req: crate::openai::ChatCompletionRequest = serde_json::from_value(serde_json::json!({"model":"claude","messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"beforeafter","provider_parts":[{"type":"text","text":"before"},{"type":"tool_use","id":"call-1","name":"_search","input":{"q":1}},{"type":"thinking","thinking":"thought","signature":"signed"},{"type":"text","text":"after"}],"tool_calls":[{"id":"call-1","type":"function","function":{"name":"search","arguments":"{\"q\":1}"}}]},{"role":"tool","tool_call_id":"call-1","content":"result"}],"tools":[{"type":"function","function":{"name":"search","parameters":{"type":"object"}}}]})).unwrap();
        let payload = super::super::payload::build(&req).unwrap();
        let response = send(
            &http().unwrap(),
            &format!("http://{address}/"),
            "secret",
            &payload,
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
    }

    /// Never follow upstream redirects while carrying credentials.
    #[tokio::test]
    async fn refuses_redirects() {
        let app = Router::new()
            .route(
                "/",
                get(|| async { axum::response::Redirect::temporary("/target") }),
            )
            .route("/target", get(|| async { "unexpected" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = http()
            .unwrap()
            .get(format!("http://{address}/"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    }

    /// Decode a complete SSE response and reject truncated streams.
    #[tokio::test]
    async fn consumes_real_sse_and_rejects_truncated_streams() {
        let app = Router::new()
            .route("/complete", get(|| async { "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\ndata: {\"type\":\"message_stop\"}\n\n" }))
            .route("/truncated", get(|| async { "data: {\"type\":\"message_start\",\"message\":{}}\n\n" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = reqwest::get(format!("http://{address}/complete"))
            .await
            .unwrap();
        let result = consume(response, Decoder::new("model".into()), None)
            .await
            .unwrap();
        assert_eq!(
            result.message().get("content").and_then(Value::as_str),
            Some("hello")
        );
        assert_eq!(
            result.usage().get("total_tokens").and_then(Value::as_u64),
            Some(3)
        );
        let response = reqwest::get(format!("http://{address}/truncated"))
            .await
            .unwrap();
        assert!(
            consume(response, Decoder::new("model".into()), None)
                .await
                .is_err()
        );
    }
}
