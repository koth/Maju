//! `kodex-browser` local MCP server: the browser provider exposed to agents.
//!
//! Mirrors [`crate::web_tools_mcp`]: one `127.0.0.1` JSON-RPC server on `/mcp`
//! serves every session, and each session registers behind its own
//! `x-kodex-browser-token` so no session can address another's browser.
//!
//! The difference is where the tool list comes from. Web tools have a fixed
//! pair, so `tools/list` returns a constant. Browser tools are whatever the
//! selected provider advertises, so the list is read from the adapter's
//! discovered catalog on every call. That is what lets a provider upgrade or a
//! version pin change the surface without a Kodex release, and it is why this
//! server refuses to start with an empty catalog rather than serving nothing.

use crate::browser_mcp::{BrowserCallError, BrowserMcpService};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::http::StatusCode;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

type BoxBody = Full<Bytes>;
const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const TOKEN_HEADER: &str = "x-kodex-browser-token";

/// Per-token view of the browser adapter.
#[derive(Clone, Default)]
pub struct BrowserServerService {
    adapter: Option<Arc<BrowserMcpService>>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl BrowserServerService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(adapter: Arc<BrowserMcpService>) -> Self {
        Self {
            adapter: Some(adapter),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a session and return its token.
    pub fn register_session(&self, session_id: &str) -> anyhow::Result<String> {
        let adapter = self
            .adapter
            .clone()
            .ok_or_else(|| anyhow::anyhow!("browser MCP server has no adapter"))?;
        let token = adapter.register_session(session_id, false);
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(token.clone(), session_id.to_string());
        }
        Ok(token)
    }

    pub fn unregister_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(token);
        }
    }

    /// Drop a token's registration. Alias of [`Self::unregister_session`],
    /// named for callers holding a token they looked up by session id.
    pub fn unregister_token(&self, token: &str) {
        self.unregister_session(token);
    }

    /// Drop a session's registration, addressed by session id.
    ///
    /// Dropping the token also drops the lease, which is what disposes the
    /// session's browser; the caller does not need to reach the adapter.
    pub fn drop_session(&self, session_id: &str) {
        if let Some(token) = self.token_for_session(session_id) {
            self.unregister_session(&token);
        }
    }

    /// The adapter, but only if this token is still registered.
    fn adapter_for(&self, token: &str) -> Option<Arc<BrowserMcpService>> {
        let registered = self
            .sessions
            .lock()
            .ok()
            .is_some_and(|sessions| sessions.contains_key(token));
        if !registered {
            return None;
        }
        self.adapter.clone()
    }

    /// The token a session registered under.
    ///
    /// The sidebar panel drives the session's browser by session id rather
    /// than by holding a token, so the reverse index has to exist. There is at
    /// most one live token per session, because re-registering replaces it.
    pub fn token_for_session(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .iter()
            .find(|(_, registered)| registered.as_str() == session_id)
            .map(|(token, _)| token.clone())
    }

    /// Call a tool on a session's browser, addressed by session id.
    pub async fn call_for_session(
        &self,
        session_id: &str,
        exposed_name: &str,
        arguments: Value,
    ) -> Result<crate::browser_mcp::BrowserToolResult, crate::browser_mcp::BrowserCallError> {
        let token = self
            .token_for_session(session_id)
            .ok_or(crate::browser_mcp::BrowserCallError::UnknownSession)?;
        let adapter = self
            .adapter
            .clone()
            .ok_or(crate::browser_mcp::BrowserCallError::UnknownSession)?;
        adapter.call(&token, exposed_name, arguments).await
    }

    /// Session ids with a live registration.
    pub fn active_sessions(&self) -> Vec<String> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = sessions.values().cloned().collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn adapter_handle(&self) -> Option<Arc<BrowserMcpService>> {
        self.adapter.clone()
    }
}

/// The running server. Dropping it shuts the listener down and joins the thread.
pub struct BrowserServerHandle {
    url: String,
    service: BrowserServerService,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BrowserServerHandle {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn register_session(&self, session_id: &str) -> anyhow::Result<String> {
        self.service.register_session(session_id)
    }

    pub fn unregister_session(&self, token: &str) {
        self.service.unregister_session(token);
    }

    /// The per-token view, for tests that assert registration state without
    /// going over the wire.
    pub fn service_ref(&self) -> &BrowserServerService {
        &self.service
    }
}

impl Drop for BrowserServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One session's registration. Dropping it unregisters the token and, through
/// the adapter, disposes the session's browser.
pub struct BrowserServerLease {
    handle: Arc<BrowserServerHandle>,
    token: String,
}

impl BrowserServerLease {
    pub fn register(handle: Arc<BrowserServerHandle>, session_id: &str) -> anyhow::Result<Self> {
        let token = handle.register_session(session_id)?;
        Ok(Self { handle, token })
    }

