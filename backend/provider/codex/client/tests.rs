use super::*;

/// Feeds raw upstream SSE through the parser and the accumulator, mirroring
/// the non-streaming paths.
fn collect(sse: &str) -> Vec<PendingToolCall> {
    let mut decoder = SseLineDecoder::default();
    let mut calls = Vec::new();
    for ev in decoder.push_chunk(sse.as_bytes()).unwrap() {
        apply_tool_event(&mut calls, ev);
    }
    calls
}

fn added(id: &str, call_id: &str, name: &str) -> String {
    format!(
        "event: response.output_item.added\ndata: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"{id}\",\"call_id\":\"{call_id}\",\"name\":\"{name}\",\"arguments\":\"\"}}}}\n\n"
    )
}

fn args_done(id: &str, arguments: &str) -> String {
    let args = serde_json::to_string(arguments).unwrap();
    format!(
        "event: response.function_call_arguments.done\ndata: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"{id}\",\"arguments\":{args}}}\n\n"
    )
}

fn item_done(id: &str, call_id: &str, name: &str, arguments: &str) -> String {
    let args = serde_json::to_string(arguments).unwrap();
    format!(
        "event: response.output_item.done\ndata: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"function_call\",\"id\":\"{id}\",\"call_id\":\"{call_id}\",\"name\":\"{name}\",\"arguments\":{args}}}}}\n\n"
    )
}

fn args_delta(id: &str, delta: &str) -> String {
    let delta = serde_json::to_string(delta).unwrap();
    format!(
        "event: response.function_call_arguments.delta\ndata: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"{id}\",\"delta\":{delta}}}\n\n"
    )
}

/// Parallel calls: the backend sends no argument deltas at all, only the
/// completed item. Before the fix every call kept `arguments: ""`.
#[test]
fn test_parallel_tool_calls_take_arguments_from_done_events() {
    let mut sse = String::new();
    sse += &added("fc_1", "call_1", "grep");
    sse += &args_done("fc_1", "{\"pattern\":\"foo\",\"path\":\"/tmp\"}");
    sse += &item_done(
        "fc_1",
        "call_1",
        "grep",
        "{\"pattern\":\"foo\",\"path\":\"/tmp\"}",
    );
    sse += &added("fc_2", "call_2", "read");
    sse += &args_done("fc_2", "{\"path\":\"/etc/hostname\"}");
    sse += &item_done("fc_2", "call_2", "read", "{\"path\":\"/etc/hostname\"}");
    sse += &added("fc_3", "call_3", "ping");
    sse += &args_done("fc_3", "{\"text\":\"hi\"}");
    sse += &item_done("fc_3", "call_3", "ping", "{\"text\":\"hi\"}");

    let calls = collect(&sse);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].name, "grep");
    assert_eq!(
        calls[0].arguments,
        "{\"pattern\":\"foo\",\"path\":\"/tmp\"}"
    );
    assert_eq!(calls[1].name, "read");
    assert_eq!(calls[1].arguments, "{\"path\":\"/etc/hostname\"}");
    assert_eq!(calls[2].name, "ping");
    assert_eq!(calls[2].arguments, "{\"text\":\"hi\"}");
}

/// Single call: deltas arrive first and the `done` event repeats the full
/// arguments; replacing must not duplicate them.
#[test]
fn test_single_tool_call_deltas_then_done_are_not_duplicated() {
    let mut sse = String::new();
    sse += &added("fc_1", "call_1", "ping");
    for delta in ["{\"", "text", "\":\"", "hi", "\"}"] {
        sse += &args_delta("fc_1", delta);
    }
    sse += &args_done("fc_1", "{\"text\":\"hi\"}");
    sse += &item_done("fc_1", "call_1", "ping", "{\"text\":\"hi\"}");

    let calls = collect(&sse);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, "{\"text\":\"hi\"}");
}

#[test]
fn test_tool_delta_can_match_call_id() {
    let mut sse = String::new();
    sse += &added("fc_1", "call_1", "ping");
    sse += "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"call_id\":\"call_1\",\"delta\":\"{\\\"text\\\":\\\"hi\\\"}\"}\n\n";

    let calls = collect(&sse);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, "{\"text\":\"hi\"}");
}

