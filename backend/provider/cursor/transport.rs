use futures_util::StreamExt;
use prost::Message;
use tokio::sync::mpsc;

use super::{proto, replay::Replay, translation::Event};
use crate::provider::common::errors::UpstreamError;

/// Default Cursor agent endpoint.
const BASE: &str = "https://api2.cursor.sh";
/// Streaming server-message RPC path.
const RUN: &str = "/agent.v1.AgentService/RunSSE";
/// Unary client-message append RPC path.
const APPEND: &str = "/aiserver.v1.BidiService/BidiAppend";
/// Cursor CLI version advertised to the service.
const VERSION: &str = "cli-2026.09.02-c22c1a3";

/// Binary subset of the `BidiAppend` request.
#[derive(Clone, PartialEq, Message)]
struct BidiAppend {
    /// Identifier shared with the streaming request.
    #[prost(message, optional, tag = "2")]
    request_id: Option<proto::BidiRequestId>,
    /// Zero-based ordering of client messages.
    #[prost(int64, tag = "3")]
    append_seqno: i64,
    /// Serialized agent client message.
    #[prost(bytes, tag = "4")]
    data_binary: Vec<u8>,
}

/// Wraps a protobuf message in a Connect streaming envelope.
fn frame(data: &[u8]) -> Result<Vec<u8>, String> {
    let len = u32::try_from(data.len()).map_err(|_| "Cursor frame too large")?;
    let mut out = Vec::with_capacity(data.len().saturating_add(5));
    out.push(0);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

/// State shared by the streaming RPC and its ordered append calls.
struct Wire {
    /// HTTP client used for all RPCs.
    client: reqwest::Client,
    /// Upstream endpoint without a trailing slash.
    base: String,
    /// OAuth access token.
    token: String,
    /// Request identifier correlating append calls to the stream.
    id: String,
    /// Sequence number for the next append.
    seq: i64,
}

impl Wire {
    /// Adds Cursor CLI authentication and protocol headers.
    fn headers(
        &self,
        request: reqwest::RequestBuilder,
        content_type: &str,
    ) -> reqwest::RequestBuilder {
        request
            .bearer_auth(&self.token)
            .header("content-type", content_type)
            .header("connect-protocol-version", "1")
            .header("x-cursor-client-type", "cli")
            .header("x-cursor-client-version", VERSION)
            .header("x-ghost-mode", "true")
            .header("x-request-id", &self.id)
            .header(
                "x-cursor-agent-allowed-tools",
                "mcp_tool_call,get_mcp_tools_tool_call",
            )
    }

    /// Sends one ordered client message through the unary append RPC.
    async fn append(&mut self, message: proto::AgentClientMessage) -> Result<(), UpstreamError> {
        let body = BidiAppend {
            request_id: Some(proto::BidiRequestId {
                request_id: self.id.clone(),
            }),
            append_seqno: self.seq,
            data_binary: message.encode_to_vec(),
        };
        self.seq = self
            .seq
            .checked_add(1)
            .ok_or("Cursor append sequence exhausted")?;
        let response = self
            .headers(
                self.client.post(format!("{}{}", self.base, APPEND)),
                "application/proto",
            )
            .timeout(std::time::Duration::from_secs(60_u64.saturating_add(
                u64::try_from(body.data_binary.len() / (128 * 1024)).unwrap_or(u64::MAX),
            )))
            .body(body.encode_to_vec())
            .send()
            .await
            .map_err(|e| format!("Cursor BidiAppend failed: {e}"))?;
        if !response.status().is_success() {
            return Err(UpstreamError {
                status: Some(response.status()),
                message: format!("Cursor BidiAppend returned {}", response.status()),
            });
        }
        Ok(())
    }
}

/// Open `RunSSE` and send the initial append concurrently: some servers do not
/// flush response headers until they receive the first client message.
pub(super) async fn start(
    base: &str,
    token: &str,
    replay: Replay,
) -> Result<mpsc::Receiver<Result<Event, String>>, UpstreamError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Cursor client initialization failed: {e}"))?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut wire = Wire {
        client,
        base: if base.is_empty() {
            BASE.to_string()
        } else {
            base.trim_end_matches('/').to_string()
        },
        token: token.to_string(),
        id,
        seq: 0,
    };
    let request_id = proto::BidiRequestId {
        request_id: wire.id.clone(),
    };
    let run = wire
        .headers(
            wire.client.post(format!("{}{}", wire.base, RUN)),
            "application/connect+proto",
        )
        .body(frame(&request_id.encode_to_vec())?)
        .send();
    let first = proto::AgentClientMessage {
        message: Some(proto::agent_client_message::Message::RunRequest(
            replay.request,
        )),
    };
    let (response, appended) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(run, wire.append(first))
    })
    .await
    .map_err(|_| "Cursor RunSSE startup timed out".to_string())?;
    appended?;
    let response = response.map_err(|e| format!("Cursor RunSSE failed: {e}"))?;
    if !response.status().is_success() {
        return Err(UpstreamError {
            status: Some(response.status()),
            message: format!("Cursor RunSSE returned {}", response.status()),
        });
    }
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        if let Err(error) = consume(
            response,
            wire,
            replay.blobs,
            replay.tools,
            replay.rules,
            &tx,
        )
        .await
        {
            let _ = tx.send(Err(error)).await;
        }
    });
    Ok(rx)
}