    pub fn url(&self) -> &str {
        self.handle.url()
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for BrowserServerLease {
    fn drop(&mut self) {
        self.handle.unregister_session(&self.token);
    }
}

/// Start a browser MCP server bound to an ephemeral loopback port.
pub fn start_browser_mcp_server(
    adapter: Arc<BrowserMcpService>,
) -> anyhow::Result<BrowserServerHandle> {
    // Providers orphaned by a previous crashed run are reaped before anything
    // new starts, so a crash cannot accumulate browsers across restarts. The
    // reaper is best-effort: a failure here must not stop the server.
    let reaped = browser_service::orphan::reap_orphaned_browser_providers();
    if !reaped.is_empty() {
        tracing::info!(
            target: "app_core::browser",
            count = reaped.len(),
            "reclaimed orphaned browser providers at startup"
        );
    }

    let (addr_tx, addr_rx) = mpsc::sync_channel::<anyhow::Result<SocketAddr>>(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = BrowserServerService::with_adapter(adapter);
    let thread_service = service.clone();

    let thread = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = addr_tx.send(Err(error.into()));
                return;
            }
        };
        runtime.block_on(async move {
            let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = addr_tx.send(Err(error.into()));
                    return;
                }
            };
            let addr = match listener.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    let _ = addr_tx.send(Err(error.into()));
                    return;
                }
            };
            let _ = addr_tx.send(Ok(addr));
            run_server(listener, thread_service, shutdown_rx).await;
        });
    });

    let addr = addr_rx.recv().map_err(|error| anyhow::anyhow!(error))??;
    Ok(BrowserServerHandle {
        url: format!("http://{addr}/mcp"),
        service,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn run_server(
    listener: TcpListener,
    service: BrowserServerService,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let service = service.clone();
                tokio::task::spawn(async move {
                    let io = TokioIo::new(stream);
                    let _ = http1::Builder::new()
                        .serve_connection(io, service_fn(move |request| {
                            handle_http_request(request, service.clone())
                        }))
                        .await;
                });
            }
        }
    }
}

async fn handle_http_request(
    request: Request<Incoming>,
    service: BrowserServerService,
) -> Result<Response<BoxBody>, Infallible> {
    if request.method() != Method::POST || request.uri().path() != "/mcp" {
        return Ok(response(StatusCode::NOT_FOUND, "Not found"));
    }

    let request_session_id = request
        .headers()
        .get(MCP_SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    // Taken before the body is consumed, since `request_token` needs the headers.
    let Some(token) = request_token(&request) else {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized"}),
        ));
    };
    let Some(adapter) = service.adapter_for(&token) else {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized"}),
        ));
    };

    // The body is read before the token is checked, because the version probe
    // has to be recognised before authentication and the body can only be read
    // once. The probe is deliberately unauthenticated — a client sends it to
    // find out *whether* authentication is required, so a 401 is not an answer,
    // it is the absence of one, and the harness SDK treats it as terminal and
    // reports the server as disconnected.
    let body = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                json!({"error": format!("failed to read request body: {error}")}),
            ));
        }
    };
    let payload = match serde_json::from_slice::<Value>(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                json!({"error": format!("invalid json: {error}")}),
            ));
        }
    };

    // The version probe is answered between authentication and the session
    // check, and that is exactly where it belongs.
    //
    // It does carry the token — the SDK sends it, which was verified by
    // capturing the probe's headers rather than assumed — so authentication was
    // never the issue. What the probe cannot carry is a *session id*: sessions
    // are issued by `initialize`, and this request runs before that.
    // `json_rpc_requires_session` exempted notifications only, so the probe was
    // refused with a 401 the SDK reads as "this server needs an authProvider I do
    // not have" — terminal, and reported as `server is disconnected`.
    if let Some(reply) = crate::mcp_version::discover_response(&payload) {
        let id = payload.get("id").cloned().unwrap_or(Value::Null);
        return Ok(json_response(
            StatusCode::OK,
            json!({"jsonrpc": "2.0", "id": id, "result": reply}),
        ));
    }

    if json_rpc_requires_session(&payload) && request_session_id.as_deref() != Some(token.as_str())
    {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized: valid MCP session id is required"}),
        ));
    }

    let result = handle_json_rpc(payload, adapter, &token).await;
    Ok(match result {
        JsonRpcHttpResult::Response(payload) => {
            json_response_with_session(StatusCode::OK, payload, Some(&token))
        }
        JsonRpcHttpResult::Accepted => {
            empty_response_with_session(StatusCode::ACCEPTED, Some(&token))
        }
    })
}

