//! Unified `kodex-image` local MCP server.
//!
//! Mirrors the `web_tools_mcp` HTTP-MCP pattern: a `127.0.0.1` JSON-RPC server
//! bound to `/mcp`, authenticated with an `x-kodex-image-token` header. The
//! server exposes up to three tools — `view_image`, `generate_image`,
//! `edit_image` — but `tools/list` is dynamically trimmed to only the tools
//! whose native counterpart is missing for the current session
//! (`ImageCapabilities`). `tools/call` rejects any tool not in the current
//! trimmed set.
//!
//! `ImageCapabilities` is held behind a shared, lockable cell so that a model
//! switch can update the offered tool set without restarting the server
//! (subsequent `tools/list` calls recompute the trimmed set).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::http::StatusCode;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
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

/// Configuration carried by the image MCP service. Phase 5 wires the real
/// `ImageApi` (real multimodal understanding + OpenAI-compatible generation).
#[derive(Clone)]
pub struct ImageMcpConfig {
    pub workspace_root: PathBuf,
    pub settings: ImageSettings,
    pub view_api_key: Option<String>,
    pub generate_api_key: Option<String>,
}

/// Clonable service state shared across connections. `caps` is mutable so the
/// offered tool set can be updated on model switch.
#[derive(Clone)]
pub struct ImageMcpService {
    caps: Arc<Mutex<ImageCapabilities>>,
    config: Arc<ImageMcpConfig>,
    view_cache: Arc<Mutex<ViewCache>>,
}

impl ImageMcpService {
    pub fn new(caps: ImageCapabilities, config: ImageMcpConfig) -> Self {
        Self {
            caps: Arc::new(Mutex::new(caps)),
            config: Arc::new(config),
            view_cache: Arc::new(Mutex::new(ViewCache::default())),
        }
    }

    fn capabilities(&self) -> ImageCapabilities {
        self.caps
            .lock()
            .map(|guard| *guard)
            .unwrap_or_default()
    }

    fn update_capabilities(&self, caps: ImageCapabilities) {
        if let Ok(mut guard) = self.caps.lock() {
            *guard = caps;
        }
    }
}

