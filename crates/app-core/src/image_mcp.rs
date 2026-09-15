//! Unified `kodex-image` local MCP server.
//!
//! Mirrors the `web_tools_mcp` HTTP-MCP pattern: a `127.0.0.1` JSON-RPC server
//! bound to `/mcp`, authenticated with an `x-kodex-image-token` header. The
//! server exposes up to three tools — `view_image`, `generate_image`,
//! `edit_image` — but `tools/list` is dynamically trimmed to only the tools
//! whose native counterpart is missing for the session that is asking
//! (`ImageCapabilities`). `tools/call` rejects any tool not in that trimmed set.
//!
//! **One process serves every session.** The assistant channels and the
//! DeepSeek Harness all point at the same server
//! ([`crate::shared_mcp`]); what differs per session is the registration
//! behind its own token: its capability set (so trimming stays per model) and
//! its [`ImageMcpConfig`] (so generated images land in that session's
//! workspace). `view_cache` is deliberately shared across sessions — the same
//! image is described once, not once per session.
//!
//! The server also advertises a `kodex-image://tools` resource describing the
//! currently mounted tool set, so client-side `list_mcp_resources` /
//! `read_mcp_resource` can surface what is mounted even though this server
//! exposes tools rather than file-like resources.

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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;
use workspace_model::{ImageCapabilities, ImageSettings};

use crate::image_api::{ImageApi, ViewCache};

const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const TOKEN_HEADER: &str = "x-kodex-image-token";
const TOOLS_RESOURCE_URI: &str = "kodex-image://tools";

/// Configuration carried by one session's image MCP registration.
#[derive(Clone)]
pub struct ImageMcpConfig {
    pub workspace_root: PathBuf,
    pub settings: ImageSettings,
    pub view_api_key: Option<String>,
    pub generate_api_key: Option<String>,
}

/// One registered session: the capabilities its `tools/list` is trimmed by and
/// the config its tool calls run with.
#[derive(Clone)]
pub struct ImageSessionState {
    caps: ImageCapabilities,
    config: Arc<ImageMcpConfig>,
}

impl ImageSessionState {
    pub fn capabilities(&self) -> ImageCapabilities {
        self.caps
    }

    pub fn config(&self) -> &ImageMcpConfig {
        &self.config
    }
}

/// Shared server state: the per-token session registry plus the cross-session
/// view cache.
#[derive(Clone)]
pub struct ImageMcpService {
    sessions: Arc<Mutex<HashMap<String, ImageSessionState>>>,
    view_cache: Arc<Mutex<ViewCache>>,
}

impl Default for ImageMcpService {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageMcpService {
    /// An empty server. Sessions register through [`Self::register_session`].
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            view_cache: Arc::new(Mutex::new(ViewCache::default())),
        }
    }

    /// Register one session and return its token.
    pub fn register_session(&self, caps: ImageCapabilities, config: ImageMcpConfig) -> String {
        let token = Uuid::new_v4().to_string();
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(
                token.clone(),
                ImageSessionState {
                    caps,
                    config: Arc::new(config),
                },
            );
        }
        token
    }

    /// Drop one session's registration. Unknown tokens are ignored.
    pub fn unregister_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(token);
        }
    }

    /// Replace one session's capabilities (model switch) without restarting the
    /// server; a later `tools/list` for that token reflects the new set.
    pub fn update_capabilities(&self, token: &str, caps: ImageCapabilities) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(token)
        {
            session.caps = caps;
        }
    }

    /// The session behind one token, or `None` when the token is unknown.
    pub fn session(&self, token: &str) -> Option<ImageSessionState> {
        self.sessions.lock().ok()?.get(token).cloned()
    }

    /// Cross-session view cache, shared by every registration.
    pub fn view_cache(&self) -> Arc<Mutex<ViewCache>> {
        self.view_cache.clone()
    }
}