fn request_token(request: &Request<Incoming>) -> Option<String> {
    if let Some(token) = request
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        return Some(token.to_string());
    }
    let header = request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    header.strip_prefix("Bearer ").map(str::to_string)
}

/// Whether this request must carry a matching MCP session id. A notification
/// is fire-and-forget and has no id to correlate, so it is allowed through.
fn json_rpc_requires_session(payload: &Value) -> bool {
    let is_notification = payload.get("method").is_some() && payload.get("id").is_none();
    !is_notification
}

enum JsonRpcHttpResult {
    Response(Value),
    Accepted,
}

async fn handle_json_rpc(
    payload: Value,
    adapter: Arc<BrowserMcpService>,
    token: &str,
) -> JsonRpcHttpResult {
    if let Some(batch) = payload.as_array() {
        let mut responses = Vec::new();
        for item in batch {
            if let Some(response) = handle_json_rpc_call(item.clone(), adapter.clone(), token).await
            {
                responses.push(response);
            }
        }
        return if responses.is_empty() {
            JsonRpcHttpResult::Accepted
        } else {
            JsonRpcHttpResult::Response(Value::Array(responses))
        };
    }
    match handle_json_rpc_call(payload, adapter, token).await {
        Some(response) => JsonRpcHttpResult::Response(response),
        None => JsonRpcHttpResult::Accepted,
    }
}

async fn handle_json_rpc_call(
    payload: Value,
    adapter: Arc<BrowserMcpService>,
    token: &str,
) -> Option<Value> {
    let id = payload.get("id").cloned().unwrap_or(Value::Null);
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": crate::mcp_version::negotiate(payload.get("params")),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kodex-browser", "version": env!("CARGO_PKG_VERSION")}
        })),
        // Priming happens here rather than at start-up: this is the first
        // moment a client can express interest, and the catalog only exists
        // once the provider has answered. Returning an empty list would tell
        // the client the capability has no tools, and a client that lists once
        // at mount time would never ask again.
        "tools/list" => match prime(&adapter, token).await {
            Ok(()) => Ok(json!({"tools": tool_schemas(&adapter)})),
            Err(error) => Err(error),
        },
        "tools/call" => {
            handle_tool_call(
                payload.get("params").cloned().unwrap_or_default(),
                adapter,
                token,
            )
            .await
        }
        _ => Err(json_rpc_error(
            -32601,
            format!("Method not found: {method}"),
        )),
    };

    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// Start the provider and install its catalog, if that has not happened yet.
///
/// Reports a protocol error rather than an empty tool list, because "the
/// provider is not installed" and "the provider has no tools" are different
/// problems and a client that cannot tell them apart will cache the empty one.
async fn prime(adapter: &Arc<BrowserMcpService>, token: &str) -> Result<(), Value> {
    let Some(session_id) = adapter.session_for_token(token) else {
        return Err(json_rpc_error(
            -32001,
            "This browser session is not registered.",
        ));
    };
    adapter
        .ensure_catalog(&session_id)
        .await
        .map(|_| ())
        .map_err(|error| {
            json_rpc_error(
                -32002,
                format!("The browser provider could not be started: {error}"),
            )
        })
}

/// The model-visible tool list, read from the adapter's discovered catalog.
///
/// The catalog is the source of truth, so this is a projection rather than a
/// constant. An empty list here means [`prime`] has not run, which only
/// happens if a caller reads this without going through `tools/list`.
fn tool_schemas(adapter: &BrowserMcpService) -> Vec<Value> {
    adapter
        .exposed_tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect()
}

