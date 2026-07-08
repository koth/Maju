use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::time::timeout;
use crate::error::{SdkError, SdkResult};
use crate::mcp;
use crate::options::{SdkCapabilities, SessionOptions};
use crate::protocol::messages::Message;
use crate::transport::SubprocessTransport;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Orchestrates the control protocol for one CLI session: background reader,
/// `control_request`/`control_response` pairing by `request_id`, reverse
/// `control_request` dispatch (`mcp_message`), and the `initialize` handshake.
pub struct Query {
    transport: Arc<SubprocessTransport>,
    options: SessionOptions,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<SdkResult<Value>>>>>,
    next_id: Mutex<u64>,
    session_id: Arc<Mutex<Option<String>>>,
    mcp_servers: Arc<Mutex<HashMap<String, Arc<mcp::server::SdkMcpServer>>>>,
    initialized: Mutex<bool>,
    has_sent_query: Mutex<bool>,
    control_timeout: Duration,
    /// Notified (true) on `shutdown()` so offloaded `tools/call` handler tasks
    /// (which may never resolve — the proxy's capture+interrupt tool strategy)
    /// abort instead of leaking when the session is torn down.
    close_tx: watch::Sender<bool>,
}
impl Query {
    pub fn new(
        transport: Arc<SubprocessTransport>,
        options: SessionOptions,
        mcp_servers: HashMap<String, Arc<mcp::server::SdkMcpServer>>,
    ) -> Self {
        let control_timeout = options
            .request_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        Self {
            transport,
            options,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Mutex::new(0),
            session_id: Arc::new(Mutex::new(None)),
            mcp_servers: Arc::new(Mutex::new(mcp_servers)),
            initialized: Mutex::new(false),
            has_sent_query: Mutex::new(false),
            control_timeout,
            close_tx: watch::channel(false).0,
        }
    }
    /// Start the background reader that routes control messages and buffers
    /// regular messages on the returned unbounded channel.
    pub async fn start(&self) -> SdkResult<mpsc::UnboundedReceiver<SdkResult<Message>>> {
        let mut rx = self
            .transport
            .take_messages()
            .await
            .ok_or_else(|| SdkError::Protocol("transport already taken".into()))?;
        let (out_tx, out_rx) = mpsc::unbounded_channel::<SdkResult<Message>>();
        let pending = self.pending.clone();
        let session_id = self.session_id.clone(); // Arc<Mutex<...>>, shared with reader task
        let mcp_servers = self.mcp_servers.clone();
        let transport = self.transport.clone();
        let close_rx = self.close_tx.subscribe();
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                match item {
                    Ok(v) => {
                        let ty = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                        match ty.as_str() {
                            "control_response" => {
                                handle_control_response(&v, &pending).await;
                            }
                            "control_request" => {
                                let req_id = v
                                    .get("request_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let subtype = v
                                    .get("request")
                                    .and_then(|r| r.get("subtype"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                if subtype == "mcp_message" {
                                    let server_name = v
                                        .get("request")
                                        .and_then(|r| r.get("server_name"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let message = v
                                        .get("request")
                                        .and_then(|r| r.get("message"))
                                        .cloned()
                                        .unwrap_or(Value::Null);
                                    // `tools/call` may never resolve: the proxy registers
                                    // capture+interrupt handlers that stall the CLI at the
                                    // `tool_use` until `session.interrupt()`. Awaiting it
                                    // inline would park this reader task and starve message
                                    // routing + the interrupt handshake. Offload it to its
                                    // own task, racing the session close signal so the task
                                    // is reaped on teardown instead of leaking. Other MCP
                                    // methods (initialize/tools/list/notifications) are
                                    // fast and stay inline to preserve handshake ordering.
                                    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
                                    if method == "tools/call" {
                                        let transport = transport.clone();
                                        let mcp_servers = mcp_servers.clone();
                                        let mut close_rx = close_rx.clone();
                                        let server_name = server_name.clone();
                                        let message = message.clone();
                                        let req_id = req_id.clone();
                                        tokio::spawn(async move {
                                            let result = tokio::select! {
                                                r = mcp::transport::handle_mcp_message(&mcp_servers, &server_name, &message) => r,
                                                _ = close_rx.changed() => return,
                                            };
                                            let mcp_response = match result {
                                                Ok(val) => val,
                                                Err(e) => json!({ "jsonrpc": "2.0", "error": { "code": -32603, "message": e.to_string() } }),
                                            };
                                            let reply = json!({
                                                "type": "control_response",
                                                "response": {
                                                    "subtype": "success",
                                                    "request_id": req_id,
                                                    "response": { "mcp_response": mcp_response },
                                                },
                                            });
                                            let _ = transport.write_json(&reply).await;
                                        });
                                    } else {
                                        write_mcp_reply(&transport, &mcp_servers, &server_name, &message, &req_id).await;
                                    }
                                } else {
                                    // can_use_tool / hook_callback: best-effort success
                                    let reply = json!({
                                        "type": "control_response",
                                        "response": {
                                            "subtype": "success",
                                            "request_id": req_id,
                                            "response": {},
                                        },
                                    });
                                    let _ = transport.write_json(&reply).await;
                                }
                            }
                            _ => {
                                // capture session_id from the first message that carries it
                                if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                                    let mut guard = session_id.lock().await;
                                    if guard.is_none() {
                                        *guard = Some(sid.to_string());
                                    }
                                }
                                let msg: Message = match serde_json::from_value(v) {
                                    Ok(m) => m,
                                    Err(e) => {
                                        let _ = out_tx.send(Err(SdkError::Json(e)));
                                        continue;
                                    }
                                };
                                let _ = out_tx.send(Ok(msg));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = out_tx.send(Err(e));
                    }
                }
            }
            // connection closed: resolve all pending waiters
            let mut pending_guard = pending.lock().await;
            for (_id, tx) in pending_guard.drain() {
                let _ = tx.send(Err(SdkError::StdinClosed));
            }
        });
        Ok(out_rx)
    }
    pub async fn session_id(&self) -> Option<String> {
        self.session_id.lock().await.clone()
    }
    /// Signal offloaded MCP `tools/call` handler tasks to abort. Called by
    /// `Session::close()`/`Drop` so never-resolving handlers (the proxy's
    /// capture+interrupt tools) don't leak spawned tasks past session lifetime.
    pub fn shutdown(&self) {
        let _ = self.close_tx.send(true);
    }
    pub async fn initialize(&self, has_prompt: bool) -> SdkResult<Value> {
        let mut init = *self.initialized.lock().await;
        if init {
            return Ok(json!({}));
        }
        let sdk_mcp_names: Vec<String> = {
            let guard = self.mcp_servers.lock().await;
            guard.keys().cloned().collect()
        };
        let caps = SdkCapabilities { ask_user_question: true };
        let mut payload = json!({
            "subtype": "initialize",
            "hasPrompt": has_prompt,
            "capabilities": caps,
        });
        if !sdk_mcp_names.is_empty() {
            payload["sdkMcpServers"] = json!(sdk_mcp_names);
        }
        if let Some(sp) = &self.options.system_prompt {
            payload["systemPrompt"] = json!(sp);
        }
        let resp = self.send_control_request(payload).await?;
        init = true;
        *self.initialized.lock().await = init;
        Ok(resp)
    }
    pub async fn interrupt(&self) -> SdkResult<()> {
        let req = json!({
            "type": "control_request",
            "request_id": format!("interrupt_{}", next_id(&self.next_id).await),
            "request": { "subtype": "interrupt" },
        });
        self.transport.write_json(&req).await
    }
    pub async fn send_user_message(&self, content: Value) -> SdkResult<()> {
        *self.has_sent_query.lock().await = true;
        let sid = self.session_id.lock().await.clone().unwrap_or_default();
        let msg = if content.is_string() {
            json!({
                "type": "user",
                "session_id": sid,
                "message": { "role": "user", "content": content },
                "parent_tool_use_id": null,
            })
        } else {
            let mut obj = content;
            if let Some(m) = obj.as_object_mut() {
                m.insert("type".to_string(), json!("user"));
                m.entry("session_id").or_insert(json!(sid));
                m.entry("parent_tool_use_id").or_insert(Value::Null);
            }
            obj
        };
        self.transport.write_json(&msg).await
    }
    pub async fn send_control_request(&self, payload: Value) -> SdkResult<Value> {
        let subtype = payload
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = format!("ctrl_{}", next_id(&self.next_id).await);
        let (tx, rx) = oneshot::channel::<SdkResult<Value>>();
        self.pending.lock().await.insert(id.clone(), tx);
        let req = json!({
            "type": "control_request",
            "request_id": id,
            "request": payload,
        });
        self.transport.write_json(&req).await?;
        match timeout(self.control_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(SdkError::ControlConnectionClosed { subtype })
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                // Capture the CLI's stderr at the timeout instant so the
                // caller sees why `initialize` never answered (auth,
                // crash, …) instead of an opaque 60s hang.
                let stderr = truncate_stderr(&self.transport.stderr_snapshot().await);
                Err(SdkError::ControlTimeout {
                    subtype,
                    timeout_ms: self.control_timeout.as_millis() as u64,
                    stderr,
                })
            }
        }
    }
}
async fn next_id(counter: &Mutex<u64>) -> u64 {
    let mut guard = counter.lock().await;
    let v = *guard;
    *guard += 1;
    v
}

/// Cap the stderr embedded in a [`SdkError::ControlTimeout`] so the proxy
/// log / HTTP error body stays bounded. The transport ring is already capped
/// at 200 lines; this is a second guard against pathological output, and
/// cuts on a UTF-8 char boundary so it never splits a codepoint.
fn truncate_stderr(s: &str) -> String {
    const MAX: usize = 4000;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[...truncated...]", &s[..end])
}
async fn write_mcp_reply(
    transport: &SubprocessTransport,
    mcp_servers: &Mutex<HashMap<String, Arc<mcp::server::SdkMcpServer>>>,
    server_name: &str,
    message: &Value,
    req_id: &str,
) {
    let result = mcp::transport::handle_mcp_message(mcp_servers, server_name, message).await;
    let mcp_response = match result {
        Ok(val) => val,
        Err(e) => json!({ "jsonrpc": "2.0", "error": { "code": -32603, "message": e.to_string() } }),
    };
    let reply = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": req_id,
            "response": { "mcp_response": mcp_response },
        },
    });
    let _ = transport.write_json(&reply).await;
}

async fn handle_control_response(
    v: &Value,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<SdkResult<Value>>>>>,
) {
    let resp = v.get("response").cloned().unwrap_or(Value::Null);
    let subtype = resp.get("subtype").and_then(Value::as_str).unwrap_or("");
    let request_id = resp.get("request_id").and_then(Value::as_str).unwrap_or("");
    if request_id.is_empty() {
        return;
    }
    let tx = pending.lock().await.remove(request_id);
    if let Some(tx) = tx {
        if subtype == "error" {
            let err = resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("control request failed")
                .to_string();
            let _ = tx.send(Err(SdkError::ControlError {
                subtype: subtype.to_string(),
                error: err,
            }));
        } else {
            let result = resp.get("response").cloned().unwrap_or(json!({}));
            let _ = tx.send(Ok(result));
        }
    }
}
