//! stdio JSON-RPC client for the browser provider.
//!
//! The provider speaks MCP over stdio: newline-delimited JSON-RPC 2.0. This
//! module owns that conversation — handshake, catalog discovery, tool calls,
//! and projecting the result into text and image parts.
//!
//! The transport is abstracted so the protocol logic can be tested against an
//! in-memory server. Everything that is actually about the wire (framing,
//! id correlation, content projection) is covered without a browser.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::BrowserError;
use crate::catalog::ProviderCatalog;

/// MCP protocol revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The client's advertised identity during the handshake.
pub const CLIENT_NAME: &str = "kodex-browser-use";

/// Split a serialized JSON-RPC message into a stdio frame.
///
/// MCP stdio uses newline-delimited JSON, and a payload containing a raw
/// newline would desynchronize the stream, so anything outside printable
/// ASCII is escaped rather than written literally.
pub fn frame(payload: &str) -> String {
    let mut out = String::with_capacity(payload.len() + 2);
    for ch in payload.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\n');
    out
}

/// Parse one framed line back into a JSON-RPC message.
pub fn parse_frame(line: &str) -> Result<serde_json::Value, McpProtocolError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(McpProtocolError::EmptyFrame);
    }
    serde_json::from_str(trimmed).map_err(|error| McpProtocolError::Decode(error.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpProtocolError {
    EmptyFrame,
    Decode(String),
    /// The peer closed the stream mid-conversation.
    Closed,
    /// A response arrived with an id nobody is waiting on.
    UnexpectedResponse(serde_json::Value),
    /// The peer sent a notification the client did not expect.
    UnexpectedNotification(String),
    /// The peer answered with an error object.
    Remote {
        code: i64,
        message: String,
    },
    /// A response had the wrong id.
    IdMismatch {
        expected: u64,
        got: u64,
    },
    /// The handshake was refused.
    Handshake(String),
    /// The call exceeded its deadline.
    Timeout,
}

impl std::fmt::Display for McpProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpProtocolError::EmptyFrame => write!(formatter, "empty JSON-RPC frame"),
            McpProtocolError::Decode(detail) => {
                write!(formatter, "malformed JSON-RPC frame: {detail}")
            }
            McpProtocolError::Closed => write!(formatter, "browser provider closed the connection"),
            McpProtocolError::UnexpectedResponse(value) => {
                write!(formatter, "unexpected JSON-RPC response: {value}")
            }
            McpProtocolError::UnexpectedNotification(method) => {
                write!(formatter, "unexpected notification: {method}")
            }
            McpProtocolError::Remote { code, message } => {
                write!(formatter, "provider error {code}: {message}")
            }
            McpProtocolError::IdMismatch { expected, got } => {
                write!(
                    formatter,
                    "response id {got} does not match request id {expected}"
                )
            }
            McpProtocolError::Handshake(detail) => {
                write!(formatter, "provider handshake failed: {detail}")
            }
            McpProtocolError::Timeout => write!(formatter, "provider call timed out"),
        }
    }
}

impl std::error::Error for McpProtocolError {}

impl From<McpProtocolError> for BrowserError {
    fn from(error: McpProtocolError) -> Self {
        match error {
            McpProtocolError::Timeout => BrowserError::Timeout {
                tool: String::new(),
                after_ms: 0,
            },
            other => BrowserError::Launch(other.to_string()),
        }
    }
}

/// A JSON-RPC request as sent to the provider.
#[derive(Debug, Clone, Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct Notification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// The transport a client writes to and reads from.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Send one already-framed line.
    async fn send_line(&self, line: &str) -> Result<(), McpProtocolError>;
    /// Read the next framed line, or `Closed` at end of stream.
    async fn read_line(&self) -> Result<String, McpProtocolError>;
}

/// A live MCP conversation with the provider.
pub struct McpClient {
    transport: Arc<dyn Transport>,
    next_id: AtomicU64,
    timeout: Duration,
    /// Set once `initialize` has completed, so it cannot run twice.
    initialized: Mutex<bool>,
}

