use super::*;
use crate::database::Database;
use crate::server::settings::SettingsStore;
use crate::storage::{ProviderCredential, ProviderRecord, Secret};
use axum::{Router, body::to_bytes, routing::post};
use std::sync::{Arc, Mutex};

/// Requests seen by the fake Anthropic upstream.
type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

/// Start a fake Anthropic upstream that refuses the listed tokens with 429.
async fn upstream(refused: &'static [&'static str]) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let record = seen.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, body: Bytes| {
            let record = record.clone();
            async move {
                let token = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                record.lock().unwrap().push((headers, body));
                if refused
                    .iter()
                    .any(|refused| token == format!("Bearer {refused}"))
                {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
                    )
                        .into_response();
                }
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (root, seen)
}

/// A gateway state with a Claude provider and two connected accounts, `a` almost always first.
async fn state() -> AppState {
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
    AppState {
        providers: crate::provider::Providers::new(),
        settings,
        database,
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
    }
}

/// Unrestricted virtual key permissions.
fn unrestricted() -> Permissions {
    Permissions {
        providers: vec![],
        models: vec![],
    }
}

/// Send one request through the pass-through to `root`.
async fn send(
    state: &AppState,
    permissions: &Permissions,
    identity: &RequestIdentity,
    root: &str,
    headers: HeaderMap,
    body: &str,
) -> Response {
    forward(
        state,
        permissions,
        Some(identity),
        "session",
        &headers,
        &format!("{root}/v1/messages?beta=true"),
        Bytes::from(body.to_owned()),
    )
    .await
}

/// Claude Code bodies arrive upstream byte for byte; only the credential and betas change.
#[tokio::test]
async fn claude_code_body_is_forwarded_byte_for_byte() {
    let (root, seen) = upstream(&[]).await;
    let state = state().await;
    let identity = RequestIdentity::default();
    let body = r#"{"model":"claude-sonnet-4-6",  "system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.323.abc; cc_entrypoint=cli; cch=1a2b3;"},{"type":"text","text":"You are Claude Code"}],"messages":[{"role":"user","content":"hi"}],"metadata":{"user_id":"client"},"stream":true}"#;
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("virtual-key"));
    headers.insert(
        "anthropic-beta",
        HeaderValue::from_static("fine-grained-tool-streaming-2025-05-14"),
    );
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static("claude-cli/2.1.323 (external, cli)"),
    );
    headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    let response = send(&state, &unrestricted(), &identity, &root, headers, body).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&reply).contains("message_stop"));
    let (headers, received) = seen.lock().unwrap()[0].clone();
    assert_eq!(received.as_ref(), body.as_bytes());
    assert_eq!(headers[header::AUTHORIZATION], "Bearer token-a");
    assert!(headers.get("x-api-key").is_none());
    assert!(headers.get(header::ACCEPT_ENCODING).is_none());
    assert_eq!(
        headers[header::USER_AGENT],
        "claude-cli/2.1.323 (external, cli)"
    );
    assert_eq!(
        headers["anthropic-beta"],
        "fine-grained-tool-streaming-2025-05-14,claude-code-20250219,oauth-2025-04-20"
    );
    assert_eq!(identity.snapshot().credential_id.as_deref(), Some("a"));
}

/// Other Anthropic clients get the Claude Code identity and a valid attestation.
#[tokio::test]
async fn other_clients_are_normalized_and_attested() {
    let (root, seen) = upstream(&[]).await;
    let state = state().await;
    let body = json!({
        "model": "claude/claude-sonnet-4-6",
        "system": "Be brief.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hello there world"}]}],
        "max_tokens": 10
    });
    let response = send(
        &state,
        &unrestricted(),
        &RequestIdentity::default(),
        &root,
        HeaderMap::new(),
        &body.to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (headers, received) = seen.lock().unwrap()[0].clone();
    let sent: Value = serde_json::from_slice(&received).unwrap();
    assert_eq!(sent["model"], "claude-sonnet-4-6");
    let billing = sent["system"][0]["text"].as_str().unwrap();
    assert!(billing.starts_with(BILLING_PREFIX) && !billing.contains("cch=00000"));
    assert_eq!(sent["system"][1]["text"], payload::IDENTITY);
    assert_eq!(sent["system"][2]["text"], "Be brief.");
    let user: Value = serde_json::from_str(sent["metadata"]["user_id"].as_str().unwrap()).unwrap();
    assert_eq!(user["session_id"], "session");
    assert_eq!(user["account_uuid"], "acct");
    // Recomputing the attestation over the same body reproduces the bytes sent.
    let mut unsigned = sent.clone();
    unsigned["system"][0]["text"] = json!(reset_attestation(billing));
    assert_eq!(
        client::attest(&unsigned).unwrap(),
        String::from_utf8(received.to_vec()).unwrap()
    );
    assert_eq!(headers[header::USER_AGENT], client::user_agent());
    assert_eq!(headers["x-app"], "cli");
}

/// A refused account falls through to the next; when all refuse, the refusal is returned.
#[tokio::test]
async fn refused_accounts_fail_over() {
    let state = state().await;
    let body =
        json!({"model": "claude-haiku-4-5", "messages": [{"role": "user", "content": "hi"}]})
            .to_string();
    let (root, seen) = upstream(&["token-a"]).await;
    let identity = RequestIdentity::default();
    let response = send(
        &state,
        &unrestricted(),
        &identity,
        &root,
        HeaderMap::new(),
        &body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let tokens: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(headers, _)| headers[header::AUTHORIZATION].clone())
        .collect();
    assert_eq!(tokens, ["Bearer token-a", "Bearer token-b"]);
    assert_eq!(identity.snapshot().credential_id.as_deref(), Some("b"));

    let (root, _) = upstream(&["token-a", "token-b"]).await;
    let response = send(
        &state,
        &unrestricted(),
        &RequestIdentity::default(),
        &root,
        HeaderMap::new(),
        &body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let reply: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(reply["error"]["message"], "slow down");
}

/// Key restrictions are enforced before anything is sent upstream.
#[tokio::test]
async fn key_restrictions_are_enforced_before_sending() {
    let (root, seen) = upstream(&[]).await;
    let state = state().await;
    let body = json!({"model": "claude-opus-4-8", "messages": []}).to_string();
    for permissions in [
        Permissions {
            providers: vec![],
            models: vec!["claude-haiku-4-5".into()],
        },
        Permissions {
            providers: vec!["codex".into()],
            models: vec![],
        },
    ] {
        let response = send(
            &state,
            &permissions,
            &RequestIdentity::default(),
            &root,
            HeaderMap::new(),
            &body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let reply: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(reply["type"], "error");
        assert_eq!(reply["error"]["type"], "permission_error");
    }
    assert!(seen.lock().unwrap().is_empty());
}
