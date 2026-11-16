use std::borrow::Cow;

use axum::http::StatusCode;
use serde_json::Value;
use sha2::Digest;
use crate::provider::common::cooldown;
use crate::provider::common::pool::{PoolError, eligible_by, rank_accounts};
use crate::provider::credentials::Credential;
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

/// Anthropic API origin used with subscription tokens.
pub(super) const API_ROOT: &str = "https://api.anthropic.com";

/// Beta flags a subscription token needs to be accepted as Claude Code.
pub(super) const REQUIRED_BETAS: [&str; 2] = ["claude-code-20250219", "oauth-2025-04-20"];
/// Beta flags sent by translated requests.
const TRANSLATED_BETAS: &str =
    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,effort-2025-11-24";

/// User agent of the Claude Code release the wire format follows.
pub(super) fn user_agent() -> String {
    format!(
        "claude-cli/{} (external, cli)",
        super::payload::CLAUDE_VERSION
    )
}

/// Send an attested request with Claude Code OAuth transport headers.
pub(super) async fn send(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    payload: &Value,
) -> Result<reqwest::Response, String> {
    let body = attest(payload)?;
    client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", TRANSLATED_BETAS)
        .header("user-agent", user_agent())
        .header("x-app", "cli")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Claude request failed: {e}"))
}

/// Send a prepared body and caller-chosen headers with a subscription token.
pub(super) async fn forward(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    headers: reqwest::header::HeaderMap,
    body: axum::body::Bytes,
) -> Result<reqwest::Response, String> {
    client
        .post(url)
        .headers(headers)
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Claude request failed: {e}"))
}

/// Claude Code's `metadata.user_id`: a stable device per credential plus the session.
pub(super) fn user_id(credential: &str, session: &str, account: Option<&str>) -> String {
    let digest = sha2::Sha256::digest(format!("zero-g-claude-device:{credential}").as_bytes());
    let mut identity = serde_json::json!({"device_id": hex(&digest), "session_id": session});
    if let Some(account) = account
        && let Some(object) = identity.as_object_mut()
    {
        object.insert("account_uuid".into(), serde_json::json!(account));
    }
    identity.to_string()
}

/// Why no credential produced a usable response.
pub(super) enum Refusal {
    /// Every connected credential is cooling down; retry after this long.
    CoolingDown(std::time::Duration),
    /// A gateway-side failure, reported with this status and message.
    Failed(StatusCode, String),
}

/// Claude's per-credential, per-model cooldown key.
fn cooldown_key(credential: &Credential, model: &str) -> String {
    format!("claude:{}:{}:{}", credential.id.len(), credential.id, model)
}

