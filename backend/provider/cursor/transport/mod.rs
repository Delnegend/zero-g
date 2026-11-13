use futures_util::StreamExt;
use prost::Message;
use tokio::sync::mpsc;

use super::{proto, replay::Replay};
use crate::provider::common::errors::UpstreamError;
use crate::provider::common::translation::Event;

/// Execution request handling and MCP argument decoding.
mod exec;

#[cfg(test)]
use exec::decode_arg;
use exec::handle_exec;

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
                Some(Update::TurnEnded(end)) => end
                    .input_tokens
                    .zip(end.output_tokens)
                    .map(|(input, output)| Event::Usage { input, output }),
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

/// Wraps a client message variant in its protocol envelope.
const fn client(message: proto::agent_client_message::Message) -> proto::AgentClientMessage {
    proto::AgentClientMessage {
        message: Some(message),
    }
}

/// Deterministic transport tests using a local mock Cursor service.
#[cfg(test)]
mod tests;