async fn handle_tool_call(
    params: Value,
    adapter: Arc<BrowserMcpService>,
    token: &str,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| json_rpc_error(-32602, "Missing tool name"))?
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // A tool name is validated against the catalog before it is forwarded, and
    // the catalog only exists after a prime. A client that skips `tools/list`
    // and calls straight through must not be told the tool is unknown.
    if prime(&adapter, token).await.is_err() && !adapter.has_tools() {
        return Err(json_rpc_error(
            -32002,
            "The browser provider could not be started.",
        ));
    }

    match adapter.call(token, &name, arguments).await {
        Ok(result) => {
            // Captures are already persisted; the model gets a text reference
            // and the tool card gets a thumbnail, never inline image bytes.
            let text = result.model_text();
            let handles: Vec<_> = result
                .screenshots
                .iter()
                .map(|handle| {
                    json!({
                        "path": handle.path,
                        "width": handle.width,
                        "height": handle.height,
                        "byte_size": handle.byte_size,
                        "media_type": handle.media_type,
                    })
                })
                .collect();

            // The handles ride in a structured field rather than the text
            // content, so the client can map them onto the tool card without
            // re-parsing prose the model also reads.
            Ok(json!({
                "content": [{"type": "text", "text": text}],
                "isError": result.is_error,
                "structuredContent": { "screenshots": handles },
            }))
        }
        Err(error) => Ok(tool_error(error)),
    }
}

/// A failed call is reported as tool output, not a protocol error, so the agent
/// can read what went wrong and try something else.
fn tool_error(error: BrowserCallError) -> Value {
    json!({
        "content": [{"type": "text", "text": error.to_string()}],
        "isError": true,
    })
}

fn json_rpc_error(code: i64, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

fn response(status: StatusCode, body: &'static str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("static response builds")
}

fn json_response(status: StatusCode, payload: Value) -> Response<BoxBody> {
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("json response builds")
}

fn json_response_with_session(
    status: StatusCode,
    payload: Value,
    session: Option<&str>,
) -> Response<BoxBody> {
    let mut builder = Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.header(MCP_SESSION_ID_HEADER, session);
    }
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    builder
        .body(Full::new(Bytes::from(body)))
        .expect("json response builds")
}

fn empty_response_with_session(status: StatusCode, session: Option<&str>) -> Response<BoxBody> {
    let mut builder = Response::builder().status(status);
    if let Some(session) = session {
        builder = builder.header(MCP_SESSION_ID_HEADER, session);
    }
    builder
        .body(Full::new(Bytes::new()))
        .expect("empty response builds")
}

/// A probe that connects exactly the way `dsh-mcp-client` does.
///
/// The two options are the ones it configures and the ones that matter:
/// `versionNegotiation: { mode: "auto" }` is what triggers the pre-authentication
/// probe, and the name it claims is the namespace its tools are registered under.
const HARNESS_SDK_PROBE: &str = r#"
const [sdk, url, token, header] = process.argv.slice(2);
const { Client, StreamableHTTPClientTransport } = await import(sdk);

const client = new Client(
  { name: "dsh-mcp-client", version: "0.0.1" },
  { capabilities: {}, versionNegotiation: { mode: "auto" } },
);
try {
  await client.connect(
    new StreamableHTTPClientTransport(new URL(url), {
      requestInit: { headers: { [header]: token } },
    }),
  );
  console.log("connected");
  await client.close();
  console.log("OK");
} catch (error) {
  console.log("FAILED:", error?.message);
  process.exit(1);
}
"#;