#[test]
fn test_tool_call_arguments_as_json_object() {
    let mut sse = String::new();
    sse += "event:response.output_item.added\ndata:{\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n";
    sse += "event:response.output_item.done\ndata:{\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":{\"path\":\"Cargo.toml\"}}}\n\n";

    let calls = collect(&sse);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, "{\"path\":\"Cargo.toml\"}");
}
/// Starts an isolated upstream and captures requests made by the real client.
async fn fake_client(
    sse: impl Into<String>,
) -> (
    CodexClient,
    tokio::sync::mpsc::Receiver<(axum::http::HeaderMap, Value)>,
) {
    use axum::{Json, Router, routing::post};
    let sse = sse.into();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let app = Router::new().route(
        "/codex/responses",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let tx = tx.clone();
                let sse = sse.clone();
                async move {
                    tx.send((headers, body)).await.unwrap();
                    ([("content-type", "text/event-stream")], sse)
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut client = CodexClient::new();
    client.base_url = format!("http://{address}");
    (client, rx)
}

/// Returns an inert test credential.
fn credentials() -> Credentials {
    Credentials {
        access_token: "test-token".to_string(),
        refresh_token: None,
        account_id: None,
        email: None,
        plan_type: None,
        residency: None,
        expires_at: None,
    }
}

/// Constructs a minimal chat request.
fn chat_request() -> ChatCompletionRequest {
    serde_json::from_value(
        serde_json::json!({"model":"test","messages":[{"role":"user","content":"hello"}]}),
    )
    .unwrap()
}

/// Reads the consumer-visible SSE response.
async fn stream_body(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn oversized_sse_is_reported_by_all_consumers() {
    let cases = [
        ("x".repeat(MAX_SSE_BYTES + 1), "line exceeds size limit"),
        (
            format!(
                "data: {}\ndata: {}\n\n",
                "x".repeat(MAX_SSE_BYTES / 2),
                "y".repeat(MAX_SSE_BYTES / 2)
            ),
            "event data exceeds size limit",
        ),
    ];
    let responses: ResponseCreateRequest =
        serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
    for (sse, expected) in cases {
        let sse = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"before\"}}\n\n{sse}"
        );
        let (client, _requests) = fake_client(sse).await;
        let chat_error = client
            .complete_chat_non_streaming(&credentials(), &chat_request(), "test")
            .await
            .unwrap_err();
        assert!(chat_error.message.contains(expected));
        let response_error = client
            .complete_responses_non_streaming(&credentials(), &responses, "test")
            .await
            .unwrap_err();
        assert!(response_error.message.contains(expected));
        let body = stream_body(
            client
                .stream_chat(&credentials(), &chat_request(), "test")
                .await
                .unwrap(),
        )
        .await;
        assert!(body.contains("event: error") && body.contains(expected));
        assert!(body.find("before").unwrap() < body.find("event: error").unwrap());
        assert!(!body.contains("[DONE]"));
        let body = stream_body(
            client
                .stream_responses(&credentials(), &responses, "test")
                .await
                .unwrap(),
        )
        .await;
        assert!(body.contains("event: error") && body.contains(expected));
        assert!(body.find("before").unwrap() < body.find("event: error").unwrap());
    }
}

#[test]
fn split_multiline_sse_event_is_decoded() {
    let mut decoder = SseLineDecoder::default();
    assert!(
        decoder
            .push_chunk(b"data: {\"type\":\"response.output_text.delta\",\nda")
            .unwrap()
            .is_empty()
    );
    let events = decoder.push_chunk(b"ta: \"delta\":\"hello\"}\n\n").unwrap();
    assert!(matches!(&events[..], [CodexParsedEvent::TextDelta(text)] if text == "hello"));
}

#[test]
fn complete_events_precede_size_errors_regardless_of_chunking() {
    let frame = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"before\"}\n\n";
    for suffix in [
        "x".repeat(MAX_SSE_BYTES + 1),
        format!(
            "data: {}\ndata: {}\n",
            "x".repeat(MAX_SSE_BYTES / 2),
            "y".repeat(MAX_SSE_BYTES / 2)
        ),
    ] {
        for split in [false, true] {
            let mut decoder = SseLineDecoder::default();
            let mut input = Vec::new();
            if split {
                assert!(
                    matches!(&decoder.push_chunk(frame).unwrap()[..], [CodexParsedEvent::TextDelta(text)] if text == "before")
                );
            } else {
                input.extend_from_slice(frame);
            }
            input.extend_from_slice(suffix.as_bytes());
            let (events, error) = decoder.push_chunk(&input).unwrap_err();
            assert!(error.contains("size limit"));
            if split {
                assert!(events.is_empty());
            } else {
                assert!(
                    matches!(&events[..], [CodexParsedEvent::TextDelta(text)] if text == "before")
                );
            }
        }
    }
}

#[tokio::test]
async fn responses_terminal_before_oversized_data_remains_successful() {
    let sse = format!(
        "data: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"output\":[]}}}}\n\n{}",
        "x".repeat(MAX_SSE_BYTES + 1)
    );
    let (client, _requests) = fake_client(sse).await;
    let request =
        serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
    let response = client
        .complete_responses_non_streaming(&credentials(), &request, "test")
        .await
        .unwrap();
    assert_eq!(response["status"], "completed");
}

#[tokio::test]
async fn responses_stream_stops_at_terminal_before_invalid_trailing_data() {
    let sse = format!(
        "data: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"output\":[]}}}}\n\n{}",
        "x".repeat(MAX_SSE_BYTES + 1)
    );
    let (client, _requests) = fake_client(sse).await;
    let request =
        serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
    let body = stream_body(
        client
            .stream_responses(&credentials(), &request, "test")
            .await
            .unwrap(),
    )
    .await;
    assert!(body.contains("response.completed"));
    assert!(!body.contains("event: error"));
}

#[tokio::test]
async fn chat_stream_includes_usage_only_when_requested() {
    let sse = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"total_tokens\":17}}}\n\n"
    );
    for include_usage in [None, Some(false), Some(true)] {
        let (client, _requests) = fake_client(sse).await;
        let mut request = chat_request();
        request.stream_options = include_usage.map(|include_usage| crate::openai::StreamOptions {
            include_usage: Some(include_usage),
        });
        let body = stream_body(
            client
                .stream_chat(&credentials(), &request, "test")
                .await
                .unwrap(),
        )
        .await;
        let events: Vec<&str> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        assert_eq!(events.last(), Some(&"[DONE]"));
        let chunks: Vec<Value> = events[..events.len() - 1]
            .iter()
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        let usage_chunks: Vec<&Value> = chunks
            .iter()
            .filter(|chunk| chunk.get("usage").is_some_and(|usage| !usage.is_null()))
            .collect();
        if include_usage == Some(true) {
            assert_eq!(usage_chunks.len(), 1);
            let usage = usage_chunks[0];
            assert_eq!(usage["choices"], serde_json::json!([]));
            assert_eq!(
                usage["usage"],
                serde_json::json!({"prompt_tokens":12,"completion_tokens":5,"total_tokens":17})
            );
            assert_eq!(chunks.last(), Some(usage));
            assert_eq!(
                chunks[chunks.len() - 2]["choices"][0]["finish_reason"],
                "stop"
            );
            assert!(
                chunks[..chunks.len() - 1]
                    .iter()
                    .all(|chunk| chunk.get("usage") == Some(&Value::Null))
            );
        } else {
            assert_eq!(usage_chunks, Vec::<&Value>::new());
        }
    }
}

#[tokio::test]
async fn missing_upstream_usage_is_not_fabricated() {
    for sse in [
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":null}}\n\n",
    ] {
        let (client, _requests) = fake_client(sse).await;
        let mut request = chat_request();
        request.stream_options = Some(crate::openai::StreamOptions {
            include_usage: Some(true),
        });
        let body = stream_body(
            client
                .stream_chat(&credentials(), &request, "test")
                .await
                .unwrap(),
        )
        .await;
        let events: Vec<&str> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        assert_eq!(events.last(), Some(&"[DONE]"));
        let last: Value = serde_json::from_str(events[events.len() - 2]).unwrap();
        assert_eq!(last["choices"], serde_json::json!([]));
        assert_eq!(last.get("usage"), Some(&Value::Null));
    }
}

/// Missing or truncated terminal events never produce successful completion.
#[tokio::test]
async fn truncated_streams_report_errors() {
    let (client, _requests) =
        fake_client("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n")
            .await;
    let request = chat_request();
    assert!(
        client
            .complete_chat_non_streaming(&credentials(), &request, "scoped")
            .await
            .is_err()
    );
    let body = stream_body(
        client
            .stream_chat(&credentials(), &request, "scoped")
            .await
            .unwrap(),
    )
    .await;
    assert!(body.contains("partial"));
    assert!(body.contains("event: error"));
    assert!(!body.contains("[DONE]"));
    let responses: ResponseCreateRequest =
        serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
    assert!(
        client
            .complete_responses_non_streaming(&credentials(), &responses, "scoped")
            .await
            .is_err()
    );
    let body = stream_body(
        client
            .stream_responses(&credentials(), &responses, "scoped")
            .await
            .unwrap(),
    )
    .await;
    assert!(body.contains("event: error"));
}

/// Incomplete and failed terminal statuses remain visible to consumers.
#[tokio::test]
async fn terminal_statuses_are_preserved() {
    for status in ["incomplete", "failed"] {
        let sse = if status == "failed" {
            "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"failed\",\"output\":[],\"error\":{\"message\":\"failed\"}}}\n\n"
        } else {
            "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"incomplete\",\"output\":[],\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
        };
        let (client, _requests) = fake_client(sse).await;
        let response_request: ResponseCreateRequest =
            serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
        let response = client
            .complete_responses_non_streaming(&credentials(), &response_request, "scoped")
            .await
            .unwrap();
        assert_eq!(response["status"], status);
        let chat = client
            .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped")
            .await;
        if status == "failed" {
            assert!(chat.is_err());
            let body = stream_body(
                client
                    .stream_chat(&credentials(), &chat_request(), "scoped")
                    .await
                    .unwrap(),
            )
            .await;
            assert!(body.contains("event: error"));
            assert!(!body.contains("[DONE]"));
        } else {
            assert_eq!(
                chat.unwrap().choices[0].finish_reason.as_deref(),
                Some("length")
            );
        }
    }
}

/// Both APIs send the middleware-derived identity as headers and cache key.
#[tokio::test]
async fn scoped_identity_reaches_both_upstream_apis() {
    let (client, mut requests) = fake_client("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
    let req: ResponseCreateRequest =
        serde_json::from_value(serde_json::json!({"model":"test","input":"hello"})).unwrap();
    client
        .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped-id")
        .await
        .unwrap();
    client
        .complete_responses_non_streaming(&credentials(), &req, "scoped-id")
        .await
        .unwrap();
    for _ in 0..2 {
        let (headers, body) = requests.recv().await.unwrap();
        assert_eq!(headers["session_id"], "scoped-id");
        assert_eq!(headers["conversation_id"], "scoped-id");
        assert_eq!(body["prompt_cache_key"], "scoped-id");
    }
}

#[tokio::test]
async fn reasoning_effort_reaches_upstream_and_default_stays_unspecified() {
    let (client, mut requests) = fake_client("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
    for effort in [
        Some("low"),
        Some("medium"),
        Some("high"),
        Some("xhigh"),
        Some("unsupported-fixture"),
        None,
    ] {
        let mut request = chat_request();
        request.reasoning_effort = effort.map(str::to_owned);
        client
            .complete_chat_non_streaming(&credentials(), &request, "scoped")
            .await
            .unwrap();
        let (_, payload) = requests.recv().await.unwrap();
        assert_eq!(
            payload["reasoning"].get("effort").and_then(Value::as_str),
            effort
        );
        let response: ResponseCreateRequest = serde_json::from_value(serde_json::json!({
            "model":"test", "input":"hello", "reasoning":{"effort":effort}
        }))
        .unwrap();
        client
            .complete_responses_non_streaming(&credentials(), &response, "scoped")
            .await
            .unwrap();
        let (_, payload) = requests.recv().await.unwrap();
        assert_eq!(
            payload["reasoning"].get("effort").and_then(Value::as_str),
            effort
        );
    }
}

/// Tool arguments and matching results survive a complete upstream round trip.
#[tokio::test]
async fn tool_calls_round_trip_without_changing_arguments() {
    let sse = concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_2\",\"call_id\":\"call_2\",\"name\":\"grep\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_2\",\"delta\":\"{\\\"pattern\\\":\\\"foo\\\"\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"\\\"/tmp/a\\\"}\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_2\",\"arguments\":\"{\\\"pattern\\\":\\\"foo\\\",\\\"path\\\":\\\"/tmp\\\"}\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_2\",\"call_id\":\"call_2\",\"name\":\"grep\",\"arguments\":\"{\\\"pattern\\\":\\\"foo\\\",\\\"path\\\":\\\"/tmp\\\"}\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/a\\\"}\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/a\\\"}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    );
    let (client, mut requests) = fake_client(sse).await;
    let result = client
        .complete_chat_non_streaming(&credentials(), &chat_request(), "scoped")
        .await
        .unwrap();
    let choice = result.choices.first().unwrap();
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    let call = choice.message.tool_calls.as_ref().unwrap().first().unwrap();
    assert_eq!(call.function.arguments, "{\"path\":\"/tmp/a\"}");
    let replay: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model":"test",
        "messages":[
            {"role":"assistant","tool_calls":[call]},
            {"role":"tool","tool_call_id":call.id,"content":"file contents"}
        ]
    }))
    .unwrap();
    client
        .complete_chat_non_streaming(&credentials(), &replay, "scoped")
        .await
        .unwrap();
    requests.recv().await.unwrap();
    let (_, payload) = requests.recv().await.unwrap();
    assert_eq!(payload["input"][0]["call_id"], call.id);
    assert_eq!(payload["input"][0]["arguments"], call.function.arguments);
    assert_eq!(payload["input"][1]["call_id"], call.id);
    assert_eq!(payload["input"][1]["output"], "file contents");
    let stream = stream_body(
        client
            .stream_chat(&credentials(), &chat_request(), "scoped")
            .await
            .unwrap(),
    )
    .await;
    let mut calls = std::collections::BTreeMap::<u64, (String, String, String)>::new();
    let mut done = false;
    for frame in stream.split("\n\n") {
        for line in frame.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim_start();
            if data == "[DONE]" {
                done = true;
                continue;
            }
            let chunk: Value = serde_json::from_str(data).unwrap();
            if let Some(tool_calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
                for tool_call in tool_calls {
                    let index = tool_call["index"].as_u64().unwrap();
                    let call = calls.entry(index).or_default();
                    if let Some(id) = tool_call["id"].as_str() {
                        call.0.push_str(id);
                    }
                    if let Some(name) = tool_call["function"]["name"].as_str() {
                        call.1.push_str(name);
                    }
                    if let Some(arguments) = tool_call["function"]["arguments"].as_str() {
                        call.2.push_str(arguments);
                    }
                }
            }
        }
    }
    assert!(done);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls.get(&0),
        Some(&(
            "call_1".into(),
            "read".into(),
            "{\"path\":\"/tmp/a\"}".into()
        ))
    );
    assert_eq!(
        calls.get(&1),
        Some(&(
            "call_2".into(),
            "grep".into(),
            "{\"pattern\":\"foo\",\"path\":\"/tmp\"}".into()
        ))
    );
}

