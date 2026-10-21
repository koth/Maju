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

use crate::browser_mcp::{ActTarget, BrowserCallError, BrowserMcpService};
use browser_service::ax::ObserveMode;
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

/// The server segment for Kodex's own browser tools.
///
/// The provider's tools arrive under its own name; observation is ours, and it
/// keeps a separate segment so a tool card says where it came from and so the
/// provider can be swapped without taking observation with it.
const OBSERVE_SERVER: &str = "browser-use";
/// The observation tool's leaf name.
const OBSERVE_TOOL: &str = "browser_observe";
/// The acting tool's leaf name.
const ACT_TOOL: &str = "browser_act";
/// The guidance tool's leaf name.
const DOCS_TOOL: &str = "browser_documentation";
/// The viewport tool's leaf name.
const VIEWPORT_TOOL: &str = "browser_viewport";

/// The exposed name of the observation tool.
fn observe_tool_name() -> String {
    browser_service::catalog::exposed_name(OBSERVE_SERVER, OBSERVE_TOOL)
}

/// The exposed name of the acting tool.
fn act_tool_name() -> String {
    browser_service::catalog::exposed_name(OBSERVE_SERVER, ACT_TOOL)
}

/// The exposed name of the guidance tool.
fn docs_tool_name() -> String {
    browser_service::catalog::exposed_name(OBSERVE_SERVER, DOCS_TOOL)
}

/// The exposed name of the viewport tool.
fn viewport_tool_name() -> String {
    browser_service::catalog::exposed_name(OBSERVE_SERVER, VIEWPORT_TOOL)
}

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

    /// Name the directory the provider's artifacts belong in (the visible
    /// workspace root), so a capture it writes by relative name lands where the
    /// conversation resolves it — and inside the webview's asset scope.
    pub fn set_work_dir(&self, dir: impl Into<std::path::PathBuf>) {
        if let Some(adapter) = self.adapter_handle() {
            adapter.service().set_work_dir(dir);
        }
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
    // check, and that is exactly where it belongs: it carries the token (the SDK
    // sends it — verified by capturing the probe's headers, not assumed) but
    // never a session id.
    //
    // For revision 2026-07-28 that is not a special case to exempt one request
    // at a time: that revision has *no* sessions, so no request ever carries an
    // id. Demanding one below refused every call and made the client report this
    // server as `server is disconnected`.
    if let Some(reply) = crate::mcp_version::discover_response(&payload) {
        let id = payload.get("id").cloned().unwrap_or(Value::Null);
        return Ok(json_response(
            StatusCode::OK,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": crate::mcp_version::wire_result("server/discover", reply)
            }),
        ));
    }

    // Only a *foreign* session id is refused. Revision 2026-07-28 has no
    // sessions to present, and the token already names this one.
    if crate::mcp_version::session_id_is_foreign(request_session_id.as_deref(), token.as_str()) {
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
            "serverInfo": {"name": "kodex-browser", "version": env!("CARGO_PKG_VERSION")},
            // Guidance rides on the handshake rather than on every tool
            // description: a client that surfaces it has already read it
            // before the first call, and one that drops it still has
            // `browser_documentation`.
            "instructions": crate::browser_docs::instructions(),
        })),
        // Priming happens here rather than at start-up: this is the first
        // moment a client can express interest, and the catalog only exists
        // once the provider has answered. Returning an empty list would tell
        // the client the capability has no tools, and a client that lists once
        // at mount time would never ask again.
        "tools/list" => match prime(&adapter, token).await {
            Ok(()) => Ok(json!({"tools": tool_schemas(&adapter)})),
            // A provider that will not start must not cost the agent the
            // ability to read the page. The observation tool is still true,
            // and seeing the page is what lets the agent explain the failure.
            Err(error) if error.get("code").and_then(Value::as_i64) == Some(-32002) => Ok(json!({
                "tools": [observe_schema(), act_schema(), viewport_schema(), docs_schema()]
            })),
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
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": crate::mcp_version::wire_result(method, result)
        }),
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
    let mut tools: Vec<Value> = adapter
        .exposed_tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect();
    // Observation is ours, so it is appended rather than discovered. It is
    // listed after the provider's tools on purpose: reading the page is the
    // step that follows an action, and the list reads in that order.
    tools.push(observe_schema());
    tools.push(act_schema());
    tools.push(viewport_schema());
    tools.push(docs_schema());
    tools
}