/// Decodes the stream and answers Cursor's supported client-side requests.
async fn consume(
    response: reqwest::Response,
    mut wire: Wire,
    mut blobs: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    tools: Vec<proto::McpToolDefinition>,
    rules: Vec<proto::CursorRule>,
    tx: &mpsc::Sender<Result<Event, String>>,
) -> Result<(), String> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(5));
    heartbeat.tick().await;
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let chunk = tokio::select! {
            () = tx.closed() => return Ok(()),
            _ = heartbeat.tick() => {
                wire.append(client(proto::agent_client_message::Message::ClientHeartbeat(proto::ClientHeartbeat {}))).await.map_err(|e| e.message)?;
                continue;
            }
            chunk = stream.next() => chunk.ok_or("Cursor stream ended without a Connect end frame")?,
        };
        buffer.extend_from_slice(&chunk.map_err(|e| format!("Cursor stream failed: {e}"))?);
        while let Some((flag, payload)) = next_frame(&mut buffer)? {
            if flag & 2 != 0 {
                let end: serde_json::Value = serde_json::from_slice(&payload)
                    .map_err(|e| format!("Invalid Cursor end frame: {e}"))?;
                if let Some(error) = end.get("error") {
                    return Err(format!("Cursor stream error: {error}"));
                }
                let _ = tx.send(Ok(Event::Done)).await;
                return Ok(());
            }
            if flag != 0 {
                return Err(format!("Unsupported Cursor frame flags: {flag}"));
            }
            let message = proto::AgentServerMessage::decode(payload.as_slice())
                .map_err(|e| format!("Invalid Cursor message: {e}"))?;
            if dispatch(message, &mut wire, &mut blobs, &tools, &rules, tx).await? {
                let _ = tx.send(Ok(Event::Done)).await;
                return Ok(());
            }
        }
    }
}

/// Extracts a complete Connect frame without assuming chunk boundaries.
fn next_frame(buffer: &mut Vec<u8>) -> Result<Option<(u8, Vec<u8>)>, String> {
    let Some(header) = buffer.get(..5) else {
        return Ok(None);
    };
    let [flag, a, b, c, d] =
        <[u8; 5]>::try_from(header).map_err(|_| "Invalid Cursor frame header")?;
    let length = usize::try_from(u32::from_be_bytes([a, b, c, d]))
        .map_err(|_| "Invalid Cursor frame size")?;
    if length > 64 * 1024 * 1024 {
        return Err("Cursor frame exceeds size limit".into());
    }
    let end = length.saturating_add(5);
    let Some(payload) = buffer.get(5..end) else {
        return Ok(None);
    };
    let payload = payload.to_vec();
    buffer.drain(..end);
    Ok(Some((flag, payload)))
}

