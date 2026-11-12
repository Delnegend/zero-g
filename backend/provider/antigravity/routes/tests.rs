//! Tests for Antigravity routing, credential pooling, and model resolution.

use super::pool::PoolError;
use super::{
    chat_handler, resolve_model, responses_handler, to_model_entry, try_candidates,
    validate_request_media,
};
use crate::provider::antigravity::{
    AntigravityClient, AntigravityModelInfo,
    models::{ResolveError, normalize_models},
};
use crate::provider::common::{
    errors::UpstreamError, request_identity::RequestIdentity, session::RequestSession,
};
use crate::provider::registry::Provider;
use crate::server::{
    AppState,
    settings::{Permissions, SettingsStore},
};
use axum::{Extension, Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use serde_json::Value;

#[test]
fn unsupported_media_returns_bad_request() {
    let message: crate::openai::ChatMessage = serde_json::from_value(serde_json::json!({
        "role": "user",
        "content": [{"type": "video_url", "video_url": {"url": "https://example.com/video.mp4"}}]
    }))
    .expect("valid chat message");
    let response = validate_request_media(&[message]).expect("external URL must be rejected");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pool_retries_only_upstream_auth_and_rate_limit_statuses() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let result = try_candidates(vec!["first", "second"], move |id| {
            let seen = seen.clone();
            async move {
                seen.lock().expect("lock").push(id);
                if id == "first" {
                    Err(PoolError::Upstream(UpstreamError {
                        status: Some(status),
                        message: "upstream".into(),
                    }))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(*calls.lock().expect("lock"), vec!["first", "second"]);
    }
}

#[tokio::test]
async fn pool_stops_on_non_retryable_and_after_exhaustion() {
    for status in [
        None,
        Some(StatusCode::BAD_REQUEST),
        Some(StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let result: Result<(), _> = try_candidates(vec!["first", "second"], move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                Err(PoolError::Upstream(UpstreamError {
                    status,
                    message: "failed".into(),
                }))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
    let result: Result<(), _> = try_candidates(vec!["first", "second"], |_| async {
        Err(PoolError::Upstream(UpstreamError {
            status: Some(StatusCode::TOO_MANY_REQUESTS),
            message: "last failure".into(),
        }))
    })
    .await;
    assert!(
        matches!(result, Err(PoolError::Upstream(error)) if error.status == Some(StatusCode::TOO_MANY_REQUESTS))
    );
    let empty: Result<(), _> = try_candidates(Vec::<&str>::new(), |_| async { Ok(()) }).await;
    assert!(matches!(empty, Err(PoolError::Internal(_))));
    let internal: Result<(), _> = try_candidates(vec!["first", "second"], |_| async {
        Err(PoolError::Internal("credential error".into()))
    })
    .await;
    assert!(matches!(internal, Err(PoolError::Internal(_))));
}

fn models(ids: &[&str]) -> Vec<(String, AntigravityModelInfo)> {
    ids.iter()
        .map(|id| {
            (
                (*id).to_owned(),
                AntigravityModelInfo {
                    display_name: Some("Model (High)".into()),
                    supports_thinking: Some(true),
                    max_tokens: Some(1_000_000),
                    max_output_tokens: Some(128_000),
                    disabled: None,
                    extra: std::collections::HashMap::from([(
                        "metadata".into(),
                        serde_json::json!(42),
                    )]),
                },
            )
        })
        .collect()
}

fn permissions(ids: &[&str]) -> Permissions {
    Permissions {
        providers: vec![],
        models: ids.iter().map(|id| (*id).to_owned()).collect(),
    }
}

#[test]
fn public_modalities_follow_provider_metadata_and_route_support() {
    let info: AntigravityModelInfo = serde_json::from_value(serde_json::json!({
            "supportsImages": true,
            "supportedMimeTypes": {"image/png": true, "audio/wav": true, "video/mp4": true, "application/pdf": true}
        })).unwrap();
    let entry = to_model_entry("fixture".into(), info);
    assert_eq!(
        entry.input_modalities,
        Some(vec![
            "text".into(),
            "image".into(),
            "audio".into(),
            "video".into(),
            "pdf".into()
        ])
    );
    assert_eq!(entry.output_modalities, Some(vec!["text".into()]));
    assert_eq!(entry.supports_tools, None);
}

#[test]
fn alias_metadata_comes_from_the_effective_permitted_default() {
    let low: AntigravityModelInfo = serde_json::from_value(serde_json::json!({
        "maxTokens": 1000, "maxOutputTokens": 100, "supportsImages": false,
        "supportsThinking": true, "marker": "low"
    }))
    .unwrap();
    let high: AntigravityModelInfo = serde_json::from_value(serde_json::json!({
        "maxTokens": 2000, "maxOutputTokens": 200, "supportsImages": true,
        "supportsThinking": true, "marker": "high"
    }))
    .unwrap();
    let variants = vec![("model-low".into(), low), ("model-high".into(), high)];
    let family = normalize_models(variants);
    for (permission, limit, image, marker) in [
        ("model-low", 100, false, "low"),
        ("model-high", 200, true, "high"),
    ] {
        let entry = family[0].entry(&permissions(&[permission])).unwrap().0;
        assert_eq!(entry.id, "model");
        assert_eq!(entry.max_output_tokens, Some(limit));
        assert_eq!(entry.extra["marker"], marker);
        assert_eq!(
            entry.input_modalities.unwrap().iter().any(|m| m == "image"),
            image
        );
        assert_eq!(
            family[0]
                .resolve(None, &permissions(&[permission]))
                .unwrap(),
            permission
        );
    }
    let entry = family[0].entry(&permissions(&["model"])).unwrap().0;
    assert_eq!(entry.context_window, Some(1000));
    assert_eq!(entry.max_output_tokens, Some(100));
    assert_eq!(entry.input_modalities, Some(vec!["text".into()]));
    assert_eq!(entry.reasoning, Some(true));
    assert_eq!(
        entry.supported_reasoning_efforts,
        Some(vec!["low".into(), "high".into()])
    );
    let variants = vec![
        (
            "other-low".into(),
            serde_json::from_value(
                serde_json::json!({"maxTokens":1000,"maxOutputTokens":100,"supportsThinking":true}),
            )
            .unwrap(),
        ),
        (
            "other-high".into(),
            serde_json::from_value(serde_json::json!({})).unwrap(),
        ),
    ];
    let family = normalize_models(variants);
    let entry = family[0].entry(&permissions(&["other"])).unwrap().0;
    assert_eq!(entry.context_window, None);
    assert_eq!(entry.max_output_tokens, None);
    assert_eq!(entry.reasoning, None);
}

#[test]
fn grouped_discovery_and_permissions_match_routing() {
    let ids = ["model-high", "model-low", "model-medium", "unrelated-low"];
    let groups = normalize_models(models(&ids));
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].id, "model");
    assert_eq!(groups[1].id, "unrelated-low");
    let (entry, raw) = groups[0]
        .entry(&permissions(&["model-low"]))
        .expect("visible");
    assert_eq!(entry.name.as_deref(), Some("Model"));
    assert_eq!(entry.display_name.as_deref(), Some("Model"));
    assert_eq!(entry.id, "model");
    assert_eq!(entry.context_window, Some(1_000_000));
    assert_eq!(entry.max_output_tokens, Some(128_000));
    assert_eq!(raw.extra["metadata"], 42);
    assert_eq!(entry.supported_reasoning_efforts, Some(vec!["low".into()]));
    assert_eq!(entry.default_reasoning_effort.as_deref(), Some("low"));
    let all = groups[0]
        .entry(&permissions(&["model"]))
        .expect("visible")
        .0;
    assert_eq!(
        all.supported_reasoning_efforts,
        Some(vec!["low".into(), "medium".into(), "high".into()])
    );
    assert_eq!(all.default_reasoning_effort.as_deref(), Some("medium"));
    let reversed = normalize_models(models(&[
        "unrelated-low",
        "model-medium",
        "model-low",
        "model-high",
    ]));
    let reordered = reversed[0]
        .entry(&permissions(&["model"]))
        .expect("visible")
        .0;
    assert_eq!(
        serde_json::to_value(all).expect("serialize"),
        serde_json::to_value(reordered).expect("serialize")
    );
    assert!(groups[0].entry(&permissions(&["unrelated-low"])).is_none());
    assert_eq!(
        resolve_model(models(&ids), "model", None, &permissions(&["model-low"])),
        Ok("model-low".into())
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model",
            Some("high"),
            &permissions(&["model-low"])
        ),
        Err(ResolveError::Forbidden)
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model-high",
            Some("low"),
            &permissions(&["model-low"])
        ),
        Err(ResolveError::Forbidden)
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model",
            Some("high"),
            &permissions(&["model"])
        ),
        Ok("model-high".into())
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model",
            Some("xhigh"),
            &permissions(&["model"])
        ),
        Ok("model-high".into())
    );
    assert_eq!(
        resolve_model(models(&ids), "model", None, &permissions(&["model"])),
        Ok("model-medium".into())
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model",
            Some("invalid"),
            &permissions(&["model"])
        ),
        Err(ResolveError::Unsupported)
    );
}

