use crate::web_tools::{
    WebFetchRequest, WebSearchRequest, WebToolsConfig, WebToolsError, WebToolsService,
    error_to_json, response_to_value,
};
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
use std::sync::mpsc;
use std::thread;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

type BoxBody = Full<Bytes>;
const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";

pub struct WebToolsMcpHandle {
    url: String,
    token: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl WebToolsMcpHandle {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for WebToolsMcpHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start_web_tools_mcp_server(config: WebToolsConfig) -> anyhow::Result<WebToolsMcpHandle> {
    let service = WebToolsService::new(config)?;
    start_web_tools_mcp_server_with_service(service)
}

pub fn start_web_tools_mcp_server_with_service(
    service: WebToolsService,
) -> anyhow::Result<WebToolsMcpHandle> {
    let token = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
    let (addr_tx, addr_rx) = mpsc::sync_channel::<anyhow::Result<SocketAddr>>(1);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let thread_token = token.clone();
    let thread_session_id = session_id.clone();
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
                service,
                thread_token,
                thread_session_id,
                shutdown_rx,
            )
            .await;
        });
    });
    let addr = addr_rx.recv().map_err(|error| anyhow::anyhow!(error))??;
    Ok(WebToolsMcpHandle {
        url: format!("http://{addr}/mcp"),
        token,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn run_server(
    listener: TcpListener,
    service: WebToolsService,
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

async fn handle_http_request(
    request: Request<Incoming>,
    service: WebToolsService,
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
        .get("x-kodex-web-tools-token")
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

async fn handle_json_rpc(payload: Value, service: WebToolsService) -> JsonRpcHttpResult {
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

async fn handle_json_rpc_call(payload: Value, service: WebToolsService) -> Option<Value> {
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
            "serverInfo": {"name": "kodex-web-tools", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(json!({"tools": tool_schemas()})),
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

async fn handle_tool_call(params: Value, service: WebToolsService) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| json_rpc_error(-32602, "Missing tool name"))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "web_search" => {
            let request =
                serde_json::from_value::<WebSearchRequest>(arguments).map_err(|error| {
                    json_rpc_error(-32602, format!("Invalid web_search input: {error}"))
                })?;
            match service.search(request).await {
                Ok(response) => tool_success(response_to_value(&response).map_err(internal_error)?),
                Err(error) => Ok(tool_error(error)),
            }
        }
        "web_fetch" => {
            let request =
                serde_json::from_value::<WebFetchRequest>(arguments).map_err(|error| {
                    json_rpc_error(-32602, format!("Invalid web_fetch input: {error}"))
                })?;
            match service.fetch(request).await {
                Ok(response) => tool_success(response_to_value(&response).map_err(internal_error)?),
                Err(error) => Ok(tool_error(error)),
            }
        }
        _ => Err(json_rpc_error(-32602, format!("Unknown tool: {name}"))),
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

fn tool_error(error: WebToolsError) -> Value {
    let value = error_to_json(&error);
    json!({
        "content": [{"type": "text", "text": error.to_string()}],
        "structuredContent": value,
        "isError": true
    })
}

fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "web_search",
            "description": "Search the public web for current information. Returns bounded source results with titles, URLs, and snippets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "count": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Maximum number of results"}
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "web_fetch",
            "description": "Fetch public HTTP/HTTPS page content and return bounded extracted text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "Public HTTP or HTTPS URL"},
                    "max_length": {"type": "integer", "minimum": 1, "maximum": 50000},
                    "start_index": {"type": "integer", "minimum": 0}
                },
                "required": ["url"]
            }
        }),
    ]
}

fn internal_error(error: anyhow::Error) -> Value {
    json_rpc_error(-32603, error.to_string())
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn run_async<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn local_page_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let body = "<html><head><title>Doc</title></head><body>Hello web</body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    fn mcp_service() -> WebToolsService {
        let mut config = WebToolsConfig::brave("secret");
        config.allow_private_network = true;
        WebToolsService::new(config).unwrap()
    }

    async fn initialize_mcp_session(
        client: &reqwest::Client,
        handle: &WebToolsMcpHandle,
    ) -> String {
        let response = client
            .post(handle.url())
            .header("x-kodex-web-tools-token", handle.token())
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
            Some("kodex-web-tools")
        );
        session_id
    }

    #[test]
    fn mcp_streamable_http_handshake_accepts_initialized_notification() {
        let handle = start_web_tools_mcp_server_with_service(mcp_service()).unwrap();
        let status = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize_mcp_session(&client, &handle).await;
            client
                .post(handle.url())
                .header("x-kodex-web-tools-token", handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {}
                }))
                .send()
                .await
                .unwrap()
                .status()
        });

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[test]
    fn mcp_lists_tools_with_token() {
        let handle = start_web_tools_mcp_server_with_service(mcp_service()).unwrap();
        let response: Value = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize_mcp_session(&client, &handle).await;
            client
                .post(handle.url())
                .header("x-kodex-web-tools-token", handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id)
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });

        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "web_search"));
        assert!(tools.iter().any(|tool| tool["name"] == "web_fetch"));
    }

    #[test]
    fn mcp_rejects_missing_token() {
        let handle = start_web_tools_mcp_server_with_service(mcp_service()).unwrap();
        let response = run_async(async {
            reqwest::Client::new()
                .post(handle.url())
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .send()
                .await
                .unwrap()
        });

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn mcp_calls_web_fetch_tool() {
        let page_url = local_page_server();
        let handle = start_web_tools_mcp_server_with_service(mcp_service()).unwrap();
        let response: Value = run_async(async {
            let client = reqwest::Client::new();
            let session_id = initialize_mcp_session(&client, &handle).await;
            client
                .post(handle.url())
                .header("x-kodex-web-tools-token", handle.token())
                .header(MCP_SESSION_ID_HEADER, session_id)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "web_fetch",
                        "arguments": {"url": page_url}
                    }
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });

        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["title"].as_str(),
            Some("Doc")
        );
        assert!(
            response["result"]["structuredContent"]["content"]
                .as_str()
                .unwrap()
                .contains("Hello web")
        );
    }
}