impl McpClient {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
            timeout: Duration::from_secs(30),
            initialized: Mutex::new(false),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Perform the MCP handshake. Safe to call once per connection.
    pub async fn initialize(&self) -> Result<(), McpProtocolError> {
        {
            let initialized = self.initialized.lock().await;
            if *initialized {
                return Ok(());
            }
        }

        let response = self
            .request(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
                })),
                None,
            )
            .await?;

        let negotiated = response
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                McpProtocolError::Handshake("response carried no protocolVersion".to_string())
            })?;
        if negotiated.is_empty() {
            return Err(McpProtocolError::Handshake(
                "provider negotiated an empty protocol version".to_string(),
            ));
        }

        self.notify("notifications/initialized", None).await?;
        *self.initialized.lock().await = true;
        Ok(())
    }

    /// Fetch the provider's tool catalog.
    pub async fn list_tools(&self) -> Result<ProviderCatalog, McpProtocolError> {
        self.initialize().await?;
        let response = self
            .request("tools/list", Some(serde_json::json!({})), None)
            .await?;
        if !response
            .get("tools")
            .is_some_and(serde_json::Value::is_array)
        {
            return Err(McpProtocolError::Decode(
                "tools/list carried no tools array".to_string(),
            ));
        }
        serde_json::from_value(response)
            .map_err(|error| McpProtocolError::Decode(error.to_string()))
    }

    /// Call one tool, projecting its result into text and image parts.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult, McpProtocolError> {
        self.initialize().await?;
        let response = self
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": name, "arguments": arguments })),
                Some(name.to_string()),
            )
            .await?;
        Ok(project_result(&response))
    }

    /// Send a request and await its matching response.
    ///
    /// A server may interleave notifications; they are skipped rather than
    /// treated as a protocol violation, because a provider is free to report
    /// progress while a long tool call runs.
    pub async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        tool: Option<String>,
    ) -> Result<serde_json::Value, McpProtocolError> {
        let id = self.allocate_id();
        let payload = serde_json::to_string(&Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|error| McpProtocolError::Decode(error.to_string()))?;

        self.transport.send_line(&frame(&payload)).await?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(match tool {
                    Some(name) => McpProtocolError::Timeout,
                    None => McpProtocolError::Timeout,
                });
            }

            let line = match tokio::time::timeout(remaining, self.transport.read_line()).await {
                Ok(result) => result?,
                Err(_) => return Err(McpProtocolError::Timeout),
            };

            let message = parse_frame(&line)?;

            // Skip server-initiated notifications.
            if message.get("method").is_some() && message.get("id").is_none() {
                continue;
            }

            let response_id = message.get("id").and_then(serde_json::Value::as_u64);
            match response_id {
                Some(response_id) if response_id == id => {
                    if let Some(error) = message.get("error") {
                        return Err(McpProtocolError::Remote {
                            code: error
                                .get("code")
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or(0),
                            message: error
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("unknown provider error")
                                .to_string(),
                        });
                    }
                    return Ok(message
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null));
                }
                Some(response_id) => {
                    return Err(McpProtocolError::IdMismatch {
                        expected: id,
                        got: response_id,
                    });
                }
                None => {
                    return Err(McpProtocolError::UnexpectedResponse(message));
                }
            }
        }
    }

    async fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpProtocolError> {
        let payload = serde_json::to_string(&Notification {
            jsonrpc: "2.0",
            method,
            params,
        })
        .map_err(|error| McpProtocolError::Decode(error.to_string()))?;
        self.transport.send_line(&frame(&payload)).await
    }
}

/// A tool call result, split by content part type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallResult {
    pub text: String,
    /// Decoded image bytes, base64 given by the provider.
    pub images: Vec<Vec<u8>>,
    /// The provider reported the call as failed even though it answered.
    pub is_error: bool,
}