/// The model-facing contract for [`VIEWPORT_TOOL`].
///
/// The description spends most of its words on *when not to use it*: resizing a
/// browser to make a screenshot roomier is a real failure mode, and it changes
/// what the user sees.
fn viewport_schema() -> Value {
    json!({
        "name": viewport_tool_name(),
        "description": "Set the page viewport to a specific size, or clear the override with `reset`. \
Use it only when the user asks for a specific size, a named breakpoint, or a device width. \
Do not resize the browser to fit more content or make a screenshot look better — take a full-page screenshot instead. \
An override lasts until it is cleared, so call it with `reset` when the check is done.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "width": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Viewport width in CSS pixels."
                },
                "height": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Viewport height in CSS pixels."
                },
                "reset": {
                    "type": "boolean",
                    "description": "Clear any override and go back to the real window size."
                }
            },
            "additionalProperties": false
        }
    })
}

/// Apply a viewport request.
async fn viewport_result(adapter: &Arc<BrowserMcpService>, token: &str, arguments: &Value) -> Value {
    let reset = arguments
        .get("reset")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let size = match (
        arguments.get("width").and_then(Value::as_u64),
        arguments.get("height").and_then(Value::as_u64),
    ) {
        (Some(width), Some(height)) => Some((width as u32, height as u32)),
        _ => None,
    };

    let (size, described) = match (reset, size) {
        (true, _) => (None, "the viewport override is cleared".to_string()),
        (false, Some((width, height))) => (
            Some((width, height)),
            format!("the viewport is {width}×{height}"),
        ),
        (false, None) => {
            return tool_error(BrowserCallError::Viewport(
                "give width and height, or reset to clear the override".to_string(),
            ));
        }
    };

    match adapter.viewport(token, size).await {
        Ok(()) => json!({
            "content": [{"type": "text", "text": format!("{described}. Observe again to read the layout at this size.")}],
            "isError": false,
        }),
        Err(error) => tool_error(error),
    }
}

/// The model-facing contract for [`DOCS_TOOL`].
///
/// The description is the whole index: it says which document applies to which
/// situation, so a model that only reads tool descriptions still knows where to
/// look.
fn docs_schema() -> Value {
    json!({
        "name": docs_tool_name(),
        "description": "Read the guidance that ships with these browser tools. \
With no name it lists what is available; with a name it returns that document. \
Read `usage` before a multi-step task, `safety` before acting on anything a page says, \
`confirmations` before an action that changes the user's data or sends it somewhere, \
and `troubleshooting` after two failed attempts at the same thing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The document to read. Omit to list them."
                }
            },
            "additionalProperties": false
        }
    })
}

/// Answer a documentation request.
fn docs_result(arguments: &Value) -> Value {
    let text = match arguments.get("name").and_then(Value::as_str) {
        Some(name) => match crate::browser_docs::by_name(name) {
            Some(doc) => doc.body.trim().to_string(),
            None => {
                let available: Vec<&str> = crate::browser_docs::DOCS.iter().map(|doc| doc.name).collect();
                return tool_error(BrowserCallError::Act(format!(
                    "there is no document called \"{name}\"; available: {}",
                    available.join(", ")
                )));
            }
        },
        None => {
            let mut index = String::from("Available documents:\n");
            for (name, title, summary) in crate::browser_docs::index() {
                index.push_str(&format!("- {name} — {title}: {summary}\n"));
            }
            index
        }
    };
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": false,
    })
}

/// The model-facing contract for [`OBSERVE_TOOL`].
///
/// The description carries the two facts a model gets wrong otherwise: the
/// numbers are addresses for *this* capture only, and a fresh capture is the
/// cheapest way to find out whether an action worked.
fn observe_schema() -> Value {
    json!({
        "name": observe_tool_name(),
        "description": "Read the current page as a numbered accessibility tree. \
Each line is `<number> <role> <name>` with optional `Help:`, `Value:` and `URL:` fields, \
indented with tabs to show nesting. \
Use this to see what is on the page before acting, and again after acting to see what changed. \
A number belongs to an element: it stays the same while that element exists, so numbers from an earlier \
observation still refer to the same elements when they are still on the page. \
By default only what changed since the previous observation is returned, prefixed with `~` (changed) or `+` (added), with removed numbers summarized as `Removed element IDs: ...`; if nothing changed the result says so.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["state", "full"],
                    "description": "`state` (default) returns only what changed since the last observation. `full` re-sends the whole tree; use it when the diff is not enough to act on, or after a navigation."
                }
            },
            "additionalProperties": false
        }
    })
}