pub struct ImageMcpHandle {
    url: String,
    service: ImageMcpService,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ImageMcpHandle {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Register one session on this server and return its token.
    pub fn register_session(&self, caps: ImageCapabilities, config: ImageMcpConfig) -> String {
        self.service.register_session(caps, config)
    }

    pub fn unregister_session(&self, token: &str) {
        self.service.unregister_session(token);
    }

    pub fn update_capabilities(&self, token: &str, caps: ImageCapabilities) {
        self.service.update_capabilities(token, caps);
    }

    /// The capabilities registered for one session token.
    pub fn capabilities(&self, token: &str) -> Option<ImageCapabilities> {
        self.service.session(token).map(|state| state.capabilities())
    }

    /// Shared cross-session view cache.
    pub fn view_cache(&self) -> Arc<Mutex<ViewCache>> {
        self.service.view_cache()
    }
}

impl Drop for ImageMcpHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One session's registration on an image MCP server, usually the shared one.
///
/// Dropping the lease unregisters the session token, so a session that ends
/// stops being addressable on the shared server while every other session keeps
/// working. The handle is kept alive by the lease so a lease can never outlive
/// the server it points at.
pub struct ImageMcpLease {
    handle: Arc<ImageMcpHandle>,
    token: String,
    config: ImageMcpConfig,
}

impl ImageMcpLease {
    /// Register `config`/`caps` on `handle` and return the lease.
    pub fn register(
        handle: Arc<ImageMcpHandle>,
        caps: ImageCapabilities,
        config: ImageMcpConfig,
    ) -> Self {
        let token = handle.register_session(caps, config.clone());
        Self {
            handle,
            token,
            config,
        }
    }

    pub fn url(&self) -> &str {
        self.handle.url()
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// This session's config, for prompt-level `view_image` degradation.
    pub fn config(&self) -> ImageMcpConfig {
        self.config.clone()
    }

    /// The shared view cache, so prompt-level degradation and the MCP tool
    /// share results.
    pub fn view_cache(&self) -> Arc<Mutex<ViewCache>> {
        self.handle.view_cache()
    }

    pub fn update_capabilities(&self, caps: ImageCapabilities) {
        self.handle.update_capabilities(&self.token, caps);
    }

    /// This session's registered capabilities.
    pub fn capabilities(&self) -> ImageCapabilities {
        self.handle
            .capabilities(&self.token)
            .unwrap_or_else(|| ImageCapabilities::default())
    }
}

impl Drop for ImageMcpLease {
    fn drop(&mut self) {
        self.handle.unregister_session(&self.token);
    }
}

pub fn start_image_mcp_server() -> anyhow::Result<ImageMcpHandle> {
    let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel::<anyhow::Result<SocketAddr>>(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = ImageMcpService::new();
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
    Ok(ImageMcpHandle {
        url: format!("http://{addr}/mcp"),
        service,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn run_server(
    listener: TcpListener,
    service: ImageMcpService,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    continue;
                };
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

type BoxBody = Full<Bytes>;

async fn handle_http_request(
    request: Request<Incoming>,
    service: ImageMcpService,
) -> Result<Response<BoxBody>, Infallible> {
    if request.method() != Method::POST || request.uri().path() != "/mcp" {
        return Ok(response(StatusCode::NOT_FOUND, "Not found"));
    }
    // The token names the session, so it is also the MCP session id: every
    // client (one per assistant session, one per harness process) gets its own,
    // and a request may never carry another session's id.
    let Some(token) = request_token(&request) else {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized"}),
        ));
    };
    let Some(session) = service.session(&token) else {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized"}),
        ));
    };
    let request_session_id = request
        .headers()
        .get(MCP_SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
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
    if json_rpc_requires_session(&payload) && request_session_id.as_deref() != Some(token.as_str()) {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized: valid MCP session id is required"}),
        ));
    }
    let result = handle_json_rpc(payload, service, session).await;
    Ok(match result {
        JsonRpcHttpResult::Response(payload) => {
            json_response_with_session(StatusCode::OK, payload, Some(&token))
        }
        JsonRpcHttpResult::Accepted => {
            empty_response_with_session(StatusCode::ACCEPTED, Some(&token))
        }
    })
}

/// The session token carried by one request, from the Kodex header or a bearer
/// authorization.
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
    service: ImageMcpService,
    session: ImageSessionState,
) -> JsonRpcHttpResult {
    if let Some(batch) = payload.as_array() {
        let mut responses = Vec::new();
        for item in batch {
            if let Some(response) =
                handle_json_rpc_call(item.clone(), service.clone(), session.clone()).await
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
    match handle_json_rpc_call(payload, service, session).await {
        Some(response) => JsonRpcHttpResult::Response(response),
        None => JsonRpcHttpResult::Accepted,
    }
}

async fn handle_json_rpc_call(
    payload: Value,
    service: ImageMcpService,
    session: ImageSessionState,
) -> Option<Value> {
    let id = payload.get("id").cloned().unwrap_or(Value::Null);
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let caps = session.capabilities();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kodex-image", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools": trimmed_tool_schemas(&caps)})),
        "tools/call" => {
            let result =
                handle_tool_call(payload.get("params").cloned().unwrap_or_default(), &service, &session)
                    .await;
            return Some(json_rpc_call_result(id, result));
        }
        "resources/list" => Ok(json!({"resources": [tool_manifest_resource()]})),
        "resources/templates" => Ok(json!({"resourceTemplates": []})),
        "resources/read" => {
            let result =
                handle_resource_read(payload.get("params").cloned().unwrap_or_default(), &caps);
            return Some(json_rpc_call_result(id, result));
        }
        _ => Err(json_rpc_error(
            -32601,
            format!("Method not found: {method}"),
        )),
    };

    Some(json_rpc_call_result(id, result))
}

