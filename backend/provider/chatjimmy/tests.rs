use axum::{Extension, body::Bytes, extract::State, http::StatusCode};

use super::protocol::{ChatJimmyMessage, ChatJimmyOptions, ChatJimmyRequest, parse_upstream};
use super::routes::chat_handler;
use super::stream::split_stream_chunks;
use crate::provider::registry;
use crate::server::{
    AppState,
    settings::{Permissions, SettingsStore},
};

#[tokio::test]
async fn tool_request_is_rejected_before_upstream() {
    let database = crate::database::Database::open(":memory:").await.unwrap();
    let settings = SettingsStore::new(
        database.clone(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let state = AppState {
        providers: crate::provider::Providers::new(),
        settings,
        logs: crate::server::observability::LogStore::new(database),
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
    };
    let provider = registry::Provider {
        id: "chatjimmy".into(),
        name: "ChatJimmy".into(),
        kind: "chatjimmy".into(),
        base_url: String::new(),
        keyless: true,
    };
    let body = Bytes::from(serde_json::json!({"model":"test", "messages":[{"role":"user","content":"hi"}], "tools":[{"type":"function","function":{"name":"lookup"}}]}).to_string());
    let response = chat_handler(
        State(state),
        Extension(Permissions {
            providers: vec![],
            models: vec![],
        }),
        Extension(provider),
        None,
        body,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn parse_upstream_with_stats() {
    let raw = "Hello world!\n<|stats|>{\"prefill_tokens\":16,\"decode_tokens\":4,\"total_tokens\":20}<|/stats|>";
    let (content, stats) = parse_upstream(raw);
    assert_eq!(content, "Hello world!");
    let stats = stats.expect("expected stats");
    assert_eq!(stats.prefill_tokens, Some(16));
    assert_eq!(stats.decode_tokens, Some(4));
    assert_eq!(stats.total_tokens, Some(20));
}

#[test]
fn parse_upstream_without_stats() {
    let raw = "Just pure text";
    let (content, stats) = parse_upstream(raw);
    assert_eq!(content, "Just pure text");
    assert!(stats.is_none());
}

#[test]
fn chatjimmy_request_serialization() {
    let req = ChatJimmyRequest {
        messages: vec![ChatJimmyMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
        }],
        chat_options: ChatJimmyOptions {
            selected_model: "llama3.1-8B".to_string(),
            system_prompt: "You are a helpful assistant".to_string(),
            top_k: 8,
        },
        attachment: None,
    };
    let val = serde_json::to_value(&req).expect("serialize");
    assert_eq!(val["chatOptions"]["selectedModel"], "llama3.1-8B");
    assert_eq!(
        val["chatOptions"]["systemPrompt"],
        "You are a helpful assistant"
    );
    assert_eq!(val["messages"][0]["content"], "Hello");
}

#[test]
fn split_stream_chunks_roundtrip() {
    let sample = "The article discusses a new regulation in Vietnam. Chuyện máy hỏng khi test!";
    let chunks = split_stream_chunks(sample);
    assert_eq!(chunks.concat(), sample);
}

#[test]
fn split_stream_chunks_multibyte_utf8() {
    let sample = "Chuyện máy hỏng khi test và nguyên tắc không khen điều tệ của Ngọc Vy là hai tình huống mà quy định mới chạm tới.";
    let chunks = split_stream_chunks(sample);
    assert_eq!(chunks.concat(), sample);
}