/// Responses history keeps opaque reasoning and pairs repaired tool IDs.
#[tokio::test]
async fn responses_history_replays_reasoning_and_repairs_call_ids() {
    let (client, mut requests) = fake_client("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
    let request: ResponseCreateRequest = serde_json::from_value(serde_json::json!({
        "model":"test",
        "input":[
            {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thinking"}],"encrypted_content":"opaque"},
            {"type":"function_call","call_id":"bad id!","name":"read","arguments":"{\"path\":\"/tmp\"}"},
            {"type":"function_call_output","call_id":"bad id!","output":"contents"}
        ]
    })).unwrap();
    client
        .complete_responses_non_streaming(&credentials(), &request, "scoped")
        .await
        .unwrap();
    let (_, payload) = requests.recv().await.unwrap();
    assert_eq!(payload["input"][0]["encrypted_content"], "opaque");
    assert_eq!(payload["input"][0]["summary"][0]["text"], "thinking");
    let id = payload["input"][1]["call_id"].as_str().unwrap();
    assert_eq!(id, "badid");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    );
    assert_eq!(payload["input"][2]["call_id"], id);
    assert_eq!(payload["input"][1]["arguments"], "{\"path\":\"/tmp\"}");
    assert_eq!(payload["input"][2]["output"], "contents");
}
