use super::remote_ssh::{is_remote_agent_ready_line, remote_agent_ready_from_line};
use super::streamable_http::{
    CODEBUDDY_RESOLVE_INTERRUPTION_METHOD, StreamableHttpState,
    codebuddy_connect_parts_from_payload, codebuddy_connection_not_found_response,
    codebuddy_session_load_request, message_has_method, post_streamable_http_line,
    send_streamable_http_request_with_retry, streamable_http_message_expects_response_body,
    streamable_http_session_id_for_message,
};
use super::*;
use std::path::Path;

#[test]
fn detects_remote_streamable_http_transport() {
    assert_eq!(
        detect_remote_acp_transport(&[
            "--acp".to_string(),
            "--acp-transport".to_string(),
            "streamable-http".to_string(),
        ]),
        RemoteAcpTransport::AcpStreamableHttp
    );
    assert_eq!(
        detect_remote_acp_transport(&["--acp-transport=streamable-http".to_string()]),
        RemoteAcpTransport::AcpStreamableHttp
    );
    assert_eq!(
        detect_remote_acp_transport(&["--serve".to_string()]),
        RemoteAcpTransport::CodeBuddyServeHttp
    );
    assert_eq!(
        detect_remote_acp_transport(&["--port".to_string(), "12345".to_string()]),
        RemoteAcpTransport::Tcp
    );
}

#[test]
fn streamable_http_endpoint_line_does_not_bypass_port_probe() {
    assert!(!is_remote_agent_ready_line(
        "ACP streamable-http endpoint: http://127.0.0.1:35499/api/v1/acp",
        RemoteAcpTransport::Tcp,
    ));
    let ready = remote_agent_ready_from_line(
        "ACP streamable-http endpoint: http://127.0.0.1:35499/api/v1/acp",
        RemoteAcpTransport::AcpStreamableHttp,
    )
    .expect("endpoint should report streamable-http readiness");
    assert_eq!(ready.endpoint_port, Some(35499));
}

#[test]
fn streamable_remote_command_does_not_wait_on_requested_port_probe() {
    let command = build_remote_streamable_agent_command(
        "/workspace/project",
        Path::new("/home/user/.kodex/remote-agents/codebuddy/current/bin/codebuddy"),
        &[
            "--acp".to_string(),
            "--acp-transport".to_string(),
            "streamable-http".to_string(),
        ],
        &[],
        4567,
    );
    assert!(command.contains("--port 4567"));
    assert!(!command.contains("/proc/net/tcp"));
    assert!(!command.contains(REMOTE_AGENT_READY_MARKER));
}

#[test]
fn remote_ssh_forward_args_use_discovered_remote_port() {
    let args = build_remote_ssh_forward_args("root@example.com", Some(2222), 3456, 45913, false);
    assert!(args.contains(&"-N".to_string()));
    assert!(args.contains(&"127.0.0.1:3456:127.0.0.1:45913".to_string()));
    assert!(args.contains(&"BatchMode=yes".to_string()));
    assert!(!args.contains(&"ControlMaster=auto".to_string()));
    assert!(!args.contains(&"ControlPersist=300".to_string()));
    assert!(args.contains(&"ControlMaster=no".to_string()));
    assert!(args.contains(&"ControlPath=none".to_string()));
    assert!(args.contains(&"ControlPersist=no".to_string()));
}