/// Run an observation and project it into an MCP tool result.
async fn observe_result(adapter: &Arc<BrowserMcpService>, token: &str, arguments: &Value) -> Value {
    let mode = match arguments.get("mode").and_then(Value::as_str) {
        Some("full") => ObserveMode::Full,
        _ => ObserveMode::State,
    };
    match adapter.observe(token, mode).await {
        Ok(observed) => json!({
            "content": [{"type": "text", "text": observed.text().unwrap_or_default()}],
            "isError": false,
        }),
        Err(error) => tool_error(error),
    }
}

/// The model-facing contract for [`ACT_TOOL`].
///
/// `observe` defaults to on. An action almost always changes the page, so the
/// cheapest useful call is "do it and tell me what changed" — one round trip
/// instead of two. Turning it off is for batching several actions before
/// looking, which is the only case where the intermediate states are waste.
fn act_schema() -> Value {
    json!({
        "name": act_tool_name(),
        "description": "Act on an element by the number it had in an observation, or on a point in the viewport. \
`click` and `double_click` press in the middle of the target; `fill` replaces a field's text, `type` inserts at the caret; \
`press` sends one key (`Enter`, `Tab`, `Escape`, `Backspace`, `Delete`, `Space`, arrows, `Home`, `End`, `PageUp`, `PageDown`); \
`scroll` moves the page by `delta_y` (and `delta_x`). \
Use a number whenever the element is in the observation. Use x and y only for something the tree does not expose — a canvas, a custom-drawn widget, a cross-origin frame — taking the coordinates from a screenshot. \
The result includes the same diff `browser_observe` would return, so a single action needs no follow-up call. \
Set `observe` to false when you are making several actions in a row and only want to look at the end. \
Numbers stay valid while the element exists, but the page may have changed since you read it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "index": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "The element number from an observation of the current page."
                },
                "x": {
                    "type": "number",
                    "description": "Viewport x in CSS pixels. Use with y instead of index."
                },
                "y": {
                    "type": "number",
                    "description": "Viewport y in CSS pixels. Use with x instead of index."
                },
                "action": {
                    "type": "string",
                    "enum": ["click", "double_click", "fill", "type", "press", "scroll"],
                    "description": "What to do to the target."
                },
                "value": {
                    "type": "string",
                    "description": "Text for `fill`/`type`, or the key name for `press`. Ignored by the other actions."
                },
                "delta_x": {
                    "type": "number",
                    "description": "Horizontal scroll distance in pixels, for `scroll`."
                },
                "delta_y": {
                    "type": "number",
                    "description": "Vertical scroll distance in pixels, for `scroll` (positive scrolls down)."
                },
                "observe": {
                    "type": "boolean",
                    "description": "Whether to include what changed on the page. Defaults to true."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }
    })
}

/// The text of an action result: what happened, then optionally what changed.
///
/// Kept pure so the wording — which is the only thing the model sees — is
/// testable without a browser.
fn act_text(action: &str, target: &str, observed: Option<&browser_service::ax::Observed>) -> String {
    let mut text = format!("{action} on {target}.");
    match observed {
        Some(observed) => {
            text.push('\n');
            text.push_str(observed.text().unwrap_or_default());
        }
        None => text.push_str(" Observe the page to see the result."),
    }
    text
}