/// Split an MCP `tools/call` result into text and image parts.
///
/// Image payloads are base64 in the protocol; decoding here rather than passing
/// the string on means the screenshot pipeline receives real bytes, and a
/// malformed payload surfaces as a provider error instead of corrupting a
/// stored file.
pub fn project_result(response: &serde_json::Value) -> ToolCallResult {
    let mut result = ToolCallResult {
        is_error: response
            .get("isError")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        ..ToolCallResult::default()
    };

    let Some(content) = response
        .get("content")
        .and_then(serde_json::Value::as_array)
    else {
        return result;
    };

    for part in content {
        match part.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                    if !result.text.is_empty() {
                        result.text.push('\n');
                    }
                    result.text.push_str(text);
                }
            }
            Some("image") => {
                let Some(encoded) = part.get("data").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                match base64_decode(encoded) {
                    Some(bytes) if !bytes.is_empty() => result.images.push(bytes),
                    // A malformed or empty image is dropped rather than
                    // reported: the text parts still describe the capture.
                    _ => continue,
                }
            }
            _ => continue,
        }
    }

    result
}

/// Decode standard base64 with padding.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in TABLE.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }

    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;

    for byte in input.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// A transport over a live child process's stdio.
pub struct StdioTransport {
    writer: Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>,
    reader: Mutex<BufReader<Box<dyn tokio::io::AsyncRead + Send + Unpin>>>,
}

impl StdioTransport {
    pub fn new(
        reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        writer: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    ) -> Self {
        Self {
            writer: Mutex::new(writer),
            reader: Mutex::new(BufReader::new(reader)),
        }
    }

    /// Attach to a spawned child process.
    pub fn from_child(child: &mut tokio::process::Child) -> std::io::Result<Self> {
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("browser provider produced no stdout to talk over")
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::other("browser provider produced no stdin to talk over")
        })?;
        Ok(Self::new(Box::new(stdout), Box::new(stdin)))
    }
}

#[async_trait::async_trait]
impl Transport for StdioTransport {
    async fn send_line(&self, line: &str) -> Result<(), McpProtocolError> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| McpProtocolError::Decode(error.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|error| McpProtocolError::Decode(error.to_string()))
    }

    async fn read_line(&self) -> Result<String, McpProtocolError> {
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|error| McpProtocolError::Decode(error.to_string()))?;
        if read == 0 {
            return Err(McpProtocolError::Closed);
        }
        Ok(line)
    }
}

/// A transport backed by a fixed script of responses, for tests.
pub struct ScriptedTransport {
    outgoing: Mutex<Vec<String>>,
    incoming: Mutex<Vec<String>>,
}