/// Dispatches a decoded server message; true denotes external tool handoff.
async fn dispatch(
    message: proto::AgentServerMessage,
    wire: &mut Wire,
    blobs: &mut std::collections::HashMap<Vec<u8>, Vec<u8>>,
    tools: &[proto::McpToolDefinition],
    rules: &[proto::CursorRule],
    tx: &mpsc::Sender<Result<Event, String>>,
) -> Result<bool, String> {
    use proto::agent_server_message::Message as Server;
    match message.message {
        Some(Server::InteractionUpdate(update)) => {
            use proto::interaction_update::Message as Update;
            let event = match update.message {
                Some(Update::TextDelta(delta)) => Some(Event::Text(delta.text)),
                Some(Update::ThinkingDelta(delta)) => Some(Event::Thinking(delta.text)),
                Some(Update::TurnEnded(end)) => Some(Event::Usage {
                    input: end.input_tokens.unwrap_or(0),
                    output: end.output_tokens.unwrap_or(0),
                }),
                _ => None,
            };
            if let Some(event) = event {
                let _ = tx.send(Ok(event)).await;
            }
        }
        Some(Server::KvServerMessage(kv)) => {
            use proto::kv_server_message::Message as Kv;
            let result = match kv.message {
                Some(Kv::GetBlobArgs(args)) => Some(
                    proto::kv_client_message::Message::GetBlobResult(proto::GetBlobResult {
                        blob_data: blobs.get(&args.blob_id).cloned(),
                    }),
                ),
                Some(Kv::SetBlobArgs(args)) => {
                    blobs.insert(args.blob_id, args.blob_data);
                    Some(proto::kv_client_message::Message::SetBlobResult(
                        proto::SetBlobResult { error: None },
                    ))
                }
                None => None,
            };
            if let Some(message) = result {
                wire.append(client(
                    proto::agent_client_message::Message::KvClientMessage(proto::KvClientMessage {
                        id: kv.id,
                        message: Some(message),
                    }),
                ))
                .await
                .map_err(|e| e.message)?;
            }
        }
        Some(Server::ExecServerMessage(exec)) => {
            return handle_exec(exec, wire, tools, rules, tx).await;
        }
        Some(Server::InteractionQuery(_)) => {
            return Err("Cursor requested an unsupported interactive permission".into());
        }
        _ => {}
    }
    Ok(false)
}

/// Handles metadata requests and hands external MCP calls to the client.
async fn handle_exec(
    exec: proto::ExecServerMessage,
    wire: &mut Wire,
    tools: &[proto::McpToolDefinition],
    rules: &[proto::CursorRule],
    tx: &mpsc::Sender<Result<Event, String>>,
) -> Result<bool, String> {
    use proto::exec_server_message::Message as Exec;
    let result = match exec.message {
        Some(Exec::RequestContextArgs(_)) => {
            let context = proto::RequestContext {
                rules: rules.to_vec(),
                tools: tools.to_vec(),
                ..Default::default()
            };
            let result = proto::RequestContextResult {
                result: Some(proto::request_context_result::Result::Success(
                    proto::RequestContextSuccess {
                        request_context: Some(context),
                        served_from_disk_cache: None,
                    },
                )),
            };
            proto::exec_client_message::Message::RequestContextResult(result)
        }
        Some(Exec::McpStateExecArgs(args)) => mcp_state(&args, tools),
        Some(Exec::McpArgs(args)) if !args.smart_mode_approval_only => {
            let name = if args.tool_name.is_empty() {
                &args.name
            } else {
                &args.tool_name
            };
            if !tools.iter().any(|tool| {
                (tool.tool_name == *name || tool.name == *name)
                    && (args.provider_identifier.is_empty()
                        || tool.provider_identifier == args.provider_identifier)
            }) {
                reject_exec(wire, exec.id).await?;
                return Ok(false);
            }
            let id = if args.tool_call_id.is_empty() {
                format!("cursor_{}", exec.id)
            } else {
                args.tool_call_id
            };
            let arguments = args
                .args
                .into_iter()
                .map(|(k, v)| (k, decode_arg(&v)))
                .collect::<serde_json::Map<_, _>>();
            let name = if args.tool_name.is_empty() {
                args.name
            } else {
                args.tool_name
            };
            let _ = tx
                .send(Ok(Event::Tool {
                    id,
                    name,
                    arguments: serde_json::Value::Object(arguments).to_string(),
                }))
                .await;
            return Ok(true);
        }
        _ => {
            reject_exec(wire, exec.id).await?;
            return Ok(false);
        }
    };
    let message = proto::ExecClientMessage {
        id: exec.id,
        exec_id: exec.exec_id,
        message: Some(result),
        ..Default::default()
    };
    wire.append(client(
        proto::agent_client_message::Message::ExecClientMessage(message),
    ))
    .await
    .map_err(|e| e.message)?;
    Ok(false)
}

