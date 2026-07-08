use std::collections::HashMap;
use std::sync::Arc;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use crate::error::SdkError;
use crate::mcp::server::SdkMcpServer;
/// How to forward a JSON-RPC message from an SDK MCP server back to the
/// CLI on the `mcp_response` control reply. Wired by the session's query
/// layer.
pub type McpMessageForwarder = Arc<dyn Fn(&str, Value) + Send + Sync + 'static>;
/// Per-server transport. Mirrors the Python SDK's `SdkControlServerTransport`
/// for documentation; the actual reply path is via the SDK's
/// `control_response{mcp_response}` envelope, not this forwarder.
pub struct SdkControlServerTransport {
    pub server_name: String,
    forwarder: McpMessageForwarder,
}
impl SdkControlServerTransport {
    pub fn new(server_name: impl Into<String>, forwarder: McpMessageForwarder) -> Self {
        Self { server_name: server_name.into(), forwarder }
    }
    pub fn send(&self, message: Value) {
        (self.forwarder)(&self.server_name, message);
    }
    pub fn closed(&self) -> bool {
        false
    }
}
/// Handle an inbound `mcp_message` control request from the CLI.
///
/// - Notifications (no `id`) get an id-less `{"jsonrpc":"2.0","result":{}}`
///   ack (the Python SDK's workaround for the CLI's MCP-handshake id
///   collision).
/// - Requests route through the matching SDK MCP server and return its
///   `result` (or `error`).
pub async fn handle_mcp_message(
    servers: &Mutex<HashMap<String, Arc<SdkMcpServer>>>,
    server_name: &str,
    message: &Value,
) -> Result<Value, SdkError> {
    let server = {
        let guard = servers.lock().await;
        guard
            .get(server_name)
            .ok_or_else(|| SdkError::Handler(format!("SDK MCP server not found: {server_name}")))?
            .clone()
    };
    // The CLI's MCP client validates each reply against the JSON-RPC envelope
    // (it needs `jsonrpc:"2.0"` + the matching request `id`). `handle_message`
    // returns just the `result`/`error` *body*; we must wrap it into a proper
    // JSON-RPC response here. Without the `id` the CLI's `initialize` request
    // never resolves, the MCP handshake deadlocks, and our own `initialize`
    // control_response never arrives (60s timeout).
    let id = message.get("id").cloned();
    let is_notification = id.is_none() || id.as_ref().map(|v| v.is_null()).unwrap_or(true);
    let reply = match server.handle_message(message).await? {
        // Notification: id-less ack (matches the TS SDK's id-collision
        // workaround — an `id` here wedges the CLI's handshake).
        None => json!({ "jsonrpc": "2.0", "result": {} }),
        Some(result) => {
            if is_notification {
                json!({ "jsonrpc": "2.0", "result": {} })
            } else {
                // Request: wrap the body as a JSON-RPC response carrying the
                // request's `id`. A body that is itself an `{error}` object
                // becomes a JSON-RPC error response; otherwise a result.
                if result.get("error").is_some() && result.get("result").is_none() {
                    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": result.get("error").cloned().unwrap_or(json!({})) })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
                }
            }
        }
    };
    Ok(reply)
}