/// The dsh SDK's ESM entry point, if this machine has dsh installed.
fn harness_sdk_entry() -> Option<String> {
    let dsh = std::process::Command::new("which")
        .arg("dsh")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())?;
    // `which dsh` gives a shim; the package sits beside it under the global root.
    let root = std::path::Path::new(&dsh)
        .parent()
        .and_then(std::path::Path::parent)?
        .join("lib/node_modules/@deepseek-ai/dsh/node_modules/@modelcontextprotocol/client/dist/index.mjs");
    root.is_file().then(|| root.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_service::{BrowserFactory, BrowserService};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use tempfile::TempDir;

    fn adapter(dir: &TempDir) -> Arc<BrowserMcpService> {
        let service = Arc::new(BrowserService::from_factory(
            BrowserFactory::new(
                workspace_model::BrowserSettings {
                    enabled: true,
                    ..workspace_model::BrowserSettings::default()
                },
                std::path::PathBuf::from("/usr/bin/node"),
                std::path::PathBuf::from("/pkg/@playwright/mcp"),
                std::path::PathBuf::from("/data"),
            )
            .without_spawn(),
        ));
        Arc::new(BrowserMcpService::new(
            service,
            Arc::new(crate::screenshot_pipeline::ScreenshotPipeline::new(
                dir.path().join("shots"),
            )),
        ))
    }

    /// Install a one-tool provider catalog so `tools/list` has something to
    /// project. Goes through the supported path rather than reaching into the
    /// adapter's private catalog slot.
    fn install_one_tool(adapter: &BrowserMcpService, name: &str) {
        let catalog = browser_service::catalog::ProviderCatalog {
            tools: vec![browser_service::catalog::ProviderTool {
                name: name.to_string(),
                description: format!("{name} description"),
                input_schema: json!({"type": "object"}),
            }],
        };
        adapter.install_catalog(&catalog).expect("catalog installs");
    }

    /// Connect, retrying briefly.
    ///
    /// The servers run on single-threaded runtimes, so under a parallel test
    /// run a connect can be refused while a sibling server is still binding.
    /// That is a scheduling artifact, not the behaviour under test, so the
    /// helper absorbs it rather than making the assertion flaky.
    fn connect_with_retry(address: &str) -> TcpStream {
        for attempt in 0..40 {
            match TcpStream::connect(address) {
                Ok(stream) => return stream,
                Err(error) if attempt == 39 => {
                    panic!("server at {address} never accepted: {error}")
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
        unreachable!("the loop either returns or panics")
    }

    /// Read a full HTTP response, tolerating a short read.
    fn read_all(stream: &mut TcpStream) -> String {
        let mut raw = String::new();
        let mut chunk = [0u8; 4096];
        // `read_to_string` can return before the peer finishes flushing when
        // the suite is under load, so read until the socket closes.
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => raw.push_str(&String::from_utf8_lossy(&chunk[..read])),
                Err(_) => break,
            }
        }
        raw
    }

    /// Minimal HTTP JSON-RPC round trip against a running server.
    fn rpc(
        handle: &BrowserServerHandle,
        token: &str,
        payload: Value,
        with_session_header: bool,
    ) -> (StatusCode, Value) {
        let address = handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        let mut stream = connect_with_retry(&address);

        let body = serde_json::to_vec(&payload).unwrap();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        if with_session_header {
            request.push_str(&format!("{MCP_SESSION_ID_HEADER}: {token}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();

        let raw = read_all(&mut stream);

        let status = raw
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or("{}");
        let value = serde_json::from_str(body.trim()).unwrap_or(json!({}));
        (status, value)
    }

    #[test]
    fn a_registered_session_gets_a_token() {
        let dir = TempDir::new().unwrap();
        let handle = start_browser_mcp_server(adapter(&dir)).unwrap();
        let service = BrowserServerService::with_adapter(adapter(&dir));

        let token = service.register_session("session-1").unwrap();
        assert!(token.starts_with("browser-"));
        assert!(handle.url().starts_with("http://127.0.0.1:"));
        assert!(handle.url().ends_with("/mcp"));
    }

    #[test]
    fn an_unregistered_token_is_refused() {
        let dir = TempDir::new().unwrap();
        let handle = start_browser_mcp_server(adapter(&dir)).unwrap();
        let payload = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});

        let (status, _) = rpc(&handle, "browser-999", payload, true);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_missing_token_header_is_refused() {
        let dir = TempDir::new().unwrap();
        let handle = start_browser_mcp_server(adapter(&dir)).unwrap();
        let address = handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        let mut stream = TcpStream::connect(&address).unwrap();
        let body = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();

        let raw = read_all(&mut stream);
        assert!(raw.starts_with("HTTP/1.1 401"), "got {raw}");
    }

    #[test]
    fn a_request_without_a_matching_session_id_is_refused() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let (status, _) = rpc(&handle, &token, payload, false);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_notification_is_accepted_without_a_session_id() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let (status, _) = rpc(&handle, &token, payload, false);
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[test]
    fn initialize_reports_the_server_identity() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let (status, body) = rpc(&handle, &token, payload, true);

        assert_eq!(status, StatusCode::OK);
        let info = &body["result"]["serverInfo"];
        assert_eq!(info["name"], "kodex-browser");
    }

    /// A client that names a revision gets that revision back.
    ///
    /// The dsh harness's MCP SDK only accepts `2025-11-25` and `2026-07-28`. A
    /// reply in anything else — which is what the hard-coded `2024-11-05` was —
    /// is refused, and the harness reports the server as disconnected rather than
    /// saying why. That is why the browser row never mounted, and it is the same
    /// reason the web-tools and image rows did not.
    #[test]
    fn initialize_answers_with_the_revision_the_client_asked_for() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        for requested in ["2025-11-25", "2026-07-28"] {
            let payload = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": requested, "capabilities": {}}
            });
            let (status, body) = rpc(&handle, &token, payload, true);

            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body["result"]["protocolVersion"], requested,
                "a tool-only server can speak what the client asked for"
            );
        }
    }

    /// The version probe is answered — with a valid token, and without a
    /// session id.
    ///
    /// This is the request that made every Kodex MCP server unreachable from a
    /// dsh session while a hand-written client could reach all three. The
    /// harness SDK negotiates protocol era before `initialize`, so the probe
    /// arrives carrying the token but with no session id, because sessions are
    /// what `initialize` hands out. The session check exempted notifications
    /// only, so the probe was refused with a 401 the SDK reads as a missing
    /// `authProvider` — terminal, reported as `server is disconnected`.
    ///
    /// Both halves matter and are asserted separately: the probe is answered,
    /// and the exemption is only for the session requirement, never for the
    /// token.
    #[test]
    fn the_version_probe_is_answered_with_a_token_and_without_a_session() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}
        });
        // `false` for the session header: this is the whole point.
        let (status, body) = rpc(&handle, &token, payload, false);

        assert_eq!(
            status,
            StatusCode::OK,
            "a 401 here is read by the client as a terminal auth problem: {body}"
        );
        let versions = body["result"]["supportedVersions"]
            .as_array()
            .unwrap_or_else(|| panic!("no supportedVersions: {body}"));
        for version in crate::mcp_version::SUPPORTED_VERSIONS {
            assert!(
                versions.contains(&json!(version)),
                "{version} not advertised: {body}"
            );
        }
    }

    /// The probe is a session-less request, not an unauthenticated one.
    ///
    /// It is tempting to answer it before the token check too, on the theory
    /// that a client asks it to find out whether authentication is required. The
    /// headers say otherwise: the SDK sends the configured token with the probe.
    /// The thing it cannot send is a session id.
    #[test]
    fn the_probe_exemption_does_not_also_exempt_the_token() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}
        });
        let (status, _) = rpc(&handle, "no-such-token", payload, false);
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "the probe carries the token; a bad one must still be refused"
        );
    }

    /// And nothing else lost its authentication.
    #[test]
    fn everything_else_still_requires_a_token() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}}
        });
        let (status, _) = rpc(&handle, "no-such-token", payload, false);
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let payload = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (status, _) = rpc(&handle, "no-such-token", payload, false);
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The real client, the real handshake, including the pre-authentication
    /// version probe.
    ///
    /// Every other test here speaks the protocol by hand, which is precisely how
    /// a client that never probes passed while the dsh harness could not connect
    /// at all. This one runs the same SDK dsh ships, with the same
    /// `versionNegotiation: { mode: "auto" }` it configures, so a regression in
    /// what the probe is answered with fails here rather than in a session.
    ///
    /// Skips when Node or the SDK is absent rather than failing, so it does not
    /// become a machine-dependent test.
    #[test]
    fn the_harness_sdk_connects_including_the_version_probe() {
        let Some(sdk) = harness_sdk_entry() else {
            eprintln!("skipped: the dsh MCP SDK is not installed here");
            return;
        };
        let Some(node) = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|dir| std::path::Path::new(dir).join("node"))
            .find(|candidate| candidate.is_file())
        else {
            eprintln!("skipped: no node on PATH");
            return;
        };

        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let script = dir.path().join("probe.mjs");
        std::fs::write(&script, HARNESS_SDK_PROBE).expect("write the probe script");

        let output = std::process::Command::new(node)
            .arg(&script)
            .arg(&sdk)
            .arg(handle.url())
            .arg(&token)
            .arg(TOKEN_HEADER)
            .output()
            .expect("run the probe");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the SDK dsh ships could not connect:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("OK"), "{stdout}");
    }

    /// The projection is empty until a prime has run.
    ///
    /// This used to be asserted over the wire, as `tools/list` returning an
    /// empty array — which is what the bug looked like from outside. The
    /// projection itself is still empty when nothing has primed it; what changed
    /// is that `tools/list` no longer reports that as the answer.
    #[test]
    fn the_projection_is_empty_until_something_primes_it() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        assert!(tool_schemas(&adapter).is_empty());
        assert!(!adapter.has_tools());

        install_one_tool(&adapter, "browser_click");
        assert_eq!(tool_schemas(&adapter).len(), 1);
    }

    #[test]
    fn tools_list_reflects_the_discovered_catalog() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        install_one_tool(&adapter, "browser_click");

        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (_, body) = rpc(&handle, &token, payload, true);

        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "mcp__playwright-mcp__browser_click");
        assert_eq!(tools[0]["description"], "browser_click description");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
    }

    #[test]
    fn an_unknown_tool_is_reported_as_tool_output_not_a_protocol_error() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        // A catalog, so the name check has something to fail against; the
        // provider itself is never started because the name does not match.
        install_one_tool(&adapter, "browser_click");
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "mcp__playwright-mcp__nope", "arguments": {}}
        });
        let (status, body) = rpc(&handle, &token, payload, true);

        // A protocol-level error would make the client give up on the call.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], true);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not available")
        );
    }

    /// A provider that cannot start must not be reported as "no tools".
    ///
    /// An empty list is a cacheable answer: a client that lists once at mount
    /// time would remember it and never ask again, so a transient launch failure
    /// would outlive the fault. An error is not cacheable that way.
    #[test]
    fn a_provider_that_cannot_start_is_an_error_not_an_empty_list() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        // No catalog and no runnable provider: the prime cannot succeed.
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (status, body) = rpc(&handle, &token, payload, true);

        assert_eq!(status, StatusCode::OK, "a JSON-RPC error, not an HTTP one");
        assert!(
            body.get("error").is_some(),
            "an empty tools list would be cached by the client: {body}"
        );
        assert!(
            !body["result"]["tools"].is_array(),
            "the failure must not look like a capability with no tools: {body}"
        );
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"});
        let (_, body) = rpc(&handle, &token, payload, true);

        assert_eq!(body["error"]["code"], -32601);
    }

    #[test]
    fn a_malformed_body_is_a_bad_request() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let address = handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        let mut stream = connect_with_retry(&address);
        let body = b"{not json";
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\n{TOKEN_HEADER}: {token}\r\n{MCP_SESSION_ID_HEADER}: {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();

        let raw = read_all(&mut stream);
        assert!(raw.starts_with("HTTP/1.1 400"), "got {raw}");
    }

    #[test]
    fn a_non_post_request_is_not_found() {
        let dir = TempDir::new().unwrap();
        let handle = start_browser_mcp_server(adapter(&dir)).unwrap();
        let address = handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        let mut stream = connect_with_retry(&address);
        stream
            .write_all(
                format!("GET /mcp HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        let raw = read_all(&mut stream);
        assert!(raw.starts_with("HTTP/1.1 404"), "got {raw}");
    }

    #[test]
    fn dropping_a_lease_unregisters_its_token() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = Arc::new(start_browser_mcp_server(adapter.clone()).unwrap());

        let lease = BrowserServerLease::register(handle.clone(), "session-1").unwrap();
        let token = lease.token().to_string();
        // Asserted through the service rather than over TCP: the property is
        // about the token table, and two real connections in a parallel test
        // run made this flaky for reasons unrelated to the behaviour.
        assert!(
            handle
                .service_ref()
                .token_for_session("session-1")
                .is_some()
        );

        drop(lease);

        assert!(
            handle
                .service_ref()
                .token_for_session("session-1")
                .is_none(),
            "the token must not survive its lease",
        );
    }

    #[test]
    fn a_dropped_lease_token_is_refused_over_the_wire() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = Arc::new(start_browser_mcp_server(adapter.clone()).unwrap());
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        assert_eq!(
            rpc(&handle, &token, payload.clone(), true).0,
            StatusCode::OK
        );

        handle.unregister_session(&token);

        // One connection per assertion: re-reading a drained stream would
        // otherwise depend on timing.
        assert_eq!(
            rpc(&handle, &token, payload, true).0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn dropping_the_handle_stops_the_server() {
        let dir = TempDir::new().unwrap();
        let handle = start_browser_mcp_server(adapter(&dir)).unwrap();
        let address = handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        drop(handle);

        // The listener is closed; a later connection attempt cannot succeed.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            TcpStream::connect_timeout(
                &address.parse().unwrap(),
                std::time::Duration::from_millis(200)
            )
            .is_err(),
            "server should stop listening once the handle is dropped",
        );
    }
}