#[test]
fn readiness_error_includes_remote_agent_stderr() {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<RemoteAgentReady>();
    drop(ready_tx);
    let live_stderr = Arc::new(Mutex::new(
        "agent startup failed\nmissing remote provider configuration".to_string(),
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let error = runtime
        .block_on(wait_remote_agent_ready(ready_rx, live_stderr, None))
        .unwrap_err();
    let message = error.to_string();

    assert!(message.contains("ended before readiness was reported"));
    assert!(message.contains("missing remote provider configuration"));
}

#[test]
fn codebuddy_connect_payload_extracts_session_token() {
    let (connection_id, session_token) = codebuddy_connect_parts_from_payload(
        r#"{"data":{"connectionId":"conn-123","sessionToken":"token-456"}}"#,
    )
    .unwrap();

    assert_eq!(connection_id, "conn-123");
    assert_eq!(session_token.as_deref(), Some("token-456"));
}

#[test]
fn codebuddy_connect_payload_accepts_root_level_fields() {
    let (connection_id, session_token) = codebuddy_connect_parts_from_payload(
        r#"{"connection_id":"conn-abc","session_token":"token-def"}"#,
    )
    .unwrap();

    assert_eq!(connection_id, "conn-abc");
    assert_eq!(session_token.as_deref(), Some("token-def"));
}

#[test]
fn codebuddy_connection_not_found_response_is_detected() {
    assert!(codebuddy_connection_not_found_response(
        r#"{"jsonrpc":"2.0","error":{"message":"Connection not found. Please establish a connection first via POST /acp/connect before sending requests."}}"#,
    ));
    assert!(!codebuddy_connection_not_found_response(
        r#"{"jsonrpc":"2.0","error":{"message":"Another ACP client is already connected."}}"#,
    ));
}

#[test]
fn codebuddy_reconnect_session_load_request_uses_current_session() {
    let request = codebuddy_session_load_request("session-123", "/workspace/project");

    assert_eq!(request["jsonrpc"], "2.0");
    assert_eq!(request["method"], "session/load");
    assert_eq!(request["params"]["sessionId"], "session-123");
    assert_eq!(request["params"]["cwd"], "/workspace/project");
    assert_eq!(request["params"]["mcpServers"].as_array().unwrap().len(), 0);
}

#[test]
fn message_has_method_matches_batched_requests() {
    let request = json!([
        { "jsonrpc": "2.0", "id": 1, "method": "session/cancel" },
        { "jsonrpc": "2.0", "id": 2, "method": "initialize" }
    ]);

    assert!(message_has_method(&request, "initialize"));
    assert!(!message_has_method(&request, "session/load"));
}

#[test]
fn streamable_http_message_response_posts_are_ack_only() {
    let response = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": {
            "outcome": {
                "outcome": "selected",
                "optionId": "allow"
            }
        }
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 43,
        "method": "session/prompt",
        "params": {
            "sessionId": "session-123"
        }
    });
    let resolve_interruption = json!({
        "jsonrpc": "2.0",
        "id": 44,
        "method": CODEBUDDY_RESOLVE_INTERRUPTION_METHOD,
        "params": {
            "sessionId": "session-123",
            "toolCallId": "call_123",
            "interruptionId": "ir-call_123",
            "decision": "allow"
        }
    });
    let mixed_batch = json!([response.clone(), request.clone()]);

    assert!(!streamable_http_message_expects_response_body(&response));
    assert!(streamable_http_message_expects_response_body(&request));
    assert!(!streamable_http_message_expects_response_body(
        &resolve_interruption
    ));
    assert!(streamable_http_message_expects_response_body(&mixed_batch));
}

#[test]
fn streamable_http_session_id_falls_back_to_current_session_for_responses() {
    let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let state = StreamableHttpState {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint: "http://127.0.0.1:1/api/v1/acp".into(),
        incoming_tx,
        connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
        session_token: Arc::new(Mutex::new(Some("token-1".into()))),
        get_task: Arc::new(Mutex::new(None)),
        acp_transport: RemoteAcpTransport::AcpStreamableHttp,
        last_initialize: Arc::new(Mutex::new(None)),
        current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
        current_session_cwd: Arc::new(Mutex::new(None)),
        log_config: None,
    };
    let response = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": {
            "outcome": {
                "outcome": "selected",
                "optionId": "allow"
            }
        }
    });

    assert_eq!(
        streamable_http_session_id_for_message(&state, &response).as_deref(),
        Some("session-123")
    );
}

#[test]
fn streamable_http_ack_only_posts_do_not_wait_for_sse_body() {
    let permission_response = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": {
            "outcome": {
                "outcome": "selected",
                "optionId": "allow"
            }
        }
    });
    let resolve_interruption = json!({
        "jsonrpc": "2.0",
        "id": 44,
        "method": CODEBUDDY_RESOLVE_INTERRUPTION_METHOD,
        "params": {
            "sessionId": "session-123",
            "toolCallId": "call_123",
            "interruptionId": "ir-call_123",
            "decision": "allow"
        }
    });

    assert_ack_only_post_does_not_wait_for_sse_body(permission_response);
    assert_ack_only_post_does_not_wait_for_sse_body(resolve_interruption);
}