#[test]
fn defaults_are_discovered_and_ambiguous_suffixes_stay_separate() {
    let ids = ["model-low", "model-high", "model", "other", "other-high"];
    let groups = normalize_models(models(&ids));
    assert_eq!(
        groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<Vec<_>>(),
        vec!["model", "other", "other-high"]
    );
    assert_eq!(
        resolve_model(models(&ids), "model", None, &permissions(&["model"])),
        Ok("model".into())
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "model",
            Some("medium"),
            &permissions(&["model"])
        ),
        Err(ResolveError::Unsupported)
    );
    assert_eq!(
        resolve_model(models(&ids), "model", None, &permissions(&["model-high"])),
        Ok("model-high".into())
    );
    assert_eq!(
        resolve_model(
            models(&ids),
            "other-high",
            Some("low"),
            &permissions(&["other-high"])
        ),
        Ok("other-high".into())
    );
    let entry = groups[2].entry(&permissions(&[])).expect("visible").0;
    assert_eq!(entry.supported_reasoning_efforts, None);
    assert_eq!(entry.default_reasoning_effort, None);
    assert_eq!(entry.display_name.as_deref(), Some("Model (High)"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn chat_route_preserves_signatures_and_applies_discovered_profile() {
    use crate::storage::{ProviderCredential, ProviderRecord, Secret};
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

    let discovery_count = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let count = discovery_count.clone();
    let fake = Router::new().fallback(axum::routing::post(move |uri: axum::http::Uri, Json(body): Json<Value>| {
            let tx = tx.clone();
            let count = count.clone();
            async move {
                if uri.path().ends_with("fetchAvailableModels") {
                    count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(body["project"], "project");
                    Json(json!({"models": {
                        "model-low": {"supportsThinking": true, "thinkingLevel": 1, "maxOutputTokens": 100},
                        "model-high": {"supportsThinking": true, "thinkingLevel": 3, "maxOutputTokens": 200}
                    }})).into_response()
                } else {
                    if body.pointer("/request/contents").and_then(Value::as_array).is_some_and(|contents| {
                        contents.iter().filter_map(|content| content.get("parts").and_then(Value::as_array))
                            .flatten().filter_map(|part| part.get("functionResponse"))
                            .any(|response| !response.get("response").is_some_and(Value::is_object))
                    }) {
                        return (StatusCode::BAD_REQUEST, "function response must be an object").into_response();
                    }
                    if let Some(text) = body.pointer("/request/contents/0/parts/0/text").and_then(Value::as_str) {
                        let usage = match text {
                            "missing-total" => Some(json!({"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":3})),
                            "missing-prompt" => Some(json!({"candidatesTokenCount":5,"thoughtsTokenCount":3,"totalTokenCount":18})),
                            "inconsistent" => Some(json!({"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":3,"totalTokenCount":6})),
                            _ => None,
                        };
                        if let Some(usage) = usage {
                            let event = json!({"response":{"candidates":[{"content":{"parts":[{"text":"answer"}]},"finishReason":"STOP"}],"usageMetadata":usage}});
                            return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], format!("data: {event}\n\n")).into_response();
                        }
                    }
                    let truncated = body.pointer("/request/contents/0/parts/0/text").and_then(Value::as_str) == Some("truncate");
                    tx.send(body).unwrap();
                    if truncated {
                        return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n").into_response();
                    }
                    ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        concat!("data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[",
                        "{\"text\":\"reason\",\"thought\":true,\"thoughtSignature\":\"reason-sig\"},",
                        "{\"text\":\"answer\",\"thoughtSignature\":\"text-sig\"},",
                        "{\"functionCall\":{\"id\":\"call_1\",\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},",
                        "\"thoughtSignature\":\"opaque\"}]},\"finishReason\":\"STOP\"}],",
                        "\"usageMetadata\":{\"promptTokenCount\":10,\"cachedContentTokenCount\":4,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":3,\"totalTokenCount\":18}}}\n\n")).into_response()
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    let database = crate::database::Database::open(":memory:").await.unwrap();
    let store = SettingsStore::new(
        database.clone(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    toasty::create!(ProviderRecord {
        id: "agy".to_string(),
        name: "agy".to_string(),
        kind: "agy".to_string(),
        base_url: String::new(),
        keyless: false,
        deleted: false
    })
    .exec(&mut (*store.database.db).clone())
    .await
    .unwrap();
    toasty::create!(ProviderCredential {
        id: "credential".to_string(),
        provider_id: "agy".to_string(),
        name: "credential".to_string(),
        weight: 1,
        enabled: true
    })
    .exec(&mut (*store.database.db).clone())
    .await
    .unwrap();
    let name = "credential.credential.credential";
    let credential = json!({"access_token":"fake-token","project_id":"project"}).to_string();
    Secret::upsert_by_name(name)
        .value(store.encrypt(name, &credential).unwrap())
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
    let mut client = AntigravityClient::new();
    client.test_endpoint = Some(format!("http://{address}"));
    let mut providers = crate::provider::Providers::new();
    providers.client = Arc::new(client);
    let state = AppState {
        providers,
        settings: store,
        logs: crate::server::observability::LogStore::new(database),
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .layer(Extension(Permissions {
            providers: vec![],
            models: vec![],
        }))
        .layer(Extension(Provider {
            id: "agy".into(),
            name: "agy".into(),
            kind: "agy".into(),
            base_url: String::new(),
            keyless: false,
        }))
        .layer(Extension(RequestSession {
            id: "scoped-session".into(),
        }))
        .with_state(state);
    let request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let identity = RequestIdentity::default();
    let first = app
        .clone()
        .layer(Extension(identity.clone()))
        .oneshot(request(json!({
            "model":"model", "reasoning_effort":"high", "max_tokens":300,
            "messages":[{"role":"user","content":"hello"}],
            "tools":[{"type":"function","function":{"name":"lookup"}}],
            "tool_choice":{"type":"function","function":{"name":"lookup"}}
        })))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let snapshot = identity.snapshot();
    assert_eq!(snapshot.credential_id.as_deref(), Some("credential"));
    assert_eq!(snapshot.credential_name.as_deref(), Some("credential"));
    let response: Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response["usage"]["prompt_tokens"], 10);
    assert_eq!(response["usage"]["completion_tokens"], 8);
    assert_eq!(response["usage"]["total_tokens"], 18);
    for (text, prompt, total) in [
        ("missing-total", 10, 18),
        ("missing-prompt", 10, 18),
        ("inconsistent", 10, 6),
    ] {
        let result = app
            .clone()
            .oneshot(request(json!({
                "model":"model-high", "messages":[{"role":"user","content":text}]
            })))
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let result: Value = serde_json::from_slice(
            &axum::body::to_bytes(result.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], prompt);
        assert_eq!(result["usage"]["completion_tokens"], 8);
        assert_eq!(result["usage"]["total_tokens"], total);
    }
    let call = response["choices"][0]["message"]["tool_calls"][0].clone();
    let provider_parts = response["choices"][0]["message"]["provider_parts"].clone();
    assert_eq!(
        response["choices"][0]["message"]["reasoning_content"],
        "reason"
    );
    assert_eq!(response["choices"][0]["message"]["content"], "answer");
    assert!(
        provider_parts[0]["thought_signature"]
            .as_str()
            .unwrap()
            .contains("reason-sig")
    );
    assert!(
        provider_parts[1]["thought_signature"]
            .as_str()
            .unwrap()
            .contains("text-sig")
    );
    assert!(
        call["thought_signature"]
            .as_str()
            .unwrap()
            .contains("opaque")
    );
    let payload = rx.recv().await.unwrap();
    assert_eq!(payload["model"], "model-high");
    assert_eq!(payload["request"]["sessionId"], "scoped-session");
    assert_eq!(
        payload["request"]["generationConfig"]["maxOutputTokens"],
        200
    );
    assert_eq!(
        payload["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "HIGH"
    );
    assert_eq!(
        payload["request"]["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
        json!(["lookup"])
    );
    for (effort, expected_model, expected_level) in [
        (Some("low"), "model-low", "LOW"),
        (Some("high"), "model-high", "HIGH"),
        (None, "model-low", "LOW"),
    ] {
        let result = app
            .clone()
            .oneshot(request(json!({
                "model": "model", "reasoning_effort": effort,
                "messages": [{"role":"user", "content":"hello"}]
            })))
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let sent = rx.recv().await.unwrap();
        assert_eq!(sent["model"], expected_model);
        assert_eq!(
            sent["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            expected_level
        );
    }
    let invalid = app
        .clone()
        .oneshot(request(json!({
            "model":"model", "reasoning_effort":"unsupported-fixture",
            "messages":[{"role":"user", "content":"hello"}]
        })))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(rx.try_recv().is_err());
    let stream_identity = RequestIdentity::default();
    let streamed = app
        .clone()
        .layer(Extension(stream_identity.clone()))
        .oneshot(request(json!({
            "model":"model-high", "stream":true, "stream_options":{"include_usage":true},
            "messages":[{"role":"user","content":"hello"}]
        })))
        .await
        .unwrap();
    assert_eq!(streamed.status(), StatusCode::OK);
    let snapshot = stream_identity.snapshot();
    assert_eq!(snapshot.credential_id.as_deref(), Some("credential"));
    assert_eq!(snapshot.credential_name.as_deref(), Some("credential"));
    let bytes = axum::body::to_bytes(streamed.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let output = String::from_utf8(bytes.to_vec()).unwrap();
    let usage = output
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find_map(|chunk| {
            chunk
                .get("usage")
                .filter(|usage| usage.is_object())
                .cloned()
        })
        .unwrap();
    assert_eq!(usage["prompt_tokens"], 10);
    assert_eq!(usage["completion_tokens"], 8);
    assert_eq!(usage["total_tokens"], 18);
    let stream_parts: Vec<Value> = output
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter_map(|chunk| {
            chunk
                .pointer("/choices/0/delta/provider_parts")
                .and_then(Value::as_array)
                .cloned()
        })
        .flatten()
        .collect();
    assert_eq!(Value::Array(stream_parts), provider_parts);
    let _ = rx.recv().await.unwrap();
    let second = app
            .clone()
            .oneshot(request(json!({
                "model":"model", "reasoning_effort":"high",
                "messages":[{"role":"assistant","content":"answer","provider_parts":provider_parts,
                    "tool_calls":[{"id":"unsigned","type":"function","function":{"name":"lookup","arguments":"{}"}},call]}],
                "tools":[{"type":"function","function":{"name":"lookup"}}], "tool_choice":"none"
            })))
            .await
            .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let payload = rx.recv().await.unwrap();
    let parts = &payload["request"]["contents"][0]["parts"];
    assert_eq!(parts[0]["text"], "reason");
    assert_eq!(parts[0]["thoughtSignature"], "reason-sig");
    assert_eq!(parts[1]["text"], "answer");
    assert_eq!(parts[1]["thoughtSignature"], "text-sig");
    assert_eq!(parts[2]["functionCall"]["id"], "unsigned");
    assert_eq!(
        parts[2]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert_eq!(parts[3]["functionCall"]["id"], "call_1");
    assert_eq!(parts[3]["thoughtSignature"], "opaque");
    assert_eq!(
        payload["request"]["toolConfig"]["functionCallingConfig"]["mode"],
        "NONE"
    );
    let parallel = app
        .clone()
        .oneshot(request(json!({
            "model":"model-high",
            "messages":[
                {"role":"assistant","tool_calls":[
                    {"id":"first","type":"function","function":{"name":"one","arguments":"{}"}},
                    {"id":"second","type":"function","function":{"name":"two","arguments":"{}"}}
                ]},
                {"role":"tool","tool_call_id":"second","content":"second result"},
                {"role":"tool","tool_call_id":"unknown","content":"unknown result"},
                {"role":"tool","tool_call_id":"first","content":"first result"}
            ]
        })))
        .await
        .unwrap();
    assert_eq!(parallel.status(), StatusCode::OK);
    let payload = rx.recv().await.unwrap();
    let parts = &payload["request"]["contents"][1]["parts"];
    assert_eq!(parts[0]["functionResponse"]["id"], "first");
    assert_eq!(parts[0]["functionResponse"]["name"], "one");
    assert_eq!(
        parts[0]["functionResponse"]["response"]["output"],
        "first result"
    );
    assert_eq!(parts[1]["functionResponse"]["id"], "unknown");
    assert_eq!(parts[2]["functionResponse"]["id"], "second");
    assert_eq!(parts[2]["functionResponse"]["name"], "two");
    assert_eq!(
        parts[2]["functionResponse"]["response"]["output"],
        "second result"
    );
    let results = app
        .clone()
        .oneshot(request(json!({
            "model": "model-high",
            "messages": [
                {"role":"assistant", "tool_calls": [
                    {"id":"object", "type":"function", "function":{"name":"lookup", "arguments":"{}"}},
                    {"id":"array", "type":"function", "function":{"name":"lookup", "arguments":"{}"}},
                    {"id":"scalar", "type":"function", "function":{"name":"lookup", "arguments":"{}"}},
                    {"id":"malformed", "type":"function", "function":{"name":"lookup", "arguments":"{}"}}
                ]},
                {"role":"tool", "tool_call_id":"object", "content":"{\"ok\":true}"},
                {"role":"tool", "tool_call_id":"array", "content":"[{\"id\":1},null]"},
                {"role":"tool", "tool_call_id":"scalar", "content":"42"},
                {"role":"tool", "tool_call_id":"malformed", "content":"[invalid"}
            ]
        })))
        .await
        .unwrap();
    assert_eq!(results.status(), StatusCode::OK);
    let payload = rx.recv().await.unwrap();
    let parts = payload["request"]["contents"][1]["parts"]
        .as_array()
        .unwrap();
    for (part, (id, expected)) in parts.iter().zip([
        ("object", json!({"ok": true})),
        ("array", json!({"output": [{"id": 1}, null]})),
        ("scalar", json!({"output": 42})),
        ("malformed", json!({"output": "[invalid"})),
    ]) {
        assert_eq!(part["functionResponse"]["id"], id);
        assert_eq!(part["functionResponse"]["name"], "lookup");
        assert_eq!(part["functionResponse"]["response"], expected);
    }
    assert_eq!(parts.len(), 4);
    let response_request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(response_request(json!({
            "model":"model-high", "input":"hello"
        })))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let message = value["output"][0].clone();
    assert_eq!(message["provider_parts"], provider_parts);
    let _ = rx.recv().await.unwrap();
    let streamed_response = app
        .clone()
        .oneshot(response_request(json!({
            "model":"model-high", "input":"hello", "stream":true
        })))
        .await
        .unwrap();
    assert_eq!(streamed_response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(streamed_response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let output = String::from_utf8(bytes.to_vec()).unwrap();
    let events: Vec<Value> = output
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    assert!(
        events.iter().any(
            |event| event["type"] == "response.output_text.delta" && event["delta"] == "answer"
        )
    );
    let arguments = events
        .iter()
        .find(|event| event["type"] == "response.function_call_arguments.done")
        .unwrap();
    assert_eq!(arguments["name"], "lookup");
    assert_eq!(
        serde_json::from_str::<Value>(arguments["arguments"].as_str().unwrap()).unwrap(),
        json!({"q":"x"})
    );
    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["status"], "completed");
    assert_eq!(completed["response"]["output_text"], "answer");
    assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 8);
    assert_eq!(completed["response"]["usage"]["total_tokens"], 18);
    let tool = completed["response"]["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(tool["call_id"], "call_1");
    assert_eq!(tool["name"], "lookup");
    assert_eq!(
        serde_json::from_str::<Value>(tool["arguments"].as_str().unwrap()).unwrap(),
        json!({"q":"x"})
    );
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "response.failed")
    );
    let _ = rx.recv().await.unwrap();
    let truncated_response = app
        .clone()
        .oneshot(response_request(json!({
            "model":"model-high", "input":"truncate", "stream":true
        })))
        .await
        .unwrap();
    assert_eq!(truncated_response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(truncated_response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let output = String::from_utf8(bytes.to_vec()).unwrap();
    let events: Vec<Value> = output
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.output_text.delta"
                && event["delta"] == "partial")
    );
    let failed = events
        .iter()
        .find(|event| event["type"] == "response.failed")
        .unwrap();
    assert_eq!(failed["response"]["status"], "failed");
    assert_eq!(failed["response"]["error"]["code"], "bad_gateway");
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "response.completed")
    );
    let _ = rx.recv().await.unwrap();
    let replay = app
        .clone()
        .oneshot(response_request(json!({
            "model":"model-high", "input":[message]
        })))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let payload = rx.recv().await.unwrap();
    assert_eq!(
        payload["request"]["contents"][0]["parts"][0]["thoughtSignature"],
        "reason-sig"
    );
    assert_eq!(
        payload["request"]["contents"][0]["parts"][1]["thoughtSignature"],
        "text-sig"
    );
    for streaming in [false, true] {
        let response = app
            .clone()
            .oneshot(request(json!({
                "model":"model-high", "stream":streaming,
                "messages":[{"role":"user","content":"truncate"}]
            })))
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();
        if streaming {
            assert_eq!(status, StatusCode::OK);
            assert!(output.contains("upstream_error"));
            assert!(!output.contains("[DONE]"));
        } else {
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert!(output.contains("upstream_error"));
        }
        let _ = rx.recv().await.unwrap();
    }
    assert_eq!(discovery_count.load(Ordering::SeqCst), 1);
    server.abort();
}