/// Run an action and project it into an MCP tool result.
async fn act_result(adapter: &Arc<BrowserMcpService>, token: &str, arguments: &Value) -> Value {
    let index = arguments.get("index").and_then(Value::as_u64);
    let x = arguments.get("x").and_then(Value::as_f64);
    let y = arguments.get("y").and_then(Value::as_f64);

    // A number addresses an element; a point addresses whatever is drawn there.
    // Everything else is a malformed call rather than something to guess at.
    let (target, described) = match (index, x, y) {
        (Some(index), _, _) => (
            ActTarget::Element {
                index: index as usize,
            },
            format!("element {index}"),
        ),
        (None, Some(x), Some(y)) => (ActTarget::Point { x, y }, format!("point ({x}, {y})")),
        _ => {
            return tool_error(BrowserCallError::Act(
                "give an element index, or x and y for a point".to_string(),
            ));
        }
    };

    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("click");
    let value = arguments.get("value").and_then(Value::as_str);
    let scroll = {
        let dx = arguments.get("delta_x").and_then(Value::as_f64);
        let dy = arguments.get("delta_y").and_then(Value::as_f64);
        (dx.is_some() || dy.is_some()).then(|| (dx.unwrap_or(0.0), dy.unwrap_or(0.0)))
    };
    // Observing is the default because a single action almost always wants its
    // result; batching is the special case, and says so.
    let observe = arguments
        .get("observe")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if let Err(error) = adapter.act(token, target, action, value, scroll).await {
        return tool_error(error);
    }

    if !observe {
        return json!({
            "content": [{"type": "text", "text": act_text(action, &described, None)}],
            "isError": false,
        });
    }

    // The action already happened. A failed follow-up observation is worth
    // reporting, but it must not turn a successful click into an error the
    // model will retry.
    match adapter.observe(token, ObserveMode::State).await {
        Ok(observed) => json!({
            "content": [{"type": "text", "text": act_text(action, &described, Some(&observed))}],
            "isError": false,
        }),
        Err(error) => json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "{}\n(the page could not be observed afterwards: {error})",
                    act_text(action, &described, None)
                )
            }],
            "isError": false,
        }),
    }
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

    // Observation and actioning do not need the provider: they open their own
    // CDP connection to the same browser. Handled before priming so a provider
    // that cannot start still leaves the agent able to read and drive the page.
    if name == observe_tool_name() {
        return Ok(observe_result(&adapter, token, &arguments).await);
    }
    if name == act_tool_name() {
        return Ok(act_result(&adapter, token, &arguments).await);
    }
    if name == viewport_tool_name() {
        return Ok(viewport_result(&adapter, token, &arguments).await);
    }
    if name == docs_tool_name() {
        // Guidance needs neither a session nor a provider: it is shipped with
        // the binary, and a model that has lost its place should always be able
        // to re-read it.
        return Ok(docs_result(&arguments));
    }

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

    /// Revision 2026-07-28 sends no session id at all — it has no sessions — and
    /// that must be accepted: the token already names and authenticates the
    /// session. Refusing the absence was what disconnected this server.
    #[test]
    fn tools_list_without_any_session_id_is_accepted() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        install_one_tool(&adapter, "browser_click");
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let (status, body) = rpc(&handle, &token, payload, false);

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 5);
    }

    /// What the rule is actually about: a request may present no session id, but
    /// never another session's.
    #[test]
    fn another_sessions_id_is_refused() {
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
        let body = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .unwrap();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n{TOKEN_HEADER}: {token}\r\n{MCP_SESSION_ID_HEADER}: someone-elses-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();

        let raw = read_all(&mut stream);
        assert!(raw.starts_with("HTTP/1.1 401"), "got {raw}");
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

    /// `initialize` is answered without a session id, because it is the request
    /// that issues one.
    ///
    /// The exemption covered notifications and the version probe, but not the
    /// handshake itself, so the server answered the one request that could never
    /// carry a session id with a 401 — which the client reads as a terminal auth
    /// problem and reports as `server is disconnected`. `web_tools_mcp` and
    /// `image_mcp` have always exempted it; this is the test that says so.
    #[test]
    fn initialize_is_answered_before_a_session_id_exists() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2026-07-28", "capabilities": {}}
        });
        // `false` for the session header: the client cannot have one yet.
        let (status, body) = rpc(&handle, &token, payload, false);

        assert_eq!(
            status,
            StatusCode::OK,
            "a 401 on the handshake is terminal for the client: {body}"
        );
        assert_eq!(body["result"]["serverInfo"]["name"], "kodex-browser");
        assert_eq!(body["result"]["protocolVersion"], "2026-07-28");
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
        // `split_paths`, not `split(':')`: on Windows PATH is `;`-separated, and a
        // hard-coded `:` made this test skip on the one platform whose client it
        // exists to protect — so a regression here went unnoticed there.
        let Some(node) = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|dir| dir.join("node"))
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

    /// The provider projection is empty until a prime has run.
    ///
    /// This used to be asserted over the wire, as `tools/list` returning an
    /// empty array — which is what the bug looked like from outside. The
    /// projection itself is still empty when nothing has primed it; what changed
    /// is that `tools/list` no longer reports that as the answer. Observation is
    /// not part of that projection: it is ours and is always listed.
    #[test]
    fn the_projection_is_empty_until_something_primes_it() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        assert!(adapter.exposed_tools().is_empty());
        assert!(!adapter.has_tools());

        // Ours are listed regardless, because they do not depend on the provider.
        let schemas = tool_schemas(&adapter);
        assert_eq!(schemas.len(), 4, "observe, act, viewport and the guidance");
        assert_eq!(schemas[0]["name"], observe_tool_name());
        assert_eq!(schemas[1]["name"], act_tool_name());
        assert_eq!(schemas[2]["name"], viewport_tool_name());
        assert_eq!(schemas[3]["name"], docs_tool_name());

        install_one_tool(&adapter, "browser_click");
        assert_eq!(tool_schemas(&adapter).len(), 5);
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
        assert_eq!(tools.len(), 5, "the provider's tool and our four");
        assert_eq!(tools[0]["name"], "mcp__playwright-mcp__browser_click");
        assert_eq!(tools[0]["description"], "browser_click description");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(tools[1]["name"], observe_tool_name());
        assert_eq!(tools[1]["inputSchema"]["type"], "object");
        assert_eq!(tools[2]["name"], act_tool_name());
        assert_eq!(tools[2]["inputSchema"]["required"][0], "action");
        assert_eq!(tools[3]["name"], viewport_tool_name());
        assert_eq!(tools[4]["name"], docs_tool_name());
    }

    /// Observation is answered here, not forwarded to the provider.
    ///
    /// The catalog is empty in this test, so a forwarded call would come back
    /// as an unknown tool. Getting "no endpoint yet" instead is the proof that
    /// the tool belongs to this server.
    #[test]
    fn observe_is_served_without_a_provider() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": observe_tool_name(), "arguments": {}}
        });
        let (status, body) = rpc(&handle, &token, payload, true);

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("DevTools endpoint"),
            "expected the observation path to run and find no browser: {text}"
        );
    }

    /// The action result is what the model reads, so its wording is tested
    /// rather than assumed.
    #[test]
    fn an_action_result_says_what_happened_and_optionally_what_changed() {
        use browser_service::ax::Observed;

        let alone = act_text("click", "element 24", None);
        assert!(alone.starts_with("click on element 24."), "{alone}");
        assert!(alone.contains("Observe the page"), "{alone}");

        let unchanged = act_text("click", "point (300, 200)", Some(&Observed::Unchanged));
        assert!(
            unchanged.starts_with("click on point (300, 200)."),
            "{unchanged}"
        );
        assert!(
            unchanged.contains("no change in the accessibility tree"),
            "{unchanged}"
        );

        let changed = act_text(
            "fill",
            "element 12",
            Some(&Observed::Diff(
                "Diff (~ changed, + added, removed by ID range):\n~\t12 text field Customer, Value: A\n"
                    .to_string(),
            )),
        );
        assert!(changed.starts_with("fill on element 12."), "{changed}");
        assert!(changed.contains("~\t12 text field Customer"), "{changed}");
    }

    /// The flag exists, defaults to on, and is not required — a model that never
    /// reads the schema still gets the useful behaviour.
    #[test]
    fn the_act_schema_offers_observation_by_default() {
        let schema = act_schema();
        let observe = &schema["inputSchema"]["properties"]["observe"];
        assert_eq!(observe["type"], "boolean", "{observe}");
        let required = schema["inputSchema"]["required"].as_array().unwrap();
        assert!(
            !required.iter().any(|field| field == "observe"),
            "observation is a default, not an obligation"
        );
    }

    /// A call with neither a number nor a point cannot be aimed at anything.
    #[test]
    fn act_needs_a_target() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": act_tool_name(), "arguments": {"action": "click"}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("element index, or x and y"), "got {text}");
    }

    /// A scroll with no distance is a typo, and the parser says so before any
    /// browser is involved.
    #[test]
    fn act_rejects_a_scroll_without_a_distance() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": {"name": act_tool_name(), "arguments": {"action": "scroll", "x": 10, "y": 10}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("delta_y"), "got {text}");
    }

    /// The handshake carries the guidance, so a client that surfaces
    /// `instructions` has it before the first tool call.
    #[test]
    fn initialize_carries_the_browser_guidance() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 20, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25"}
        });
        let (_, body) = rpc(&handle, &token, payload, false);

        let instructions = body["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("Working with the browser"), "{instructions}");
        assert!(
            instructions.contains("browser_documentation"),
            "the index of what can be read is missing"
        );
    }

    /// The guidance tool answers without a session, a provider, or a page.
    #[test]
    fn documentation_is_always_readable() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let listing = json!({
            "jsonrpc": "2.0", "id": 21, "method": "tools/call",
            "params": {"name": docs_tool_name(), "arguments": {}}
        });
        let (_, body) = rpc(&handle, &token, listing, true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("usage"), "{text}");
        assert!(text.contains("troubleshooting"), "{text}");

        let one = json!({
            "jsonrpc": "2.0", "id": 22, "method": "tools/call",
            "params": {"name": docs_tool_name(), "arguments": {"name": "usage"}}
        });
        let (_, body) = rpc(&handle, &token, one, true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("browser_observe"), "{text}");
    }

    /// An unknown document names the ones that exist instead of failing blankly.
    #[test]
    fn an_unknown_document_lists_the_real_ones() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 23, "method": "tools/call",
            "params": {"name": docs_tool_name(), "arguments": {"name": "cookbook"}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("there is no document called"), "{text}");
        assert!(text.contains("safety"), "{text}");
    }

    /// A viewport call with neither a size nor a reset is a typo.
    #[test]
    fn viewport_needs_a_size_or_a_reset() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 24, "method": "tools/call",
            "params": {"name": viewport_tool_name(), "arguments": {}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("reset"), "got {text}");
    }

    /// Acting before observing has no numbers to act on, and says so.    #[test]
    fn act_without_an_observation_is_refused() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": act_tool_name(), "arguments": {"index": 2, "action": "click"}}
        });
        let (status, body) = rpc(&handle, &token, payload, true);

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("browser_observe first"), "got {text}");
    }

    /// A missing index is a malformed call, not an element that cannot be found.
    #[test]
    fn act_requires_an_index() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": act_tool_name(), "arguments": {"action": "click"}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("element index"), "got {text}");
    }

    /// An unknown action fails before a browser is ever needed.
    #[test]
    fn act_rejects_an_unknown_action_without_touching_the_page() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": {"name": act_tool_name(), "arguments": {"index": 1, "action": "hover"}}
        });
        let (_, body) = rpc(&handle, &token, payload, true);

        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("click, double_click, fill"), "got {text}");
    }

    /// An unregistered token cannot observe anything.    ///
    /// The refusal happens at the transport, before any tool name is read:
    /// the token *is* the session, so an unknown one is an authentication
    /// failure rather than a tool error.
    #[test]
    fn observe_refuses_an_unregistered_token() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();

        let payload = json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": observe_tool_name(), "arguments": {"mode": "full"}}
        });
        let (status, _body) = rpc(&handle, "browser-999", payload, true);

        assert_eq!(status, StatusCode::UNAUTHORIZED);
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
    /// would outlive the fault. The list is therefore never empty while
    /// observation is available — the agent keeps the ability to read the page,
    /// which is what lets it explain the failure instead of guessing at it.
    #[test]
    fn a_provider_that_cannot_start_still_leaves_observation_available() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter(&dir);
        // No catalog and no runnable provider: the prime cannot succeed.
        let handle = start_browser_mcp_server(adapter.clone()).unwrap();
        let token = handle.register_session("session-1").unwrap();

        let payload = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (status, body) = rpc(&handle, &token, payload, true);

        assert_eq!(status, StatusCode::OK, "a JSON-RPC error, not an HTTP one");
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools.len(),
            4,
            "only our tools survive a broken provider: {body}"
        );
        assert_eq!(tools[0]["name"], observe_tool_name());
        assert_eq!(tools[1]["name"], act_tool_name());
        assert_eq!(tools[2]["name"], viewport_tool_name());
        assert_eq!(tools[3]["name"], docs_tool_name());
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
