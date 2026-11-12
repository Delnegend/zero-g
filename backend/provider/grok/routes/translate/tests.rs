use super::*;

/// Chat history and tool replay retain their wire semantics.
#[test]
fn chat_history_tools_and_options_translate() {
    let req: ChatCompletionRequest = serde_json::from_value(json!({
        "model":"grok-4", "stream":true, "temperature":0.5,
        "max_completion_tokens":50,
        "messages":[
            {"role":"system","content":"Be precise"},
            {"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]},
            {"role":"assistant","content":null,"provider_parts":[{"type":"reasoning","id":"r1","encrypted_content":"opaque","summary":[]}],"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":1}"}}]},
            {"role":"tool","tool_call_id":"call_1","content":"result"}
        ],
        "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}],
        "tool_choice":{"type":"function","function":{"name":"lookup"}}
    })).unwrap();
    let payload = chat_to_responses(&req);
    assert_eq!(payload["stream"], true);
    assert_eq!(payload["max_output_tokens"], 50);
    assert_eq!(payload["input"][0]["role"], "system");
    assert_eq!(
        payload["input"][1]["content"][1]["image_url"],
        "https://example.com/a.png"
    );
    assert_eq!(payload["input"][2]["encrypted_content"], "opaque");
    assert_eq!(payload["input"][3]["call_id"], "call_1");
    assert_eq!(payload["input"][4]["output"], "result");
    assert_eq!(payload["tools"][0]["name"], "lookup");
    assert_eq!(
        payload["tool_choice"],
        json!({"type":"function","name":"lookup"})
    );
    assert!(payload.get("store").is_none());
}

/// Completed responses retain content, calls, reasoning and usage.
#[test]
fn completed_response_preserves_calls_reasoning_and_usage() {
    let response = json!({"id":"resp_1","created_at":42,"status":"completed","output":[
        {"type":"reasoning","id":"reason_1","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"thinking"}]},
        {"type":"message","content":[{"type":"output_text","text":"answer"}]},
        {"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{}"}
    ],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}});
    let chat = responses_to_chat(&response, "grok-4").unwrap();
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["choices"][0]["message"]["content"], "answer");
    assert_eq!(
        chat["choices"][0]["message"]["reasoning_content"],
        "thinking"
    );
    assert_eq!(
        chat["choices"][0]["message"]["provider_parts"][0]["encrypted_content"],
        "opaque"
    );
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(chat["usage"]["total_tokens"], 5);
}

#[test]
fn split_multiline_sse_event_is_decoded() {
    let mut decoder = decoder();
    assert_eq!(
        decoder
            .push_chunk(b"event: response.created\ndata: {\"type\":\"response.created\",\nda")
            .unwrap(),
        Vec::new()
    );
    let frames = decoder
        .push_chunk(b"ta: \"response\":{\"id\":\"r\",\"created_at\":42}}\n\n")
        .unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0.as_deref(), Some("response.created"));
    let event: Value = serde_json::from_str(&frames[0].1).unwrap();
    assert_eq!(event["response"]["id"], "r");
}

#[test]
fn complete_events_precede_size_errors_regardless_of_chunking() {
    let frame =
        b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\n";
    for suffix in [
        "x".repeat(MAX_SSE_BYTES + 1),
        format!(
            "data: {}\ndata: {}\n",
            "x".repeat(MAX_SSE_BYTES / 2),
            "y".repeat(MAX_SSE_BYTES / 2)
        ),
    ] {
        for split in [false, true] {
            let mut decoder = decoder();
            let mut input = Vec::new();
            if split {
                assert_eq!(decoder.push_chunk(frame).unwrap().len(), 1);
            } else {
                input.extend_from_slice(frame);
            }
            input.extend_from_slice(suffix.as_bytes());
            let (frames, error) = decoder.push_chunk(&input).unwrap_err();
            assert!(error.contains("size limit"));
            assert_eq!(frames.len(), usize::from(!split));
            if !split {
                let event: Value = serde_json::from_str(&frames[0].1).unwrap();
                assert_eq!(event["response"]["id"], "r");
            }
        }
    }
}

/// Split SSE frames and completed argument suffixes are decoded correctly.
#[test]
fn stream_decodes_split_frames_and_tool_argument_suffixes() {
    let mut decoder = decoder();
    assert_eq!(
        decoder
            .push_chunk(b"event: response.created\r\ndata: {\"type\":\"response.")
            .unwrap()
            .len(),
        0
    );
    let frames = decoder
        .push_chunk(b"created\",\"response\":{\"id\":\"r\",\"created_at\":42}}\r\n\r\n")
        .unwrap();
    let mut state = ChatStream {
        id: String::new(),
        created: 0,
        model: "grok".into(),
        calls: vec![],
        finished: false,
        include_usage: true,
    };
    let created: Value = serde_json::from_str(&frames[0].1).unwrap();
    assert_eq!(
        state.handle(&created, frames[0].0.as_deref()).unwrap()[0]["choices"][0]["delta"]["role"],
        "assistant"
    );
    let added = json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc","call_id":"call","name":"lookup","arguments":""}});
    assert_eq!(
        state.handle(&added, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call"
    );
    let delta =
        json!({"type":"response.function_call_arguments.delta","item_id":"fc","delta":"{\"x\":"});
    assert_eq!(
        state.handle(&delta, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["function"]
            ["arguments"],
        "{\"x\":"
    );
    let done = json!({"type":"response.function_call_arguments.done","item_id":"fc","arguments":"{\"x\":1}"});
    assert_eq!(
        state.handle(&done, None).unwrap()[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "1}"
    );
    assert_eq!(state.handle(&done, None).unwrap().len(), 0);
    let terminal = json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}});
    let chunks = state.handle(&terminal, None).unwrap();
    assert_eq!(chunks[0]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chunks[1]["usage"]["total_tokens"], 3);
    assert!(state.finished);
}