#[test]
fn streamable_http_request_posts_do_not_wait_for_sse_body() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server_thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_mock_http_request(&mut stream);
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("acp-session-id").as_deref(),
            Some("session-123")
        );

        let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert!(streamable_http_message_expects_response_body(&value));

        use std::io::Write;
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n: open\n\n",
                )
                .unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(600));
    });

    let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let state = StreamableHttpState {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
        incoming_tx,
        connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
        session_token: Arc::new(Mutex::new(Some("token-1".into()))),
        get_task: Arc::new(Mutex::new(None)),
        acp_transport: RemoteAcpTransport::AcpStreamableHttp,
        last_initialize: Arc::new(Mutex::new(None)),
        current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
        current_session_cwd: Arc::new(Mutex::new(None)),
        log_config: None,
    };
    let prompt = json!({
        "jsonrpc": "2.0",
        "id": 43,
        "method": "session/prompt",
        "params": {
            "sessionId": "session-123",
            "prompt": [{ "type": "text", "text": "hello" }]
        }
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap();

    runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_millis(200),
                post_streamable_http_line(&state, prompt.to_string()),
            )
            .await
        })
        .expect("request POST should hand off the SSE body to a background task")
        .unwrap();

    if let Ok(mut guard) = state.get_task.lock() {
        if let Some(task) = guard.take() {
            task.abort();
        }
    }
    server_thread.join().unwrap();
}

fn assert_ack_only_post_does_not_wait_for_sse_body(payload: serde_json::Value) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server_thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_mock_http_request(&mut stream);
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("acp-session-id").as_deref(),
            Some("session-123")
        );

        let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert!(!streamable_http_message_expects_response_body(&value));

        use std::io::Write;
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n: open\n\n",
                )
                .unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(600));
    });

    let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let state = StreamableHttpState {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
        incoming_tx,
        connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
        session_token: Arc::new(Mutex::new(Some("token-1".into()))),
        get_task: Arc::new(Mutex::new(None)),
        acp_transport: RemoteAcpTransport::AcpStreamableHttp,
        last_initialize: Arc::new(Mutex::new(None)),
        current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
        current_session_cwd: Arc::new(Mutex::new(None)),
        log_config: None,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap();

    runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_millis(200),
                post_streamable_http_line(&state, payload.to_string()),
            )
            .await
        })
        .expect("response POST should not wait for the SSE body")
        .unwrap();

    server_thread.join().unwrap();
}

#[test]
fn streamable_http_post_reconnects_and_retries_when_connection_is_missing() {
    let server = MockStreamableHttpServer::start();
    let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let state = StreamableHttpState {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint: server.endpoint.clone(),
        incoming_tx,
        connection_id: Arc::new(Mutex::new(Some("stale-connection".into()))),
        session_token: Arc::new(Mutex::new(Some("stale-token".into()))),
        get_task: Arc::new(Mutex::new(None)),
        acp_transport: RemoteAcpTransport::AcpStreamableHttp,
        last_initialize: Arc::new(Mutex::new(Some(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 1 }
        })))),
        current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
        current_session_cwd: Arc::new(Mutex::new(Some("/workspace/project".into()))),
        log_config: None,
    };
    let prompt = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/prompt",
        "params": {
            "sessionId": "session-123",
            "prompt": [{ "type": "text", "text": "hello" }]
        }
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap();

    let response = runtime
        .block_on(send_streamable_http_request_with_retry(&state, &prompt))
        .unwrap();
    assert!(response.status().is_success());
    let _ = runtime.block_on(response.text()).unwrap();
    if let Ok(mut guard) = state.get_task.lock() {
        if let Some(task) = guard.take() {
            task.abort();
        }
    }

    server.wait_until_complete();
    assert_eq!(server.connect_count(), 1);
    assert_eq!(server.initialize_count(), 1);
    assert_eq!(server.session_load_count(), 1);
    assert_eq!(server.retried_prompt_count(), 1);
    assert_eq!(
        state
            .connection_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .as_deref(),
        Some("fresh-connection")
    );
    assert_eq!(
        state
            .session_token
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .as_deref(),
        Some("fresh-token")
    );
}

