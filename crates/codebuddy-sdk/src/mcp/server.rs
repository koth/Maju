use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use crate::error::SdkError;
/// Tool-call result content (matches MCP `CallToolResult.content`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SdkMcpToolContent {
    Text { r#type: String, text: String },
    Other(Value),
}
impl SdkMcpToolContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { r#type: "text".to_string(), text: s.into() }
    }
}
/// Result returned by an MCP tool handler.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SdkMcpToolResult {
    pub content: Vec<SdkMcpToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}
impl SdkMcpToolResult {
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: vec![SdkMcpToolContent::text(s)], is_error: None }
    }
    pub fn error(s: impl Into<String>) -> Self {
        Self { content: vec![SdkMcpToolContent::text(s)], is_error: Some(true) }
    }
}
/// Future returned by an MCP tool handler.
pub type SdkMcpHandlerFuture =
    Pin<Box<dyn Future<Output = Result<SdkMcpToolResult, SdkError>> + Send + 'static>>;
/// Handler signature: receive the parsed `input` dict, return a result
/// asynchronously. Async so a handler can defer resolution indefinitely:
/// the proxy's capture+interrupt tool strategy registers handlers that
/// **never resolve** (the CLI's agentic loop stalls at the `tool_use` until
/// `session.interrupt()` cancels it, leaving no `tool_result` in history so
/// the real result can be fed back as plain text next turn). A sync handler
/// would force the CLI to record a (spurious) result immediately and continue
/// the loop. See `codebuddy_proxy::prompt_builder::build_proxy_tools`.
pub type SdkMcpHandler =
    Arc<dyn Fn(Value) -> SdkMcpHandlerFuture + Send + Sync + 'static>;
/// A single tool registered on an SDK MCP server.
#[derive(Clone)]
pub struct SdkMcpTool {
    pub name: String,
    pub description: String,
    /// JSON-Schema-like dict describing the input shape. Pass through verbatim.
    pub input_schema: Value,
    pub handler: SdkMcpHandler,
}
/// An in-process SDK MCP server, the kind the CLI invokes via
/// `control_request{subtype:"mcp_message"}`.
pub struct SdkMcpServer {
    pub name: String,
    pub version: String,
    pub tools: Vec<SdkMcpTool>,
}
impl SdkMcpServer {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), version: "1.0.0".to_string(), tools: Vec::new() }
    }
    pub fn with_tool(mut self, tool: SdkMcpTool) -> Self {
        self.tools.push(tool);
        self
    }
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }
    /// Run a JSON-RPC message from the CLI. Returns the JSON-RPC
    /// `result` (or `error`) the SDK should send back via `mcp_response`.
    /// `None` means "notification; the caller should ack with an id-less result"
    /// (matches the Python SDK's MCP-handshake id collision workaround).
    pub async fn handle_message(&self, msg: &Value) -> Result<Option<Value>, SdkError> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id");
        let is_notification = id.is_none() || id.map(|v| v.is_null()).unwrap_or(true);
        match method {
            "initialize" => {
                // Echo the client's requested protocolVersion (MCP spec: the
                // server picks a version it supports that is <= the client's
                // request). The CodeBuddy CLI requests `2025-11-25`; replying
                // with a stale `2024-11-05` wedges the CLI's MCP handshake —
                // it never sends `notifications/initialized` and our
                // `initialize` control_response never arrives (60s deadlock).
                // The TS SDK delegates to `@modelcontextprotocol/sdk`, which
                // negotiates the version; we mirror that by echoing back the
                // requested version.
                let requested = msg
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05");
                let result = json!({
                    "protocolVersion": requested,
                    "serverInfo": { "name": self.name, "version": self.version },
                    "capabilities": { "tools": {} },
                });
                if is_notification { Ok(None) } else { Ok(Some(result)) }
            }
            "notifications/initialized" | "notifications/cancelled" => Ok(None),
            "tools/list" => {
                let tools: Vec<Value> = self
                    .tools
                    .iter()
                    .map(|t| json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    }))
                    .collect();
                if is_notification { Ok(None) } else { Ok(Some(json!({ "tools": tools }))) }
            }
            "tools/call" => {
                let name = msg
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let input = msg
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| Value::Object(BTreeMap::new().into_iter().collect()));
                let value = match self.tools.iter().find(|t| t.name == name) {
                    Some(tool) => match (tool.handler)(input).await {
                        Ok(result) => serde_json::to_value(result)?,
                        Err(err) => serde_json::to_value(SdkMcpToolResult::error(err.to_string()))?,
                    },
                    None => serde_json::to_value(SdkMcpToolResult::error(format!("tool not found: {name}")))?,
                };
                if is_notification { Ok(None) } else { Ok(Some(value)) }
            }
            _ => {
                let err = json!({ "code": -32601, "message": format!("Method not found: {method}") });
                if is_notification { Ok(None) } else { Ok(Some(err)) }
            }
        }
    }
    pub fn server_info(&self) -> Value {
        json!({ "name": self.name, "version": self.version })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn echo_tool() -> SdkMcpTool {
        SdkMcpTool {
            name: "echo".to_string(),
            description: "echo input".to_string(),
            input_schema: json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            handler: Arc::new(|input| Box::pin(async move { Ok(SdkMcpToolResult::text(format!("got:{input}"))) })),
        }
    }
    #[tokio::test]
    async fn tools_list_returns_tool_table() {
        let s = SdkMcpServer::new("t").with_tool(echo_tool());
        let resp = s.handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await.unwrap().unwrap();
        let arr = resp.get("tools").and_then(Value::as_array).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "echo");
    }
    #[tokio::test]
    async fn tools_call_invokes_handler() {
        let s = SdkMcpServer::new("t").with_tool(echo_tool());
        let resp = s
            .handle_message(
                &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo", "arguments": { "x": "hi" } } }),
            )
            .await
            .unwrap()
            .unwrap();
        let text = resp["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("hi"), "text={text}");
    }
    #[tokio::test]
    async fn tools_call_unknown_returns_error_result() {
        let s = SdkMcpServer::new("t");
        let resp = s
            .handle_message(
                &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "nope", "arguments": {} } }),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resp["is_error"], json!(true));
    }
    #[tokio::test]
    async fn notification_yields_none_for_caller_to_id_less_ack() {
        let s = SdkMcpServer::new("t").with_tool(echo_tool());
        let out = s
            .handle_message(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await
            .unwrap();
        assert!(out.is_none());
    }
    #[tokio::test]
    async fn initialize_returns_server_info() {
        let s = SdkMcpServer::new("svc").with_version("9.9");
        let resp = s.handle_message(&json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize" })).await.unwrap().unwrap();
        assert_eq!(resp["serverInfo"]["name"], "svc");
        assert_eq!(resp["serverInfo"]["version"], "9.9");
    }
}
impl std::fmt::Debug for SdkMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for SdkMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpServer")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("tools_count", &self.tools.len())
            .finish()
    }
}
impl Clone for SdkMcpServer {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            version: self.version.clone(),
            tools: self.tools.clone(),
        }
    }
}