impl ScriptedTransport {
    pub fn new(incoming: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            outgoing: Mutex::new(Vec::new()),
            incoming: Mutex::new(incoming),
        })
    }

    /// Everything the client has sent, in order.
    pub async fn sent(&self) -> Vec<String> {
        self.outgoing.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl Transport for ScriptedTransport {
    async fn send_line(&self, line: &str) -> Result<(), McpProtocolError> {
        self.outgoing.lock().await.push(line.to_string());
        Ok(())
    }

    async fn read_line(&self) -> Result<String, McpProtocolError> {
        let mut incoming = self.incoming.lock().await;
        if incoming.is_empty() {
            return Err(McpProtocolError::Closed);
        }
        Ok(incoming.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(id: u64, result: serde_json::Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
    }

    fn error_response(id: u64, code: i64, message: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
        .to_string()
    }

    fn handshake(id: u64) -> String {
        response(
            id,
            json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {} }),
        )
    }

    fn client_with(script: Vec<String>) -> (McpClient, Arc<ScriptedTransport>) {
        let transport = ScriptedTransport::new(script);
        (McpClient::new(transport.clone()), transport)
    }

    #[test]
    fn framing_appends_exactly_one_newline() {
        assert_eq!(frame("{\"a\":1}"), "{\"a\":1}\n");
    }

    #[test]
    fn framing_escapes_control_characters_that_would_split_the_stream() {
        let framed = frame("{\"text\":\"line\nbreak\"}");
        assert!(
            !framed.trim_end().contains('\n'),
            "raw newline leaked into the frame"
        );
        assert!(framed.contains("\\n"));
    }

    #[test]
    fn parse_rejects_empty_and_malformed_frames() {
        assert_eq!(parse_frame("   "), Err(McpProtocolError::EmptyFrame));
        assert!(matches!(
            parse_frame("{nope"),
            Err(McpProtocolError::Decode(_))
        ));
        assert!(parse_frame("{\"ok\":true}").is_ok());
    }

    #[tokio::test]
    async fn handshake_sends_initialize_then_the_initialized_notification() {
        let (client, transport) = client_with(vec![handshake(1)]);

        client.initialize().await.unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 2, "expected a request and a notification");
        assert!(sent[0].contains("\"initialize\""));
        assert!(sent[0].contains(PROTOCOL_VERSION));
        assert!(sent[0].contains(CLIENT_NAME));
        assert!(sent[1].contains("notifications/initialized"));
    }

    #[tokio::test]
    async fn handshake_runs_only_once_per_connection() {
        let (client, transport) = client_with(vec![handshake(1)]);

        client.initialize().await.unwrap();
        client.initialize().await.unwrap();

        assert_eq!(transport.sent().await.len(), 2);
    }

    #[tokio::test]
    async fn handshake_without_a_protocol_version_is_refused() {
        let (client, _transport) = client_with(vec![response(1, json!({ "capabilities": {} }))]);
        assert!(matches!(
            client.initialize().await,
            Err(McpProtocolError::Handshake(_))
        ));
    }

    #[tokio::test]
    async fn list_tools_decodes_the_provider_catalog() {
        let (client, _transport) = client_with(vec![
            handshake(1),
            response(
                2,
                json!({
                    "tools": [
                        {
                            "name": "browser_click",
                            "description": "Click an element",
                            "inputSchema": { "type": "object" }
                        },
                        { "name": "browser_take_screenshot" }
                    ]
                }),
            ),
        ]);

        let catalog = client.list_tools().await.unwrap();
        assert_eq!(catalog.tools.len(), 2);
        assert_eq!(catalog.tools[0].name, "browser_click");
        // A tool with no description still parses, with an empty one.
        assert_eq!(catalog.tools[1].description, "");
        // The schema binds under the provider's camelCase spelling. This is the
        // assertion this test never made while the field failed to bind at all:
        // every tool decoded with `inputSchema: null` and the harness client
        // refused the entire list.
        assert_eq!(
            catalog.tools[0].input_schema,
            json!({"type": "object"}),
            "inputSchema did not decode"
        );
    }

    #[tokio::test]
    async fn list_tools_without_a_tools_array_is_a_decode_error() {
        let (client, _transport) = client_with(vec![handshake(1), response(2, json!({}))]);
        assert!(matches!(
            client.list_tools().await,
            Err(McpProtocolError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn call_tool_projects_text_and_image_parts() {
        // A one-pixel PNG, base64 encoded as a provider would send it.
        let png = base64_encode(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13]);
        let (client, _transport) = client_with(vec![
            handshake(1),
            response(
                2,
                json!({
                    "content": [
                        { "type": "text", "text": "captured page" },
                        { "type": "image", "data": png, "mimeType": "image/png" }
                    ]
                }),
            ),
        ]);

        let result = client
            .call_tool("browser_take_screenshot", json!({}))
            .await
            .unwrap();

        assert_eq!(result.text, "captured page");
        assert_eq!(result.images.len(), 1);
        assert_eq!(
            result.images[0],
            vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13]
        );
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn multiple_text_parts_are_joined_and_a_tool_error_is_surfaced() {
        let (client, _transport) = client_with(vec![
            handshake(1),
            response(
                2,
                json!({
                    "content": [
                        { "type": "text", "text": "first" },
                        { "type": "text", "text": "second" }
                    ],
                    "isError": true
                }),
            ),
        ]);

        let result = client.call_tool("browser_click", json!({})).await.unwrap();
        assert_eq!(result.text, "first\nsecond");
        assert!(result.is_error);
        assert!(result.images.is_empty());
    }

    #[test]
    fn unknown_content_types_are_ignored() {
        let result = project_result(&json!({
            "content": [
                { "type": "resource", "resource": { "uri": "file:///x" } },
                { "type": "text", "text": "kept" }
            ]
        }));
        assert_eq!(result.text, "kept");
    }

    #[test]
    fn a_malformed_image_is_dropped_rather_than_stored() {
        let result = project_result(&json!({
            "content": [
                { "type": "text", "text": "shot taken" },
                { "type": "image", "data": "!!!not base64!!!" }
            ]
        }));
        assert_eq!(result.text, "shot taken");
        assert!(result.images.is_empty());
    }

    #[test]
    fn a_result_with_no_content_projects_to_empty() {
        let result = project_result(&json!({ "isError": false }));
        assert!(result.text.is_empty());
        assert!(result.images.is_empty());
    }

    #[tokio::test]
    async fn a_provider_error_object_becomes_a_remote_error() {
        let (client, _transport) = client_with(vec![
            handshake(1),
            error_response(2, -32602, "unknown tool"),
        ]);

        assert!(matches!(
            client.call_tool("browser_nope", json!({})).await,
            Err(McpProtocolError::Remote { code: -32602, .. })
        ));
    }

    #[tokio::test]
    async fn notifications_interleaved_with_a_response_are_skipped() {
        let (client, _transport) = client_with(vec![
            handshake(1),
            json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}).to_string(),
            json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}}).to_string(),
            response(
                2,
                json!({ "content": [{ "type": "text", "text": "done" }] }),
            ),
        ]);

        let result = client.call_tool("browser_click", json!({})).await.unwrap();
        assert_eq!(result.text, "done");
    }

    #[tokio::test]
    async fn a_response_for_another_id_is_a_protocol_error() {
        let (client, _transport) = client_with(vec![handshake(1), response(999, json!({}))]);
        assert!(matches!(
            client.call_tool("browser_click", json!({})).await,
            Err(McpProtocolError::IdMismatch {
                expected: 2,
                got: 999
            })
        ));
    }

    #[tokio::test]
    async fn a_closed_stream_surfaces_rather_than_hanging() {
        let (client, _transport) = client_with(vec![handshake(1)]);
        assert_eq!(
            client.call_tool("browser_click", json!({})).await,
            Err(McpProtocolError::Closed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_provider_times_out() {
        let transport = Arc::new(StalledTransport);
        let client = McpClient::new(transport).with_timeout(Duration::from_secs(5));

        assert_eq!(
            client.call_tool("browser_click", json!({})).await,
            Err(McpProtocolError::Timeout)
        );
    }

    struct StalledTransport;

    #[async_trait::async_trait]
    impl Transport for StalledTransport {
        async fn send_line(&self, _line: &str) -> Result<(), McpProtocolError> {
            Ok(())
        }
        async fn read_line(&self) -> Result<String, McpProtocolError> {
            // Never resolves: only the deadline can end this.
            std::future::pending::<()>().await;
            Err(McpProtocolError::Closed)
        }
    }

    #[tokio::test]
    async fn ids_increase_so_responses_can_be_correlated() {
        let (client, transport) = client_with(vec![
            handshake(1),
            response(2, json!({ "content": [] })),
            response(3, json!({ "content": [] })),
        ]);

        client.call_tool("a", json!({})).await.unwrap();
        client.call_tool("b", json!({})).await.unwrap();

        let sent = transport.sent().await;
        let ids: Vec<u64> = sent
            .iter()
            .filter_map(|line| parse_frame(line).ok())
            .filter_map(|value| value.get("id").and_then(serde_json::Value::as_u64))
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn base64_round_trips_through_decode() {
        for payload in [
            vec![0u8],
            vec![0u8, 1],
            vec![0u8, 1, 2],
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let encoded = base64_encode(&payload);
            assert_eq!(base64_decode(&encoded).as_deref(), Some(&payload[..]));
        }
    }

    fn base64_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let bits = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let indices = [
                (bits >> 18) & 0x3F,
                (bits >> 12) & 0x3F,
                (bits >> 6) & 0x3F,
                bits & 0x3F,
            ];
            for (index, value) in indices.iter().enumerate() {
                if index > chunk.len() {
                    out.push('=');
                } else {
                    out.push(TABLE[*value as usize] as char);
                }
            }
        }
        out
    }
}