fn json_rpc_call_result(id: Value, result: Result<Value, Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

/// The resource advertised by `resources/list`: a single manifest describing
/// the currently mounted image tools, trimmed to the session's capabilities.
fn tool_manifest_resource() -> Value {
    json!({
        "uri": TOOLS_RESOURCE_URI,
        "name": "kodex-image mounted tools",
        "description": "Tools currently mounted for this session (trimmed by model image capabilities): view_image, generate_image, edit_image.",
        "mimeType": "application/json",
    })
}

fn handle_resource_read(params: Value, caps: &ImageCapabilities) -> Result<Value, Value> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| json_rpc_error(-32602, "Missing resource uri"))?;
    if uri != TOOLS_RESOURCE_URI {
        return Err(json_rpc_error(-32002, format!("Resource not found: {uri}")));
    }
    let text = serde_json::to_string_pretty(&trimmed_tool_schemas(caps))
        .unwrap_or_else(|_| "[]".to_string());
    Ok(json!({
        "contents": [{
            "uri": TOOLS_RESOURCE_URI,
            "mimeType": "application/json",
            "text": text,
        }]
    }))
}

/// Tool names offered for the given capabilities (the trimmed set).
fn offered_tools(caps: &ImageCapabilities) -> Vec<&'static str> {
    let mut tools = Vec::new();
    if !caps.native_view {
        tools.push("view_image");
    }
    if !caps.native_generate {
        tools.push("generate_image");
    }
    // native_edit is always false; edit_image is always offered.
    tools.push("edit_image");
    tools
}

fn trimmed_tool_schemas(caps: &ImageCapabilities) -> Vec<Value> {
    offered_tools(caps)
        .into_iter()
        .map(|name| tool_schema(name).expect("tool schema must exist"))
        .collect()
}

fn tool_schema(name: &str) -> Option<Value> {
    Some(match name {
        "view_image" => json!({
            "name": "view_image",
            "description": "Understand a local image. Reads the image at the given file:// path and returns a text description (optionally answering a question). Use this when the current model cannot directly view images.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "image_path": {"type": "string", "description": "Local file:// path to the image"},
                    "question": {"type": "string", "description": "Optional question about the image"}
                },
                "required": ["image_path"]
            }
        }),
        "generate_image" => json!({
            "name": "generate_image",
            "description": "Generate a new image from a text prompt. The result is persisted to the workspace and returned as a file:// path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "Text description of the image to generate"},
                    "size": {"type": "string", "description": "Image size, e.g. 1024x1024"},
                    "n": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Number of images to generate"}
                },
                "required": ["prompt"]
            }
        }),
        "edit_image" => json!({
            "name": "edit_image",
            "description": "Edit an existing image. The original image (read from a local file:// path) and the edit prompt are passed directly to the generation model; the edited result is persisted to the workspace and returned as a file:// path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "image_path": {"type": "string", "description": "Local file:// path to the original image"},
                    "prompt": {"type": "string", "description": "Edit instruction"},
                    "mask_path": {"type": "string", "description": "Optional local file:// path to an edit mask"}
                },
                "required": ["image_path", "prompt"]
            }
        }),
        _ => return None,
    })
}

