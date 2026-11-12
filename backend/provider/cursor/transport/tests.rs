use super::{APPEND, BidiAppend, Event, RUN, Wire, decode_arg, dispatch, frame, proto, start};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use prost::Message;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
/// Receiver for mock Connect response chunks.
type ChunkReceiver = mpsc::Receiver<Result<Bytes, std::convert::Infallible>>;

/// Shared state for the local streaming and append endpoints.
#[derive(Clone)]
struct Mock {
    /// Channel used to produce streaming response chunks.
    chunks: mpsc::Sender<Result<Bytes, std::convert::Infallible>>,
    /// Ordered client append requests captured by the mock.
    received: Arc<Mutex<Vec<BidiAppend>>>,
    /// Streaming receiver, consumed once by the run endpoint.
    receiver: Arc<Mutex<Option<ChunkReceiver>>>,
}

/// Opens the mock stream and verifies its authentication and request ID.
async fn run(State(mock): State<Mock>, headers: HeaderMap, body: Bytes) -> (StatusCode, Body) {
    assert_eq!(
        headers.get("authorization").and_then(|h| h.to_str().ok()),
        Some("Bearer test-token")
    );
    assert_eq!(
        headers.get("content-type").and_then(|h| h.to_str().ok()),
        Some("application/connect+proto")
    );
    let request = proto::BidiRequestId::decode(body.get(5..).unwrap()).unwrap();
    assert_ne!(request.request_id, "");
    let rx = mock.receiver.lock().unwrap().take().unwrap();
    (StatusCode::OK, Body::from_stream(ReceiverStream::new(rx)))
}

/// Emits split frames and checks the response to a blob lookup.
async fn append(State(mock): State<Mock>, body: Bytes) -> StatusCode {
    let append = BidiAppend::decode(body).unwrap();
    let seq = append.append_seqno;
    let msg = proto::AgentClientMessage::decode(append.data_binary.as_slice()).unwrap();
    mock.received.lock().unwrap().push(append);
    if seq == 0 {
        assert!(matches!(
            msg.message,
            Some(proto::agent_client_message::Message::RunRequest(_))
        ));
        let update = proto::AgentServerMessage {
            message: Some(proto::agent_server_message::Message::InteractionUpdate(
                proto::InteractionUpdate {
                    message: Some(proto::interaction_update::Message::TextDelta(
                        proto::TextDeltaUpdate {
                            text: "hello".into(),
                        },
                    )),
                },
            )),
        };
        let kv = proto::AgentServerMessage {
            message: Some(proto::agent_server_message::Message::KvServerMessage(
                proto::KvServerMessage {
                    id: 7,
                    span_context: None,
                    message: Some(proto::kv_server_message::Message::GetBlobArgs(
                        proto::GetBlobArgs {
                            blob_id: b"missing".to_vec(),
                        },
                    )),
                },
            )),
        };
        let mut data = frame(&update.encode_to_vec()).unwrap();
        data.extend(frame(&kv.encode_to_vec()).unwrap());
        // Exercise split headers as well as multiple frames per chunk.
        mock.chunks
            .send(Ok(Bytes::copy_from_slice(&data[..3])))
            .await
            .unwrap();
        mock.chunks
            .send(Ok(Bytes::copy_from_slice(&data[3..])))
            .await
            .unwrap();
    } else if seq == 1 {
        assert!(matches!(
            msg.message,
            Some(proto::agent_client_message::Message::KvClientMessage(
                proto::KvClientMessage { id: 7, .. }
            ))
        ));
        let mut end = vec![2, 0, 0, 0, 2];
        end.extend_from_slice(b"{}");
        mock.chunks.send(Ok(Bytes::from(end))).await.unwrap();
    }
    StatusCode::OK
}

/// Missing token fields never become fabricated counts in the client stream.
#[tokio::test]
async fn turn_ended_usage_requires_both_reported_counts() {
    for (input, output, expected) in [
        (None, None, None),
        (Some(7), None, None),
        (None, Some(2), None),
        (Some(7), Some(2), Some((7, 2))),
        (Some(0), Some(0), Some((0, 0))),
    ] {
        let message = proto::AgentServerMessage {
            message: Some(proto::agent_server_message::Message::InteractionUpdate(
                proto::InteractionUpdate {
                    message: Some(proto::interaction_update::Message::TurnEnded(
                        proto::TurnEndedUpdate {
                            input_tokens: input,
                            output_tokens: output,
                            ..Default::default()
                        },
                    )),
                },
            )),
        };
        let message =
            proto::AgentServerMessage::decode(message.encode_to_vec().as_slice()).unwrap();
        let (tx, rx) = mpsc::channel(4);
        let mut wire = Wire {
            client: reqwest::Client::new(),
            base: String::new(),
            token: String::new(),
            id: String::new(),
            seq: 0,
        };
        dispatch(
            message,
            &mut wire,
            &mut std::collections::HashMap::default(),
            &[],
            &[],
            &tx,
        )
        .await
        .unwrap();
        tx.send(Ok(Event::Done)).await.unwrap();
        let response =
            super::super::translation::respond(rx, "model".into(), true, false, true).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let chunks: Vec<serde_json::Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect();
        let usage = chunks
            .iter()
            .find(|chunk| chunk["choices"] == serde_json::json!([]));
        if let Some((input, output)) = expected {
            assert_eq!(
                usage.unwrap()["usage"],
                serde_json::json!({
                    "prompt_tokens": input,
                    "completion_tokens": output,
                    "total_tokens": input + output
                })
            );
        } else {
            assert!(usage.is_none());
        }
    }
}

