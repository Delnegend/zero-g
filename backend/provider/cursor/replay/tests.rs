use serde_json::{Value, json};

use super::build;
use crate::openai::ChatCompletionRequest;

/// Parses a chat request fixture.
fn request(value: Value) -> ChatCompletionRequest {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("invalid fixture: {e}"))
}

/// Replays a paired tool result while excluding the active user message.
#[test]
fn history_replays_tool_results_and_excludes_active_user() {
    let req = request(json!({"model":"model", "messages":[
        {"role":"system","content":"Be concise"}, {"role":"user","content":"old"},
        {"role":"assistant","tool_calls":[{"id":"call|1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Oslo\"}"}}]},
        {"role":"tool","tool_call_id":"call|1","content":"rain"}, {"role":"user","content":"new"}
    ], "tools":[{"type":"function","function":{"name":"weather","parameters":{"type":"object"}}}]}));
    let replay = build(&req).unwrap_or_else(|e| panic!("replay failed: {e}"));
    let state = replay
        .request
        .conversation_state
        .as_ref()
        .unwrap_or_else(|| panic!("missing state"));
    let records: Vec<Value> = state
        .root_prompt_messages_json
        .iter()
        .map(|id| {
            serde_json::from_slice(
                replay
                    .blobs
                    .get(id)
                    .unwrap_or_else(|| panic!("missing blob")),
            )
            .unwrap_or_else(|e| panic!("bad JSON: {e}"))
        })
        .collect();
    assert!(records.iter().any(|v| v["role"] == "tool"
        && v["id"] == "call_1"
        && v["content"][0]["result"] == "rain"));
    assert!(!records.iter().any(|v| v.to_string().contains("new")));
    assert_eq!(state.turns.len(), 1);
    assert_eq!(replay.tools.len(), 1);
    assert_eq!(replay.rules.len(), 1);
    let again = build(&req).unwrap_or_else(|e| panic!("replay failed: {e}"));
    assert_eq!(
        state.turns,
        again.request.conversation_state.unwrap_or_default().turns
    );
}

/// Rejects malformed calls and unsupported remote image URLs.
#[test]
fn rejects_malformed_tool_arguments_and_unsupported_remote_image() {
    let bad_call = request(
        json!({"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"a","type":"function","function":{"name":"f","arguments":"{"}}]}]}),
    );
    assert!(build(&bad_call).is_err());
    let bad_image = request(
        json!({"model":"m","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png"}}]}]}),
    );
    assert!(build(&bad_image).is_err());
}
/// Both API request shapes enforce supported tool-selection modes.
#[test]
fn tool_choice_is_enforced_for_chat_and_responses() {
    for choice in [
        json!("none"),
        json!("auto"),
        json!("required"),
        json!({"type":"function","function":{"name":"weather"}}),
    ] {
        let chat = request(
            json!({"model":"m","messages":[{"role":"user","content":"hello"}],
            "tools":[{"type":"function","function":{"name":"weather"}}], "tool_choice":choice}),
        );
        let response: crate::responses::ResponseCreateRequest = serde_json::from_value(json!({
            "model":"m","input":"hello","tools":[{"type":"function","name":"weather"}],"tool_choice":choice
        })).unwrap_or_else(|error| panic!("invalid fixture: {error}"));
        for req in [chat, response.into_chat_request()] {
            let built = build(&req);
            match choice.as_str() {
                Some("none") => assert_eq!(
                    built.unwrap_or_else(|error| panic!("{error}")).tools.len(),
                    0
                ),
                Some("auto") => assert_eq!(
                    built.unwrap_or_else(|error| panic!("{error}")).tools.len(),
                    1
                ),
                _ => assert!(built.is_err()),
            }
        }
    }
}
/// Builds an authenticated Cursor router with no upstream credential.
async fn tool_choice_router() -> axum::Router {
    use crate::storage::{ProviderRecord, VirtualKey};
    use axum::Router;

    let database = crate::database::Database::open(":memory:")
        .await
        .unwrap_or_else(|error| panic!("database: {error}"));
    let store = crate::server::settings::SettingsStore::new(
        database.clone(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap_or_else(|error| panic!("settings: {error}"));
    let mut db = (*database.db).clone();
    toasty::create!(ProviderRecord {
        id: "cursor".to_string(),
        name: "Cursor".to_string(),
        kind: "cursor".to_string(),
        base_url: String::new(),
        keyless: false,
        deleted: false
    })
    .exec(&mut db)
    .await
    .unwrap_or_else(|error| panic!("provider: {error}"));
    let key = "test-cursor-key";
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    toasty::create!(VirtualKey {
        id: "key".to_string(),
        name: "test".to_string(),
        key_hash: digest.as_ref().to_vec(),
        providers: "[]".to_string(),
        models: "[]".to_string(),
        created_at: 0_i64,
        expires_at: None,
        revoked_at: None
    })
    .exec(&mut db)
    .await
    .unwrap_or_else(|error| panic!("key: {error}"));
    let state = crate::server::AppState {
        providers: crate::provider::Providers::new(),
        settings: store.clone(),
        logs: crate::server::observability::LogStore::new(database),
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
    };
    Router::new()
        .nest("/cursor/{id}", super::super::routes::router(&store))
        .with_state(state)
}

/// The actual authenticated routes reject unsupported selections before credential lookup.
#[tokio::test]
async fn tool_choice_http_routes_validate_before_credential_lookup() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;

    let app = tool_choice_router().await;
    let key = "test-cursor-key";
    for (path, input) in [
        (
            "/cursor/cursor/v1/chat/completions",
            json!({"model":"m","messages":[{"role":"user","content":"hello"}]}),
        ),
        (
            "/cursor/cursor/v1/responses",
            json!({"model":"m","input":"hello"}),
        ),
    ] {
        for (choice, expected) in [
            (json!("none"), StatusCode::BAD_GATEWAY),
            (json!("required"), StatusCode::BAD_REQUEST),
            (
                json!({"type":"function","function":{"name":"weather"}}),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let mut body = input.clone();
            let Some(object) = body.as_object_mut() else {
                panic!("invalid fixture")
            };
            object.insert("tool_choice".into(), choice);
            object.insert(
                "tools".into(),
                json!([{"type":"function","function":{"name":"weather"}}]),
            );
            let request = Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap_or_else(|error| panic!("request: {error}"));
            let response = app
                .clone()
                .oneshot(request)
                .await
                .unwrap_or_else(|error| panic!("router: {error}"));
            assert_eq!(response.status(), expected);
            let bytes = to_bytes(response.into_body(), 1_000_000)
                .await
                .unwrap_or_else(|error| panic!("body: {error}"));
            let body: Value =
                serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("JSON: {error}"));
            let message = body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if expected == StatusCode::BAD_REQUEST {
                assert!(message.contains("tool_choice"));
            } else {
                assert!(message.contains("not configured"));
            }
        }
    }
}