/// Lists the external tool providers advertised in the request context.
fn mcp_state(
    args: &proto::McpStateExecArgs,
    tools: &[proto::McpToolDefinition],
) -> proto::exec_client_message::Message {
    let mut grouped = std::collections::BTreeMap::<String, Vec<proto::McpToolDefinition>>::new();
    for tool in tools {
        if args.server_identifiers.is_empty()
            || args.server_identifiers.contains(&tool.provider_identifier)
        {
            grouped
                .entry(tool.provider_identifier.clone())
                .or_default()
                .push(tool.clone());
        }
    }
    let servers = grouped
        .into_iter()
        .map(|(id, tools)| proto::McpStateServer {
            server_name: id.clone(),
            server_identifier: id,
            tools,
            status: Some("connected".into()),
            ..Default::default()
        })
        .collect();
    let result = proto::McpStateExecResult {
        result: Some(proto::mcp_state_exec_result::Result::Success(
            proto::McpStateSuccess { servers },
        )),
    };
    proto::exec_client_message::Message::McpStateExecResult(result)
}

/// Rejects unsupported local execution without running a command or accessing files.
async fn reject_exec(wire: &mut Wire, id: u32) -> Result<(), String> {
    use proto::exec_client_control_message::Message;
    let throw = proto::ExecClientControlMessage {
        message: Some(Message::Throw(proto::ExecClientThrow {
            id,
            error: "This Cursor execution is not supported by the gateway".into(),
            stack_trace: None,
            error_code: None,
        })),
    };
    wire.append(client(
        proto::agent_client_message::Message::ExecClientControlMessage(throw),
    ))
    .await
    .map_err(|e| e.message)?;
    let close = proto::ExecClientControlMessage {
        message: Some(Message::StreamClose(proto::ExecClientStreamClose { id })),
    };
    wire.append(client(
        proto::agent_client_message::Message::ExecClientControlMessage(close),
    ))
    .await
    .map_err(|e| e.message)
}

/// Wraps a client message variant in its protocol envelope.
const fn client(message: proto::agent_client_message::Message) -> proto::AgentClientMessage {
    proto::AgentClientMessage {
        message: Some(message),
    }
}

/// Decodes a protobuf JSON value or a legacy textual MCP argument.
fn decode_arg(bytes: &[u8]) -> serde_json::Value {
    /// Converts recursively decoded protobuf JSON values into JSON.
    fn convert(value: prost_types::Value) -> serde_json::Value {
        use prost_types::value::Kind;
        match value.kind {
            Some(Kind::NullValue(_)) | None => serde_json::Value::Null,
            Some(Kind::BoolValue(v)) => v.into(),
            Some(Kind::NumberValue(v)) => serde_json::json!(v),
            Some(Kind::StringValue(v)) => {
                if matches!(v.trim_start().chars().next(), Some('{' | '[' | '"')) {
                    serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v))
                } else {
                    serde_json::Value::String(v)
                }
            }
            Some(Kind::ListValue(v)) => {
                serde_json::Value::Array(v.values.into_iter().map(convert).collect())
            }
            Some(Kind::StructValue(v)) => serde_json::Value::Object(
                v.fields.into_iter().map(|(k, v)| (k, convert(v))).collect(),
            ),
        }
    }
    if let Ok(value) = prost_types::Value::decode(bytes)
        && value.kind.is_some()
    {
        return convert(value);
    }
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// Deterministic transport tests using a local mock Cursor service.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use std::sync::{Arc, Mutex};
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
            super::super::translation::respond(events, "test".into(), false, false).await;
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
}