/// Effort options follow each xAI model's supported settings.
#[test]
fn xai_reasoning_options_follow_model_capabilities() {
    for (model, effort, expected) in [
        ("grok-build", "high", None),
        ("grok-build-0.1", "high", None),
        ("grok-4.20-0309-non-reasoning", "high", None),
        ("grok-4.3", "xhigh", Some("high")),
        ("grok-4.6", "minimal", Some("low")),
        ("grok-4.6", "xhigh", Some("xhigh")),
        ("grok-4.20-multi-agent-0309", "xhigh", Some("xhigh")),
    ] {
        let req: ChatCompletionRequest = serde_json::from_value(json!({"model":model,"messages":[],"reasoning_effort":effort,"reasoning":{"summary":"auto"}})).unwrap();
        let payload = chat_to_responses(&req);
        assert_eq!(
            payload.pointer("/reasoning/effort").and_then(Value::as_str),
            expected
        );
        assert!(payload.pointer("/reasoning/summary").is_none());
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    }
}

/// The HTTP stream exposes tool chunks and truncated-stream errors.
#[tokio::test]
async fn upstream_stream_is_exposed_as_chat_sse_and_errors_are_exposed() {
    use axum::{
        Router,
        body::{Body, Bytes},
        routing::get,
    };
    let app = Router::new()
        .route("/complete", get(|| async {
            let chunks: Vec<Result<Bytes, Infallible>> = vec![
                Ok(Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\ndata: {\"type\":\"response.output_item.add")),
                Ok(Bytes::from_static(b"ed\",\"item\":{\"type\":\"function_call\",\"id\":\"fc\",\"call_id\":\"call\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc\",\"delta\":\"{}\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")),
            ];
            Body::from_stream(futures_util::stream::iter(chunks))
        }))
        .route("/truncated", get(|| async {
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"created_at\":1}}\n\n"
        }))
        .route("/oversized-line", get(|| async {
            format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\n{}", "x".repeat(MAX_SSE_BYTES + 1))
        }))
        .route("/oversized-event", get(|| async {
            format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\ndata: {}\ndata: {}\n\n", "x".repeat(MAX_SSE_BYTES / 2), "y".repeat(MAX_SSE_BYTES / 2))
        }))
        .route("/terminal-before-oversized", get(|| async {
            format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"r\",\"created_at\":1}}}}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\"}}}}\n\n{}", "x".repeat(MAX_SSE_BYTES + 1))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let upstream = reqwest::get(format!("http://{addr}/complete"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(
        stream_to_chat(upstream, "grok".into(), false).into_body(),
        1024 * 1024,
    )
    .await
    .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    let events: Vec<Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect();
    assert_eq!(
        events[1]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call"
    );
    assert_eq!(
        events[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(events[3]["choices"][0]["finish_reason"], "tool_calls");
    assert!(text.contains("data: [DONE]"));
    let upstream = reqwest::get(format!("http://{addr}/truncated"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(
        stream_to_chat(upstream, "grok".into(), false).into_body(),
        1024 * 1024,
    )
    .await
    .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("event: error"));
    assert!(text.contains("without a terminal response"));
    assert!(!text.contains("[DONE]"));
    for (path, expected) in [
        ("oversized-line", "line exceeds size limit"),
        ("oversized-event", "event data exceeds size limit"),
    ] {
        let upstream = reqwest::get(format!("http://{addr}/{path}")).await.unwrap();
        let body = axum::body::to_bytes(
            stream_to_chat(upstream, "grok".into(), false).into_body(),
            1024 * 1024,
        )
        .await
        .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("event: error") && text.contains(expected));
        assert!(text.find("\"role\":\"assistant\"").unwrap() < text.find("event: error").unwrap());
        assert!(!text.contains("[DONE]"));
    }
    let upstream = reqwest::get(format!("http://{addr}/terminal-before-oversized"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(
        stream_to_chat(upstream, "grok".into(), false).into_body(),
        1024 * 1024,
    )
    .await
    .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("assistant") && text.contains("\"finish_reason\":\"stop\""));
    assert!(text.contains("[DONE]"));
    assert!(!text.contains("event: error"));
    server.abort();
}

/// Invalid tool events and failed responses produce errors.
#[test]
fn fails_on_invalid_or_failed_upstream_events() {
    let mut state = ChatStream {
        id: "r".into(),
        created: 1,
        model: "grok".into(),
        calls: vec![],
        finished: false,
        include_usage: false,
    };
    assert!(state.handle(&json!({"type":"response.function_call_arguments.delta","item_id":"unknown","delta":"x"}), None).is_err());
    assert!(
        responses_to_chat(
            &json!({"id":"r","created_at":1,"status":"failed","output":[]}),
            "grok"
        )
        .is_err()
    );
}