async fn handle_tool_call(
    params: Value,
    service: &ImageMcpService,
    session: &ImageSessionState,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| json_rpc_error(-32602, "Missing tool name"))?;
    let caps = session.capabilities();
    if !offered_tools(&caps).contains(&name) {
        return Err(json_rpc_error(
            -32602,
            "tool not available in current capability mode",
        ));
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let api = ImageApi::new(session.config().clone(), service.view_cache());
    let result = match name {
        "view_image" => api.view_image(&arguments).await,
        "generate_image" => api.generate_image(&arguments).await,
        "edit_image" => api.edit_image(&arguments).await,
        _ => return Err(json_rpc_error(-32602, format!("Unknown tool: {name}"))),
    };
    match result {
        Ok(value) => tool_success(value),
        Err(error) => Ok(tool_error(&error)),
    }
}

fn tool_success(value: Value) -> Result<Value, Value> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": value,
        "isError": false
    }))
}

fn tool_error(error: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": error}],
        "isError": true
    })
}

fn json_rpc_error(code: i64, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

fn json_rpc_requires_session(payload: &Value) -> bool {
    if let Some(batch) = payload.as_array() {
        return batch.iter().any(json_rpc_requires_session);
    }
    payload.get("method").and_then(Value::as_str) != Some("initialize")
}

fn json_response(status: StatusCode, payload: Value) -> Response<BoxBody> {
    json_response_with_session(status, payload, None)
}

fn json_response_with_session(
    status: StatusCode,
    payload: Value,
    session_id: Option<&str>,
) -> Response<BoxBody> {
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    let mut builder = Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json");
    if let Some(session_id) = session_id {
        builder = builder.header(MCP_SESSION_ID_HEADER, session_id);
    }
    builder
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| response(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

fn empty_response_with_session(status: StatusCode, session_id: Option<&str>) -> Response<BoxBody> {
    let mut builder = Response::builder().status(status);
    if let Some(session_id) = session_id {
        builder = builder.header(MCP_SESSION_ID_HEADER, session_id);
    }
    builder
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn response(status: StatusCode, body: impl Into<Bytes>) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(Full::new(body.into()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn run_async<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn config() -> ImageMcpConfig {
        ImageMcpConfig {
            workspace_root: std::env::temp_dir(),
            settings: ImageSettings::default(),
            view_api_key: None,
            generate_api_key: None,
        }
    }

    /// A private server with one registered session, exposing the same surface
    /// the tests were written against (`url`/`token`/`update_capabilities`).
    struct TestServer {
        handle: Arc<ImageMcpHandle>,
        token: String,
    }

    impl TestServer {
        fn url(&self) -> &str {
            self.handle.url()
        }

        fn token(&self) -> &str {
            &self.token
        }

        fn update_capabilities(&self, caps: ImageCapabilities) {
            self.handle.update_capabilities(&self.token, caps);
        }
    }

    fn service(caps: ImageCapabilities) -> TestServer {
        let handle = Arc::new(start_image_mcp_server().unwrap());
        let token = handle.register_session(caps, config());
        TestServer { handle, token }
    }

    async fn initialize(client: &reqwest::Client, handle: &TestServer) -> String {
        let response = client
            .post(handle.url())
            .header(TOKEN_HEADER, handle.token())
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1.0"}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response
            .headers()
            .get(MCP_SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let payload: Value = response.json().await.unwrap();
        assert_eq!(
            payload["result"]["serverInfo"]["name"].as_str(),
            Some("kodex-image")
        );
        session_id
    }

    async fn list_tools(
        client: &reqwest::Client,
        handle: &TestServer,
        session_id: &str,
    ) -> Vec<String> {
        let response: Value = client
            .post(handle.url())
            .header(TOKEN_HEADER, handle.token())
            .header(MCP_SESSION_ID_HEADER, session_id)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn lists_only_missing_tools_for_text_only_byok() {
        let handle = service(ImageCapabilities {
            native_view: false,
            native_generate: false,
            native_edit: false,
            view_fallback: false,
        });
        let tools = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            list_tools(&client, &handle, &session_id).await
        });
        assert!(tools.contains(&"view_image".to_string()));
        assert!(tools.contains(&"generate_image".to_string()));
        assert!(tools.contains(&"edit_image".to_string()));
    }

    #[test]
    fn omits_view_and_generate_when_native_available() {
        let handle = service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        });
        let tools = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            list_tools(&client, &handle, &session_id).await
        });
        assert!(!tools.contains(&"view_image".to_string()));
        assert!(!tools.contains(&"generate_image".to_string()));
        assert!(tools.contains(&"edit_image".to_string()));
    }

    #[test]
    fn rejects_missing_token() {
        let handle = service(ImageCapabilities::default());
        let status = run_async(async {
            reqwest::Client::new()
                .post(handle.url())
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .send()
                .await
                .unwrap()
                .status()
        });
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn rejects_tool_not_in_trimmed_set() {
        let handle = service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        });
        let response: Value = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            client
                .post(handle.url())
                .header(TOKEN_HEADER, handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {"name": "view_image", "arguments": {"image_path": "file:///x.png"}}
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(
            response["error"]["message"].as_str(),
            Some("tool not available in current capability mode")
        );
    }

    #[test]
    fn resources_list_exposes_mounted_tools_manifest() {
        let handle = service(ImageCapabilities {
            native_view: true,
            native_generate: false,
            native_edit: false,
            view_fallback: false,
        });
        let tools_in_manifest = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            let list: Value = client
                .post(handle.url())
                .header(TOKEN_HEADER, handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id.clone())
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let resources = list["result"]["resources"].as_array().unwrap();
            assert_eq!(resources.len(), 1);
            let uri = resources[0]["uri"].as_str().unwrap();
            assert_eq!(uri, TOOLS_RESOURCE_URI);
            let read: Value = client
                .post(handle.url())
                .header(TOKEN_HEADER, handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id.clone())
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "resources/read",
                    "params": {"uri": uri}
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let text = read["result"]["contents"][0]["text"].as_str().unwrap();
            let parsed: Value = serde_json::from_str(text).unwrap();
            parsed
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        });
        // native_view=true trims view_image from the mounted set.
        assert!(!tools_in_manifest.contains(&"view_image".to_string()));
        assert!(tools_in_manifest.contains(&"generate_image".to_string()));
        assert!(tools_in_manifest.contains(&"edit_image".to_string()));
    }

    #[test]
    fn resources_read_rejects_unknown_uri() {
        let handle = service(ImageCapabilities::default());
        let error: Value = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            client
                .post(handle.url())
                .header(TOKEN_HEADER, handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "resources/read",
                    "params": {"uri": "kodex-image://missing"}
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        assert_eq!(error["error"]["code"], -32002);
    }

    #[test]
    fn update_capabilities_recomputes_tools_list() {
        let handle = service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        });
        let tools = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            // Simulate a model switch to a text-only BYOK model.
            handle.update_capabilities(ImageCapabilities {
                native_view: false,
                native_generate: false,
                native_edit: false,
                view_fallback: false,
            });
            list_tools(&client, &handle, &session_id).await
        });
        assert!(tools.contains(&"view_image".to_string()));
        assert!(tools.contains(&"generate_image".to_string()));
    }

    #[test]
    fn model_switch_tools_list_changes_both_directions() {
        // Mirrors `Application::reapply_image_capabilities`: a model switch
        // updates `ImageCapabilities` in place and a subsequent `tools/list`
        // recomputes the trimmed set without restarting the server.
        let handle = service(ImageCapabilities {
            native_view: false,
            native_generate: false,
            native_edit: false,
            view_fallback: false,
        });
        let (text_only, multimodal) = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize(&client, &handle).await;
            // Start: text-only BYOK -> view_image + generate_image + edit_image.
            let text_only = list_tools(&client, &handle, &session_id).await;
            // Switch to a multimodal model under the default provider:
            // native_view + native_generate become true.
            handle.update_capabilities(ImageCapabilities {
                native_view: true,
                native_generate: true,
                native_edit: false,
                view_fallback: false,
            });
            let multimodal = list_tools(&client, &handle, &session_id).await;
            // Switch back to text-only BYOK.
            handle.update_capabilities(ImageCapabilities {
                native_view: false,
                native_generate: false,
                native_edit: false,
                view_fallback: false,
            });
            let back = list_tools(&client, &handle, &session_id).await;
            assert!(back.contains(&"view_image".to_string()));
            assert!(back.contains(&"generate_image".to_string()));
            (text_only, multimodal)
        });
        assert!(text_only.contains(&"view_image".to_string()));
        assert!(text_only.contains(&"generate_image".to_string()));
        assert!(text_only.contains(&"edit_image".to_string()));
        // Multimodal + default provider: only edit_image remains offered.
        assert!(!multimodal.contains(&"view_image".to_string()));
        assert!(!multimodal.contains(&"generate_image".to_string()));
        assert!(multimodal.contains(&"edit_image".to_string()));
    }

    #[test]
    fn one_server_serves_two_sessions_with_their_own_tool_sets() {
        // The whole point of sharing the process: two sessions on ONE server,
        // each addressable only by its own token, each with its own trimmed
        // tool set (and its own config for tool calls).
        let handle = Arc::new(start_image_mcp_server().unwrap());
        let text_only_token = handle.register_session(
            ImageCapabilities {
                native_view: false,
                native_generate: false,
                native_edit: false,
                view_fallback: true,
            },
            config(),
        );
        let multimodal_token = handle.register_session(
            ImageCapabilities {
                native_view: true,
                native_generate: true,
                native_edit: false,
                view_fallback: true,
            },
            config(),
        );
        let url = handle.url().to_string();

        let (text_only, multimodal) = run_async(async {
            let client = reqwest::Client::new();
            let list = |token: String| {
                let client = client.clone();
                let url = url.clone();
                async move {
                    let session_id = client
                        .post(&url)
                        .header(TOKEN_HEADER, &token)
                        .json(&json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "initialize",
                            "params": {"protocolVersion": "2025-11-25", "capabilities": {}}
                        }))
                        .send()
                        .await
                        .unwrap()
                        .headers()
                        .get(MCP_SESSION_ID_HEADER)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string();
                    let response: Value = client
                        .post(&url)
                        .header(TOKEN_HEADER, &token)
                        .header(MCP_SESSION_ID_HEADER, &session_id)
                        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    response["result"]["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|tool| tool["name"].as_str().unwrap().to_string())
                        .collect::<Vec<_>>()
                }
            };
            (list(text_only_token.clone()).await, list(multimodal_token.clone()).await)
        });

        assert!(text_only.contains(&"view_image".to_string()));
        assert!(text_only.contains(&"generate_image".to_string()));
        // The multimodal session keeps its own trimming on the same server.
        assert!(!multimodal.contains(&"view_image".to_string()));
        assert!(!multimodal.contains(&"generate_image".to_string()));
        assert!(multimodal.contains(&"edit_image".to_string()));

        // A token that was never registered (or already unregistered) is refused.
        handle.unregister_session(&multimodal_token);
        let status = run_async(async {
            reqwest::Client::new()
                .post(&url)
                .header(TOKEN_HEADER, &multimodal_token)
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .send()
                .await
                .unwrap()
                .status()
        });
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // Keep an unused import warning suppressor for `Write` parity with the web
    // tools server tests; future image fetch tests will use it.
    #[allow(dead_code)]
    fn _write_suppressor(_w: &mut dyn Write) {}
}