pub struct ImageMcpHandle {
    url: String,
    token: String,
    service: ImageMcpService,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ImageMcpHandle {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Update the offered tool set without restarting the server. Subsequent
    /// `tools/list` calls reflect the new capabilities.
    pub fn update_capabilities(&self, caps: ImageCapabilities) {
        self.service.update_capabilities(caps);
    }

    /// Current resolved image capabilities for the session. Exposed for
    /// diagnostics / future UI surfacing; not consumed on the hot path today.
    #[allow(dead_code)]
    pub fn capabilities(&self) -> ImageCapabilities {
        self.service.capabilities()
    }

    /// The `ImageMcpConfig` carried by this handle, so prompt-level
    /// intercept can construct an `ImageApi` for automatic `view_image` calls.
    pub fn config(&self) -> ImageMcpConfig {
        (*self.service.config).clone()
    }

    /// Shared view cache so prompt-level intercept shares cached results with
    /// the MCP `view_image` tool.
    pub fn view_cache(&self) -> Arc<Mutex<ViewCache>> {
        self.service.view_cache.clone()
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

pub fn start_image_mcp_server(service: ImageMcpService) -> anyhow::Result<ImageMcpHandle> {
    let token = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
    let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel::<anyhow::Result<SocketAddr>>(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let thread_token = token.clone();
    let thread_session_id = session_id.clone();
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
            run_server(
                listener,
                thread_service,
                thread_token,
                thread_session_id,
                shutdown_rx,
            )
            .await;
        });
    });
    let addr = addr_rx.recv().map_err(|error| anyhow::anyhow!(error))??;
    Ok(ImageMcpHandle {
        url: format!("http://{addr}/mcp"),
        token,
        service,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn run_server(
    listener: TcpListener,
    service: ImageMcpService,
    token: String,
    session_id: String,
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
                let token = token.clone();
                let session_id = session_id.clone();
                tokio::task::spawn(async move {
                    let io = TokioIo::new(stream);
                    let _ = http1::Builder::new()
                        .serve_connection(io, service_fn(move |request| {
                            handle_http_request(
                                request,
                                service.clone(),
                                token.clone(),
                                session_id.clone(),
                            )
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
    token: String,
    session_id: String,
) -> Result<Response<BoxBody>, Infallible> {
    if request.method() != Method::POST || request.uri().path() != "/mcp" {
        return Ok(response(StatusCode::NOT_FOUND, "Not found"));
    }
    if !authorized(&request, &token) {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized"}),
        ));
    }
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
    if json_rpc_requires_session(&payload)
        && request_session_id.as_deref() != Some(session_id.as_str())
    {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "unauthorized: valid MCP session id is required"}),
        ));
    }
    let result = handle_json_rpc(payload, service).await;
    Ok(match result {
        JsonRpcHttpResult::Response(payload) => {
            json_response_with_session(StatusCode::OK, payload, Some(&session_id))
        }
        JsonRpcHttpResult::Accepted => {
            empty_response_with_session(StatusCode::ACCEPTED, Some(&session_id))
        }
    })
}

fn authorized(request: &Request<Incoming>, token: &str) -> bool {
    let header_token = request
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if header_token == Some(token) {
        return true;
    }
    request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {token}"))
}

enum JsonRpcHttpResult {
    Response(Value),
    Accepted,
}

async fn handle_json_rpc(payload: Value, service: ImageMcpService) -> JsonRpcHttpResult {
    if let Some(batch) = payload.as_array() {
        let mut responses = Vec::new();
        for item in batch {
            if let Some(response) = handle_json_rpc_call(item.clone(), service.clone()).await {
                responses.push(response);
            }
        }
        return if responses.is_empty() {
            JsonRpcHttpResult::Accepted
        } else {
            JsonRpcHttpResult::Response(Value::Array(responses))
        };
    }
    match handle_json_rpc_call(payload, service).await {
        Some(response) => JsonRpcHttpResult::Response(response),
        None => JsonRpcHttpResult::Accepted,
    }
}

async fn handle_json_rpc_call(payload: Value, service: ImageMcpService) -> Option<Value> {
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
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kodex-image", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => {
            let caps = service.capabilities();
            Ok(json!({"tools": trimmed_tool_schemas(&caps)}))
        }
        "tools/call" => {
            handle_tool_call(payload.get("params").cloned().unwrap_or_default(), service).await
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

async fn handle_tool_call(params: Value, service: ImageMcpService) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| json_rpc_error(-32602, "Missing tool name"))?;
    let caps = service.capabilities();
    if !offered_tools(&caps).contains(&name) {
        return Err(json_rpc_error(
            -32602,
            "tool not available in current capability mode",
        ));
    }
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let api = ImageApi::new((*service.config).clone(), service.view_cache.clone());
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

    fn service(caps: ImageCapabilities) -> ImageMcpService {
        ImageMcpService::new(
            caps,
            ImageMcpConfig {
                workspace_root: std::env::temp_dir(),
                settings: ImageSettings::default(),
                view_api_key: None,
                generate_api_key: None,
            },
        )
    }

    async fn initialize(client: &reqwest::Client, handle: &ImageMcpHandle) -> String {
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
        assert_eq!(payload["result"]["serverInfo"]["name"].as_str(), Some("kodex-image"));
        session_id
    }

    async fn list_tools(client: &reqwest::Client, handle: &ImageMcpHandle, session_id: &str) -> Vec<String> {
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
        let handle = start_image_mcp_server(service(ImageCapabilities {
            native_view: false,
            native_generate: false,
            native_edit: false,
            view_fallback: false,
        }))
        .unwrap();
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
        let handle = start_image_mcp_server(service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        }))
        .unwrap();
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
        let handle = start_image_mcp_server(service(ImageCapabilities::default())).unwrap();
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
        let handle = start_image_mcp_server(service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        }))
        .unwrap();
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
    fn update_capabilities_recomputes_tools_list() {
        let handle = start_image_mcp_server(service(ImageCapabilities {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        }))
        .unwrap();
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
        let handle = start_image_mcp_server(service(ImageCapabilities {
            native_view: false,
            native_generate: false,
            native_edit: false,
            view_fallback: false,
        }))
        .unwrap();
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

    // Keep an unused import warning suppressor for `Write` parity with the web
    // tools server tests; future image fetch tests will use it.
    #[allow(dead_code)]
    fn _write_suppressor(_w: &mut dyn Write) {}
}