struct MockStreamableHttpServer {
    endpoint: String,
    state: Arc<MockStreamableHttpState>,
}

#[derive(Default)]
struct MockStreamableHttpState {
    connect_count: std::sync::atomic::AtomicUsize,
    prompt_count: std::sync::atomic::AtomicUsize,
    initialize_count: std::sync::atomic::AtomicUsize,
    session_load_count: std::sync::atomic::AtomicUsize,
    retried_prompt_count: std::sync::atomic::AtomicUsize,
}

impl MockStreamableHttpServer {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(MockStreamableHttpState::default());
        let thread_state = state.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline
                && thread_state
                    .retried_prompt_count
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == 0
            {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        handle_mock_streamable_http_request(&mut stream, &thread_state)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
            state,
        }
    }

    fn wait_until_complete(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.retried_prompt_count() > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("mock streamable-http server did not receive retried prompt");
    }

    fn connect_count(&self) -> usize {
        self.state
            .connect_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn initialize_count(&self) -> usize {
        self.state
            .initialize_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn session_load_count(&self) -> usize {
        self.state
            .session_load_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn retried_prompt_count(&self) -> usize {
        self.state
            .retried_prompt_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn handle_mock_streamable_http_request(
    stream: &mut std::net::TcpStream,
    state: &MockStreamableHttpState,
) {
    let request = read_mock_http_request(stream);
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", path) if path.ends_with("/connect") => {
            state
                .connect_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_mock_json_response(
                stream,
                200,
                r#"{"connectionId":"fresh-connection","sessionToken":"fresh-token"}"#,
            );
        }
        ("GET", _) => {
            write_mock_response(stream, 200, "text/event-stream", ":ok\n\n");
        }
        ("POST", _) => {
            let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            let method = value
                .get("method")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            match method {
                "session/prompt" => {
                    let count = state
                        .prompt_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if count == 0 {
                        write_mock_json_response(
                            stream,
                            409,
                            r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Connection not found. Please establish a connection first via POST /acp/connect before sending requests."},"id":null}"#,
                        );
                    } else {
                        assert_eq!(
                            request.header("acp-connection-id").as_deref(),
                            Some("fresh-connection")
                        );
                        assert_eq!(request.header_count("acp-connection-id"), 1);
                        assert_eq!(
                            request.header("acp-session-token").as_deref(),
                            Some("fresh-token")
                        );
                        state
                            .retried_prompt_count
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        write_mock_json_response(
                            stream,
                            200,
                            r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
                        );
                    }
                }
                "initialize" => {
                    state
                        .initialize_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    write_mock_json_response(
                        stream,
                        200,
                        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                    );
                }
                "session/load" => {
                    assert_eq!(value["params"]["sessionId"], "session-123");
                    assert_eq!(value["params"]["cwd"], "/workspace/project");
                    state
                        .session_load_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    write_mock_json_response(
                        stream,
                        200,
                        r#"{"jsonrpc":"2.0","id":"kodex-codebuddy-reconnect-session-load","result":{}}"#,
                    );
                }
                other => panic!("unexpected mock ACP method {other}"),
            }
        }
        other => panic!("unexpected mock HTTP request {other:?}"),
    }
}

struct MockHttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl MockHttpRequest {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    fn header_count(&self, name: &str) -> usize {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .count()
    }
}

fn read_mock_http_request(stream: &mut std::net::TcpStream) -> MockHttpRequest {
    use std::io::Read;

    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "mock HTTP request ended before headers");
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap().to_string();
    let path = request_parts.next().unwrap().to_string();
    let headers = lines
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect::<Vec<_>>();
    let content_length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "mock HTTP request ended before body");
        buffer.extend_from_slice(&chunk[..count]);
    }
    let body =
        String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string();
    MockHttpRequest {
        method,
        path,
        headers,
        body,
    }
}

fn write_mock_json_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    write_mock_response(stream, status, "application/json", body);
}

fn write_mock_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) {
    use std::io::Write;

    let status_text = match status {
        200 => "OK",
        409 => "Conflict",
        _ => "Status",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
    stream.flush().unwrap();
}
