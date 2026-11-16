use prost::Message;
use tokio::sync::mpsc;

use super::{Event, Wire, client, proto};

/// Handles metadata requests and hands external MCP calls to the client.
pub(super) async fn handle_exec(
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

/// Decodes a protobuf JSON value or a legacy textual MCP argument.
pub(super) fn decode_arg(bytes: &[u8]) -> serde_json::Value {
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