/// Try connected credentials in ranked order until one is not refused.
///
/// A 401, 403, 429, or 503 moves on to the next credential. Rate limits are
/// scoped to the requested model; other refusals take precedence over a 429.
pub(super) async fn first_accepted_at<F, Fut>(
    store: &crate::server::settings::SettingsStore,
    provider: &crate::provider::registry::Provider,
    catalog_base: &str,
    model: &str,
    mut send: F,
) -> Result<(reqwest::Response, Credential), Refusal>
where
    F: FnMut(super::auth::Credentials, &Credential) -> Fut,
    Fut: Future<Output = Result<reqwest::Response, String>>,
{
    let credentials = match eligible_by(store, provider, |credential| {
        Cow::Owned(cooldown_key(credential, model))
    })
    .await
    {
        Ok(credentials) => credentials,
        Err(PoolError::Cooling(wait)) => return Err(Refusal::CoolingDown(wait)),
        Err(PoolError::Internal(message)) => {
            return Err(Refusal::Failed(StatusCode::INTERNAL_SERVER_ERROR, message));
        }
        Err(err) => {
            return Err(Refusal::Failed(
                StatusCode::SERVICE_UNAVAILABLE,
                err.to_string(),
            ));
        }
    };
    let http = http().map_err(|e| Refusal::Failed(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut failure = None;
    let mut candidates = Vec::with_capacity(credentials.len());
    for credential in credentials {
        let creds = match super::auth::load(store, &provider.id, &credential.id).await {
            Ok(creds) => creds,
            Err(e) => {
                failure = Some(Refusal::Failed(StatusCode::SERVICE_UNAVAILABLE, e));
                continue;
            }
        };
        let catalog =
            super::models::account_catalog(&http, catalog_base, &credential.id, &creds.access_token)
                .await;
        candidates.push(((credential, creds), catalog));
    }
    let ranked = rank_accounts(candidates, model);
    let mut last = None;
    let mut limited = None;
    for (credential, creds) in ranked {
        let cooldown_id = cooldown_key(&credential, model);
        let response = match send(creds, &credential).await {
            Ok(response) => response,
            Err(e) => {
                failure = Some(Refusal::Failed(StatusCode::BAD_GATEWAY, e));
                continue;
            }
        };
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            super::models::invalidate_catalog_on_auth_error(&credential.id, Some(status)).await;
            last = Some((response, credential));
            continue;
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            cooldown::start(&cooldown_id, cooldown::wait_for(response.headers()));
            limited = Some((response, credential));
            continue;
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            last = Some((response, credential));
            continue;
        }
        cooldown::clear(&cooldown_id);
        return Ok((response, credential));
    }
    if let Some(refused) = last {
        return Ok(refused);
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    if let Some(refused) = limited {
        return Ok(refused);
    }
    Err(Refusal::Failed(
        StatusCode::SERVICE_UNAVAILABLE,
        "No connected Claude credentials".to_owned(),
    ))
}

/// Send a translated request using the first available credential.
pub(super) async fn inference(
    store: &crate::server::settings::SettingsStore,
    provider: &crate::provider::registry::Provider,
    model: &str,
    session: &str,
    payload: &Value,
) -> Result<(reqwest::Response, Credential), Refusal> {
    inference_at(store, provider, API_ROOT, model, session, payload).await
}

/// Send a translated request against a configurable Anthropic root.
pub(super) async fn inference_at(
    store: &crate::server::settings::SettingsStore,
    provider: &crate::provider::registry::Provider,
    base: &str,
    model: &str,
    session: &str,
    payload: &Value,
) -> Result<(reqwest::Response, Credential), Refusal> {
    let http = http().map_err(|e| Refusal::Failed(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let base = base.trim_end_matches('/');
    let url = format!("{base}/v1/messages?beta=true");
    first_accepted_at(store, provider, base, model, |creds, credential| {
        let mut payload = payload.clone();
        if let Some(object) = payload.as_object_mut() {
            let user_id = user_id(&credential.id, session, creds.account_id.as_deref());
            object.insert("metadata".into(), serde_json::json!({ "user_id": user_id }));
        }
        let (http, url) = (&http, &url);
        async move { send(http, url, &creds.access_token, &payload).await }
    })
    .await
}


/// HTTP transport and stream tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use super::super::{downstream::consume, stream::Decoder};
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

    /// Route to the account advertising a model, aggregate discovery across accounts, and keep unlisted models reachable.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn routes_by_account_catalog_aggregates_discovery_and_allows_unlisted() {
        use crate::database::Database;
        use crate::provider::registry::Provider;
        use crate::server::{AppState, settings::SettingsStore};
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use axum::{Json, http::HeaderMap, routing::post};
        use serde_json::json;
        use std::sync::{Arc, Mutex};

        let database = Database::open(":memory:").await.unwrap();
        let settings = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*database.db).clone();
        toasty::create!(ProviderRecord {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            kind: "claude".to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        for (id, weight) in [("a", 1_000_000_000), ("b", 1)] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: "claude".to_string(),
                name: format!("account {id}"),
                weight,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
            let name = format!("credential.{id}.credential");
            let credential = json!({
                "access_token": format!("token-{id}"),
                "refresh_token": "refresh",
                "expires_at": i64::MAX,
                "account_id": "acct"
            });
            Secret::upsert_by_name(name.as_str())
                .value(settings.encrypt(&name, &credential.to_string()).unwrap())
                .exec(&mut db)
                .await
                .unwrap();
        }
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings,
            database,
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let provider = Provider {
            id: "claude".into(),
            name: "Claude Code".into(),
            kind: "claude".into(),
            base_url: String::new(),
            keyless: false,
        };

        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let seen_messages = seen.clone();
        let app = Router::new()
            .route(
                "/v1/models",
                get(|headers: HeaderMap| async move {
                    let auth = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default();
                    if auth == "Bearer token-a" {
                        Json(json!({"data":[{"id":"model-x"},{"id":"shared"}],"has_more":false}))
                    } else {
                        Json(json!({"data":[{"id":"model-y"},{"id":"shared"}],"has_more":false}))
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |headers: HeaderMap, body: axum::body::Bytes| {
                    let seen_messages = seen_messages.clone();
                    async move {
                        let auth = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        let model = value["model"].as_str().unwrap_or_default().to_owned();
                        seen_messages.lock().unwrap().push((auth, model));
                        Json(json!({"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"test","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let discovered = super::super::models::discover_at(&state, &provider, &root, None)
            .await
            .unwrap();
        let ids: Vec<_> = discovered.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"model-x"));
        assert!(ids.contains(&"model-y"));
        assert!(ids.contains(&"shared"));

        let req_y: crate::openai::ChatCompletionRequest = serde_json::from_value(json!({
            "model": "model-y",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        let payload_y = super::super::payload::build(&req_y).unwrap();
        let (resp_y, cred_y) =
            inference_at(&state.settings, &provider, &root, "model-y", "sess", &payload_y)
                .await
                .unwrap_or_else(|_| panic!("expected model-y to succeed"));
        assert!(resp_y.status().is_success());
        assert_eq!(cred_y.id, "b");

        let req_z: crate::openai::ChatCompletionRequest = serde_json::from_value(json!({
            "model": "model-z",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        let payload_z = super::super::payload::build(&req_z).unwrap();
        let (resp_z, _) =
            inference_at(&state.settings, &provider, &root, "model-z", "sess", &payload_z)
                .await
                .unwrap_or_else(|_| panic!("expected unlisted model-z to reach pool"));
        assert!(resp_z.status().is_success());

        let recorded = seen.lock().unwrap();
        assert_eq!(recorded[0], ("Bearer token-b".to_owned(), "model-y".to_owned()));
        assert_eq!(recorded[1].1, "model-z");
    }
}