/// A split stream preserves messages and replies to blob requests.
#[tokio::test]
async fn run_sse_streams_frames_and_answers_kv() {
    let (tx, rx) = mpsc::channel(8);
    let received = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        chunks: tx,
        received: received.clone(),
        receiver: Arc::new(Mutex::new(Some(rx))),
    };
    let app = Router::new()
        .route(RUN, post(run))
        .route(APPEND, post(append))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let req: crate::openai::ChatCompletionRequest = serde_json::from_value(
        serde_json::json!({"model":"test","messages":[{"role":"user","content":"hello"}]}),
    )
    .unwrap();
    let replay = super::super::replay::build(&req).unwrap();
    let mut events = start(&format!("http://{address}"), "test-token", replay)
        .await
        .unwrap();
    assert!(matches!(events.recv().await, Some(Ok(Event::Text(text))) if text == "hello"));
    assert!(matches!(events.recv().await, Some(Ok(Event::Done))));
    let appends = received.lock().unwrap();
    assert_eq!(appends.len(), 2);
    assert_eq!(appends[0].append_seqno, 0);
    assert_eq!(appends[1].append_seqno, 1);
    assert_eq!(appends[0].request_id, appends[1].request_id);
    server.abort();
}

/// Emits an MCP request followed by misleading upstream text.
async fn append_tool(State(mock): State<Mock>, body: Bytes) -> StatusCode {
    let append = BidiAppend::decode(body).unwrap();
    let seq = append.append_seqno;
    assert_eq!(seq, 0);
    mock.received.lock().unwrap().push(append);
    if seq == 0 {
        let args = proto::McpArgs {
            name: "lookup".into(),
            tool_call_id: "call-1".into(),
            args: std::iter::once((
                "key".into(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue("value".into())),
                }
                .encode_to_vec(),
            ))
            .collect(),
            ..Default::default()
        };
        let exec = proto::ExecServerMessage {
            id: 9,
            message: Some(proto::exec_server_message::Message::McpArgs(args)),
            ..Default::default()
        };
        let msg = proto::AgentServerMessage {
            message: Some(proto::agent_server_message::Message::ExecServerMessage(
                exec,
            )),
        };
        let misleading = proto::AgentServerMessage {
            message: Some(proto::agent_server_message::Message::InteractionUpdate(
                proto::InteractionUpdate {
                    message: Some(proto::interaction_update::Message::TextDelta(
                        proto::TextDeltaUpdate {
                            text: "tool executed".into(),
                        },
                    )),
                },
            )),
        };
        let mut frames = frame(&msg.encode_to_vec()).unwrap();
        frames.extend(frame(&misleading.encode_to_vec()).unwrap());
        mock.chunks.send(Ok(Bytes::from(frames))).await.unwrap();
    }
    StatusCode::OK
}

/// An MCP execution reaches the downstream client as a tool call.
#[tokio::test]
async fn mcp_call_is_handed_to_the_client() {
    let (tx, rx) = mpsc::channel(8);
    let received = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        chunks: tx,
        received: received.clone(),
        receiver: Arc::new(Mutex::new(Some(rx))),
    };
    let app = Router::new()
        .route(RUN, post(run))
        .route(APPEND, post(append_tool))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let req: crate::openai::ChatCompletionRequest = serde_json::from_value(
        serde_json::json!({"model":"test","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{}}}}]}),
    )
    .unwrap();
    let replay = super::super::replay::build(&req).unwrap();
    let events = start(&format!("http://{address}"), "test-token", replay)
        .await
        .unwrap();
    let response =
        super::super::translation::respond(events, "test".into(), false, false, false).await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("\"finish_reason\":\"tool_calls\""));
    assert!(body.contains("\"name\":\"lookup\""));
    assert!(!body.contains("tool executed"));
    assert_eq!(received.lock().unwrap().len(), 1);
    server.abort();
}

/// MCP argument values use the standard protobuf JSON wire format.
#[test]
fn mcp_arguments_decode_protobuf_values() {
    let bytes = prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue("{\"x\":1}".into())),
    }
    .encode_to_vec();
    assert_eq!(decode_arg(&bytes), serde_json::json!({"x": 1}));
}
