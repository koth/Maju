//! A fake dsh web host for integration tests.
//!
//! Speaks just enough of the harness RPC surface to exercise the bridge:
//! `POST /api/<method>` control responses and the `GET /api/events.mux` /
//! `GET /api/events.host` SSE streams. Frame delivery is scripted by the
//! test: the mux stream replays a configurable list of frames (each a
//! `ServerRequest` JSON), can drop the connection mid-way to exercise the
//! bridge's reconnection, and can fail a `session.history` call to exercise
//! per-session isolation.

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::ReadBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Wraps a `TcpStream` with a prefix of already-read bytes, so a WebSocket
/// handshake can re-read the HTTP upgrade request that `handle_connection`
/// already consumed from the socket.
struct PrefixedStream {
    prefix: Vec<u8>,
    prefix_pos: usize,
    inner: TcpStream,
}

impl tokio::io::AsyncRead for PrefixedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = unsafe { self.get_unchecked_mut() };
        if this.prefix_pos < this.prefix.len() {
            let remaining = &this.prefix[this.prefix_pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            this.prefix_pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(_cx, buf)
    }
}

impl tokio::io::AsyncWrite for PrefixedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = unsafe { self.get_unchecked_mut() };
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = unsafe { self.get_unchecked_mut() };
        Pin::new(&mut this.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = unsafe { self.get_unchecked_mut() };
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// What the mux stream does when the test script ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxEnd {
    /// Hold the connection open after the scripted frames (idle keep-alive).
    Hold,
    /// Close the connection after the scripted frames (triggers reconnect).
    Close,
}

/// When the mux stream emits its scripted frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HoldFramesUntil {
    /// Emit frames as soon as the WebSocket connection is up. Use for tests
    /// that register their sinks manually right after `acquire()` — the
    /// WebSocket handshake always loses that race.
    #[default]
    Connected,
    /// Hold frames until the mock has served a `session.models` POST.
    /// `run_harness_session` registers the session sink synchronously right
    /// after `session.create` returns and strictly BEFORE it POSTs
    /// `session.models`, so serving that call guarantees the sink is
    /// registered. Without the hold, an up-front scripted frame races the
    /// registration and is dropped as "mux frame for unregistered session"
    /// (which flaked the question/answer tests under parallel load).
    SessionRegistered,
}

/// Behavior for a single mux stream connection (a reconnection re-runs the
/// script from its own frame list).
#[derive(Debug, Clone)]
pub struct MuxScript {
    /// Frames to emit as `data: <json>\n\n` on this connection.
    pub frames: Vec<Value>,
    /// What to do after `frames` are emitted.
    pub end: MuxEnd,
    /// When to emit the scripted frames relative to the bridge's session
    /// registration.
    pub hold_frames_until: HoldFramesUntil,
}

#[derive(Debug, Clone)]
pub struct MockHarnessConfig {
    /// Script for the first mux connection. Reconnections use
    /// `reconnect_scripts` when non-empty, else `mux` again.
    pub mux: Vec<MuxScript>,
    /// Frames for the per-session `session/follow` WS. Each new follow
    /// connection drains one script (same lifecycle as `mux`).
    pub follow: Vec<MuxScript>,
    /// Frames for the host-wide `session/control` WS. Each new control
    /// connection drains one script (same lifecycle as `mux`). These carry the
    /// live projection updates (`contextPressure`, `tokenUsage`).
    pub control: Vec<MuxScript>,
    /// Per-session history failure: session ids whose `session.history` call
    /// should return an error (to exercise per-session re-baseline isolation).
    pub history_failures: Vec<String>,
    /// Events returned by `session.history` (each a `HistoryEntry` JSON
    /// `{ event: { type, seq, time, data }, view? }`).
    pub history_events: Vec<Value>,
    /// When true, `agentPreset.select` answers with the `agent-preset-locked`
    /// business error (as dsh does for a session that has already started).
    pub preset_locked: bool,
    /// When true, the answer carrier rejects with `bad-response` (as dsh does
    /// for a malformed/mismatched answer payload).
    pub respond_reject: bool,
    /// When true, the mock speaks the dsh ≤ 0.1.4 *answer protocol*: the
    /// `$events` ready frame is sent top-level (no `item` envelope) and
    /// `POST /api/$events/result` takes a bare `{ args }` body whose question
    /// value is the wrapped `{ sessionId, answer }`. The default emulates dsh
    /// ≥ 0.1.5: an `item`-wrapped ready frame plus the shared Connection RPC
    /// `client-request` envelope, resolving with the bare answer value.
    pub legacy_answer_protocol: bool,
    /// Overrides the `$events` ready-frame wire form independently of the
    /// answer carrier, to emulate a host whose stream form and answer carrier
    /// disagree — the case the bridge's one-shot protocol retry exists for.
    /// `None` follows `legacy_answer_protocol`.
    pub ready_frame_item_envelope: Option<bool>,
    /// When true, token-authenticated endpoints reject requests without the
    /// `dsh-auth-` cookie.
    pub require_auth: bool,
    /// Projection values returned by `session/page` under its top-level
    /// `projections.values` object (dsh supplies `modelSelection` here).
    pub history_projections: Option<Value>,
    /// Wire name the mock's `commands/execute` descriptor declares for the
    /// attachment parameter. dsh ≤ 0.1.4 declares `images`, dsh ≥ 0.1.5
    /// declares `submittedAttachments` (the rename that broke `/compact`);
    /// any other name in the args is rejected as `arguments-invalid`.
    pub command_attachment_field: String,
    /// When set, every `commands/execute` answers with this
    /// `gateway/arguments-invalid` message instead of validating the args —
    /// used to check that unrelated args rejections are not retried.
    pub commands_execute_error: Option<String>,
}
#[derive(Debug, Default)]
struct MockState {
    /// Methods received (POST /api/<method>) with their rpcId, in order.
    pub calls: Vec<(String, String)>,
    /// `session.create` payloads received, in order.
    pub creates: Vec<Value>,
    /// `session.fork` request objects received, in order — the descriptor's
    /// `request` parameter, unwrapped from its `{ args: { request: … } }`
    /// envelope.
    pub forks: Vec<Value>,
    /// `session.cancel` args objects received, in order (rejected attempts
    /// included, so tests can pin the `request` envelope the descriptor wants).
    pub cancel_args: Vec<Value>,
    /// `respond` / `$events/result` payloads received (approval/question
    /// answers). For `$events/result` this is the args object the bridge built,
    /// with the carrier envelope stripped.
    pub responds: Vec<Value>,
    /// Whether each `$events/result` request arrived as a Connection RPC
    /// `client-request` envelope (dsh ≥ 0.1.5) rather than a bare `{ args }`
    /// body, in order.
    pub answer_envelopes: Vec<bool>,
    /// `agentPreset.select` preset ids received, in order.
    pub preset_selects: Vec<String>,
    /// `commands/execute` args objects received, in order (including rejected
    /// attempts, so tests can observe the attachment-field fallback).
    pub commands_execute_args: Vec<Value>,
    /// Pending question frames keyed by envelope rpcId (the questions the host
    /// is waiting on). Populated when a `question/requested` frame is sent.
    pub pending_questions: std::collections::HashMap<String, Vec<Value>>,
    /// Pending approval waterfalls keyed by envelope rpcId, so the answer
    /// handler can validate the closed outcome the way dsh does.
    pub pending_approvals: std::collections::HashMap<String, Value>,
    /// Set once a `session.models` POST has been served. `run_harness_session`
    /// only sends it after the session sink is registered, so tests can use it
    /// as a registration barrier (`HoldFramesUntil::SessionRegistered`).
    pub models_served: bool,
}

pub struct MockHarness {
    pub addr: SocketAddr,
    config: Arc<Mutex<MockHarnessConfig>>,
    state: Arc<Mutex<MockState>>,
    /// Notifies a waiting test that the mux stream reached the end of its
    /// scripted frames (used to observe a drop).
    mux_dropped: Mutex<Option<mpsc::Receiver<()>>>,
    /// Tracks how many mux connections have been served.
    mux_conns: Arc<std::sync::atomic::AtomicUsize>,
}

impl MockHarness {
    /// Start the mock host on a random loopback port.
    pub async fn start(config: MockHarnessConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let config = Arc::new(Mutex::new(config));
        let state = Arc::new(Mutex::new(MockState::default()));
        let (drop_tx, drop_rx) = mpsc::channel(1);
        let mux_conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let launch_token = if config.lock().unwrap().require_auth {
            Some("mock-launch-token".to_string())
        } else {
            None
        };

        let config_handle = config.clone();
        let state_handle = state.clone();
        let mux_conns_handle = mux_conns.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let config = config_handle.clone();
                let state = state_handle.clone();
                let drop_tx = drop_tx.clone();
                let mux_conns = mux_conns_handle.clone();
                let launch_token = launch_token.clone();
                tokio::spawn(async move {
                    handle_connection(
                        stream,
                        config,
                        state,
                        drop_tx,
                        mux_conns,
                        launch_token.as_deref(),
                    )
                    .await;
                });
            }
        });

        Self {
            addr,
            config,
            state,
            mux_dropped: Mutex::new(Some(drop_rx)),
            mux_conns,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn calls(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Payloads of the `session.create` calls received, in order.
    pub fn creates(&self) -> Vec<Value> {
        self.state.lock().unwrap().creates.clone()
    }

    /// Payloads of the `session.fork` calls received, in order.
    pub fn forks(&self) -> Vec<Value> {
        self.state.lock().unwrap().forks.clone()
    }

    /// Args objects of the `session.cancel` calls received, in order.
    pub fn cancel_args(&self) -> Vec<Value> {
        self.state.lock().unwrap().cancel_args.clone()
    }

    pub fn responds(&self) -> Vec<Value> {
        self.state.lock().unwrap().responds.clone()
    }

    /// Whether each `$events/result` request arrived in the Connection RPC
    /// `client-request` envelope, in order.
    pub fn answer_envelopes(&self) -> Vec<bool> {
        self.state.lock().unwrap().answer_envelopes.clone()
    }

    /// `agentPreset.select` preset ids received, in order.
    pub fn preset_selects(&self) -> Vec<String> {
        self.state.lock().unwrap().preset_selects.clone()
    }

    /// `commands/execute` args objects received, in order (rejected attempts
    /// included).
    pub fn commands_execute_args(&self) -> Vec<Value> {
        self.state.lock().unwrap().commands_execute_args.clone()
    }

    pub fn mux_connection_count(&self) -> usize {
        self.mux_conns.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Wait until the mux stream has emitted its scripted frames (and, for a
    /// `Close` script, dropped). Bounded by a timeout.
    pub async fn wait_for_mux_drop(&self) {
        let mut guard = self.mux_dropped.lock().unwrap();
        if let Some(rx) = guard.as_mut() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        }
    }

    /// Replace the connection script (e.g. a reconnection script with a
    /// different frame set).
    pub fn set_config(&self, config: MockHarnessConfig) {
        *self.config.lock().unwrap() = config;
    }

    /// Append scripts for subsequent mux connections.
    pub fn append_mux_scripts(&self, scripts: Vec<MuxScript>) {
        let mut guard = self.config.lock().unwrap();
        guard.mux.extend(scripts);
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    config: Arc<Mutex<MockHarnessConfig>>,
    state: Arc<Mutex<MockState>>,
    drop_tx: mpsc::Sender<()>,
    mux_conns: Arc<std::sync::atomic::AtomicUsize>,
    launch_token: Option<&str>,
) {
    let mut buf = Vec::new();
    // Read the request head (until \r\n\r\n).
    let mut tmp = [0u8; 4096];
    let head_end;
    loop {
        let n = stream.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
            head_end = idx + 4;
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut cookies = String::new();
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("cookie:") {
            cookies = value.trim().to_string();
        } else if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        }
    }
    if let Some(token) = launch_token {
        if method == "GET" && path.starts_with('/') {
            let (root_path, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
            if root_path == "/" {
                let token_matches = query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .any(|(name, value)| name == "token" && value == token);
                if token_matches {
                    let cookie = format!(
                        "HTTP/1.1 303 See Other\r\nlocation: /\r\nset-cookie: dsh-auth-test={token}; Path=/; HttpOnly\r\ncontent-length: 0\r\n\r\n"
                    );
                    stream.write_all(cookie.as_bytes()).await.unwrap();
                    return;
                }
                let response = b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n";
                stream.write_all(response).await.unwrap();
                return;
            }
        }
        if !cookies
            .split(';')
            .any(|pair| pair.trim().starts_with("dsh-auth-test="))
        {
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
            return;
        }
    }

    while buf.len() < head_end + content_length {
        let n = stream.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = if buf.len() >= head_end + content_length {
        buf[head_end..head_end + content_length].to_vec()
    } else {
        Vec::new()
    };

    match (method.as_str(), path.as_str()) {
        ("GET", "/api/remote.mux") => {
            // The mux path carries every logical stream: the `$events` event
            // mux, each session's `session/follow` journal, and the host-wide
            // `session/control` channel. `serve_mux` reads the opening frame to
            // learn which one this is, so nothing may be consumed from the
            // scripted `mux` list until then — a control connection that stole
            // a script would silently break the event-mux tests.
            serve_mux(
                stream,
                buf[..head_end].to_vec(),
                drop_tx,
                state.clone(),
                config.clone(),
                mux_conns.clone(),
            )
            .await;
        }
        ("GET", "/api/events.host") => {
            // Host stream: WebSocket keep-alive with no frames (idle).
            let prefixed = PrefixedStream {
                prefix: buf[..head_end].to_vec(),
                prefix_pos: 0,
                inner: stream,
            };
            if let Ok(mut ws) = tokio_tungstenite::accept_async(prefixed).await {
                while ws.next().await.is_some() {}
            }
        }
        ("POST", "/api/respond") => {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            state.lock().unwrap().responds.push(parsed.clone());
            // Real validation, mirroring dsh's respond() + matchesQuestions:
            // the answer must reference a pending approval/question id and
            // satisfy count/id/label constraints, else `bad-response`.
            let receipt = {
                let state_guard = state.lock().unwrap();
                let rpc_id = parsed.get("rpcId").and_then(Value::as_str).unwrap_or("");
                let reject = config.lock().unwrap().respond_reject;
                if reject {
                    serde_json::json!({ "accepted": false, "reason": "bad-response" })
                } else if let Some(questions) = state_guard.pending_questions.get(rpc_id) {
                    let value = parsed.pointer("/result/value");
                    let answers = value
                        .and_then(|v| v.pointer("/answer/answers"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let ok = answers.len() == questions.len()
                        && answers.iter().zip(questions.iter()).all(|(a, q)| {
                            let qid = q.get("id").and_then(Value::as_str).unwrap_or("");
                            let aid = a.get("id").and_then(Value::as_str).unwrap_or("");
                            if aid != qid {
                                return false;
                            }
                            let selected: Vec<&str> = a
                                .get("selected")
                                .and_then(Value::as_array)
                                .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                                .unwrap_or_default();
                            let labels: Vec<&str> = q
                                .get("options")
                                .and_then(Value::as_array)
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|o| o.get("label").and_then(Value::as_str))
                                        .collect()
                                })
                                .unwrap_or_default();
                            selected.iter().all(|s| labels.contains(s))
                        });
                    if ok {
                        serde_json::json!({ "accepted": true })
                    } else {
                        serde_json::json!({ "accepted": false, "reason": "bad-response" })
                    }
                } else {
                    serde_json::json!({ "accepted": false, "reason": "not-pending" })
                }
            };
            let body = serde_json::to_vec(&receipt).unwrap();
            write_response(&mut stream, 200, "application/json", &body).await;
        }
        ("POST", "/api/$events/result") => {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let legacy = config.lock().unwrap().legacy_answer_protocol;
            // dsh 0.1.5 serves this endpoint through the shared Connection RPC
            // interceptor: the body MUST be a `client-request` envelope. A bare
            // `{ args }` body is refused before the answer is ever looked at —
            // which is what reached the user as an opaque "gateway error".
            let enveloped = parsed.get("type").and_then(Value::as_str) == Some("client-request")
                && parsed.get("method").and_then(Value::as_str) == Some("$events/result")
                && parsed.pointer("/payload/args").is_some();
            let respond_legacy_shape = |ok: bool, reason: &str| {
                if ok {
                    serde_json::json!({ "ok": true, "value": null })
                } else {
                    serde_json::json!({ "ok": false, "error": { "message": reason } })
                }
            };
            let rpc_id = parsed
                .get("rpcId")
                .and_then(Value::as_str)
                .unwrap_or("invalid-request")
                .to_string();
            let respond_envelope = |ok: bool, reason: &str| {
                if ok {
                    serde_json::json!({
                        "type": "server-response",
                        "rpcId": rpc_id,
                        "result": { "ok": true }
                    })
                } else {
                    serde_json::json!({
                        "type": "server-response",
                        "rpcId": rpc_id,
                        "result": {
                            "ok": false,
                            "error": {
                                "code": "gateway/bad-response",
                                "message": reason,
                                "details": {}
                            }
                        }
                    })
                }
            };
            let args = if enveloped {
                parsed
                    .pointer("/payload/args")
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                parsed.get("args").cloned().unwrap_or(Value::Null)
            };
            // Record every attempt (carrier + args) so tests can assert both the
            // envelope and a protocol-retry sequence.
            {
                let mut guard = state.lock().unwrap();
                guard.answer_envelopes.push(enveloped);
                guard.responds.push(args.clone());
            }
            if !legacy && !enveloped {
                // Mirror the Connection interceptor's envelope refusal.
                let response = serde_json::json!({
                    "type": "server-response",
                    "rpcId": rpc_id,
                    "result": {
                        "ok": false,
                        "error": {
                            "code": "gateway/bad-request",
                            "message": "invalid client-request message",
                            "details": { "issues": [] }
                        }
                    }
                });
                let body = serde_json::to_vec(&response).unwrap();
                write_response(&mut stream, 200, "application/json", &body).await;
                return;
            }
            if legacy && enveloped {
                // Mirror the pre-0.1.5 handler, which requires the body to be
                // exactly `{ args }`.
                let response = serde_json::json!({
                    "ok": false,
                    "error": {
                        "code": "gateway/internal",
                        "message": "typert gateway: Remote event result requires exactly one plain-object args field"
                    }
                });
                let body = serde_json::to_vec(&response).unwrap();
                write_response(&mut stream, 200, "application/json", &body).await;
                return;
            }
            // Real validation, mirroring dsh's receiveRemoteEventResult and the
            // answer schema: the result must reference an active `$events`
            // client id, a pending waterfall eventId, and carry the value shape
            // that protocol resolves waterfalls with.
            let (ok, reason) = {
                let state_guard = state.lock().unwrap();
                let client_id = args.get("clientId").and_then(Value::as_str).unwrap_or("");
                let event_id = args.get("eventId").and_then(Value::as_str).unwrap_or("");
                let reject = config.lock().unwrap().respond_reject;
                let value = args.pointer("/outcome/value");
                if client_id != "$events-client-1" {
                    (false, "identifies no active event stream".to_string())
                } else if reject {
                    (false, "bad-response".to_string())
                } else if let Some(questions) = state_guard.pending_questions.get(event_id) {
                    // A rejected outcome is how a cancel resolves a question
                    // waterfall: the gateway's parseRemoteEventResult accepts
                    // `{ kind: "rejected", error: { name, message } }`, and dsh
                    // turns it into the tool's own abort error. It carries no
                    // value, so it must short-circuit the answer schema check.
                    let outcome_kind = args
                        .pointer("/outcome/kind")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if outcome_kind == "rejected" {
                        let name = args
                            .pointer("/outcome/error/name")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let message = args
                            .pointer("/outcome/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if name.is_empty() || message.is_empty() {
                            (false, "invalid Remote event rejection".to_string())
                        } else {
                            (true, String::new())
                        }
                    } else {
                        // 0.1.5 resolves with the bare answer; ≤ 0.1.4 wrapped it.
                        let answers = value
                            .and_then(|v| v.pointer("/answer/answers").or_else(|| v.get("answers")))
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let matches = answers.len() == questions.len()
                            && answers.iter().zip(questions.iter()).all(|(a, q)| {
                                let qid = q.get("id").and_then(Value::as_str).unwrap_or("");
                                let aid = a.get("id").and_then(Value::as_str).unwrap_or("");
                                if aid != qid {
                                    return false;
                                }
                                let selected: Vec<&str> = a
                                    .get("selected")
                                    .and_then(Value::as_array)
                                    .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                                    .unwrap_or_default();
                                let labels: Vec<&str> = q
                                    .get("options")
                                    .and_then(Value::as_array)
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|o| o.get("label").and_then(Value::as_str))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                selected.iter().all(|s| labels.contains(s))
                            });
                        if matches {
                            (true, String::new())
                        } else {
                            (false, "bad-response".to_string())
                        }
                    }
                } else if state_guard.pending_approvals.contains_key(event_id) {
                    // An approval waterfall resolves with the bare closed
                    // outcome string.
                    match value.and_then(Value::as_str) {
                        Some("allowed-once") | Some("rejected") => (true, String::new()),
                        _ => (false, "bad-response".to_string()),
                    }
                } else {
                    (false, "not-pending".to_string())
                }
            };
            let response = if legacy {
                respond_legacy_shape(ok, &reason)
            } else {
                respond_envelope(ok, &reason)
            };
            let body = serde_json::to_vec(&response).unwrap();
            write_response(&mut stream, 200, "application/json", &body).await;
        }
        ("POST", path) => {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let rpc_id = parsed
                .get("rpcId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let method_name = path.strip_prefix("/api/").unwrap_or("").to_string();
            let session_id = parsed
                .pointer("/payload/args/request/sessionId")
                .and_then(Value::as_str)
                .or_else(|| {
                    parsed
                        .pointer("/payload/args/request/address/sessionId")
                        .and_then(Value::as_str)
                })
                .unwrap_or("s-mock")
                .to_string();
            state
                .lock()
                .unwrap()
                .calls
                .push((method_name.clone(), rpc_id.clone()));
            if method_name == "session/create" {
                let payload = parsed
                    .pointer("/payload/args/request")
                    .cloned()
                    .unwrap_or(Value::Null);
                state.lock().unwrap().creates.push(payload);
            }
            if method_name == "session/fork" {
                let payload = parsed
                    .pointer("/payload/args/request")
                    .cloned()
                    .unwrap_or(Value::Null);
                state.lock().unwrap().forks.push(payload);
            }
            if method_name == "session/modelCatalog" {
                // Sent by `run_harness_session` only after the session sink is
                // registered — used as the frame-hold barrier.
                state.lock().unwrap().models_served = true;
            }
            if method_name == "session/cancel" {
                state.lock().unwrap().cancel_args.push(
                    parsed
                        .pointer("/payload/args")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }

            // Mirror the gateway's `assertExactArguments` for the endpoints the
            // bridge calls. Shipping a request object bare (e.g. `{ sessionId }`
            // for `session/cancel`) is rejected exactly like this — that is how
            // the stop button used to fail while reporting nothing.
            let descriptor_wires: Option<&[&str]> = match method_name.as_str() {
                "session/cancel" | "session/fork" => Some(&["request"]),
                "agentPresets/select" => Some(&["agentId", "agentPreset"]),
                _ => None,
            };
            if let Some(expected) = descriptor_wires {
                let args = parsed
                    .pointer("/payload/args")
                    .cloned()
                    .unwrap_or(Value::Null);
                if let Some(detail) = exact_args_rejection(&args, expected) {
                    let response = gateway_args_invalid_response(
                        &rpc_id,
                        &format!(
                            "typert gateway: {method_name}: args fields do not match the descriptor: {detail}"
                        ),
                    );
                    let body = serde_json::to_vec(&response).unwrap();
                    write_response(&mut stream, 200, "application/json", &body).await;
                    return;
                }
            }

            let value = match method_name.as_str() {
                "host.describe" => serde_json::json!({
                    "version": "0.1.0-test",
                    "cwd": "/tmp",
                    "attachedSessions": 0,
                    "canOpenPath": false,
                }),
                "session.list" => serde_json::json!({ "items": [] }),
                "session/create" => serde_json::json!({ "sessionId": "s-1" }),
                "session/fork" => serde_json::json!({ "sessionId": "s-fork" }),
                "session/prompt" => serde_json::json!({ "accepted": true }),
                "session/cancel" => serde_json::json!({ "accepted": true }),
                "session/page" => {
                    let fail = config
                        .lock()
                        .unwrap()
                        .history_failures
                        .iter()
                        .any(|id| *id == session_id);
                    if fail {
                        let response = serde_json::json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {
                                "ok": false,
                                "error": { "code": "internal", "message": "history failure", "details": {} }
                            }
                        });
                        let body = serde_json::to_vec(&response).unwrap();
                        write_response(&mut stream, 200, "application/json", &body).await;
                        return;
                    }
                    let events = config.lock().unwrap().history_events.clone();
                    let projections = config.lock().unwrap().history_projections.clone();
                    let mut response = serde_json::json!({ "records": events, "hasMore": false });
                    if let Some(projections) = projections {
                        response["projections"] = projections;
                    }
                    response
                }
                "session/modelCatalog" => serde_json::json!({
                    "default": { "provider": "deepseek", "model": "deepseek-v4-pro" },
                    "routableProviders": ["deepseek"],
                    "groups": [
                        {
                            "id": "deepseek",
                            "name": "DeepSeek",
                            "models": [
                                { "id": "deepseek-v4-pro", "name": "DeepSeek V4 Pro" },
                                { "id": "deepseek-v4-flash", "name": "DeepSeek V4 Flash" }
                            ]
                        }
                    ],
                    "failures": [],
                }),
                "session/selectModel" => serde_json::json!({
                    "selected": { "provider": "deepseek", "model": "deepseek-v4-pro" }
                }),
                "agentPresets/list" => serde_json::json!({
                    "presets": [
                        { "id": "code", "trust": "system", "isDefault": true, "name": "Code", "description": "Standard coding agent" },
                        { "id": "standard", "trust": "system", "isDefault": false, "name": "Standard", "description": "Full coding agent" },
                        { "id": "minimal", "trust": "system", "isDefault": false, "name": "Minimal", "description": "Fixed-prompt composition" },
                        { "id": "cordis", "trust": "system", "isDefault": false, "name": "Cordis", "description": "Runtime read/write" }
                    ],
                    "authorable": false,
                    "hasDocument": false,
                }),
                "commands/execute" => {
                    // Mirror the typert gateway's args validation: the args
                    // object must carry exactly the attachment field the
                    // descriptor declares. Everything else is rejected with
                    // `gateway/arguments-invalid`, the shape dsh 0.1.5.1-rc.1
                    // returns for the pre-rename `images` payload.
                    let args = parsed
                        .pointer("/payload/args")
                        .cloned()
                        .unwrap_or(Value::Null);
                    state
                        .lock()
                        .unwrap()
                        .commands_execute_args
                        .push(args.clone());
                    let expected = config.lock().unwrap().command_attachment_field.clone();
                    let forced_error = config.lock().unwrap().commands_execute_error.clone();
                    let rejection = match forced_error {
                        Some(message) => Some((expected.clone(), message, true)),
                        None if args.get(&expected).is_none() => {
                            let unexpected = ["submittedAttachments", "images"]
                                .into_iter()
                                .find(|field| args.get(*field).is_some())
                                .unwrap_or("<none>");
                            Some((
                                expected.clone(),
                                format!(
                                    "type=t gateway: commands/execute: args fields do not match the descriptor: missing \"{expected}\"; unexpected \"{unexpected}\""
                                ),
                                false,
                            ))
                        }
                        None => None,
                    };
                    if let Some((_, message, forced)) = rejection {
                        let message = if forced {
                            // A rejection unrelated to the attachment field.
                            format!("type=t gateway: commands/execute: {message}")
                        } else {
                            message
                        };
                        let response = serde_json::json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {
                                "ok": false,
                                "error": {
                                    "code": "gateway/arguments-invalid",
                                    "message": message,
                                    "details": {}
                                }
                            }
                        });
                        let body = serde_json::to_vec(&response).unwrap();
                        write_response(&mut stream, 200, "application/json", &body).await;
                        return;
                    }
                    serde_json::json!({
                        "commandId": "cmd-compact-1",
                        "result": {
                            "kind": "success",
                            "text": "Compacted 3 history items (~1.2k tokens).",
                            "sourceEventSeq": 42
                        }
                    })
                }
                "agentPresets/select" => {
                    let preset = parsed
                        .pointer("/payload/args/agentPreset")
                        .and_then(Value::as_str)
                        .unwrap_or("code")
                        .to_string();
                    if config.lock().unwrap().preset_locked {
                        let response = serde_json::json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {
                                "ok": false,
                                "error": {
                                    "code": "agent-preset-locked",
                                    "message": format!("session has already started; its agent preset is fixed"),
                                    "details": { "sessionId": session_id, "agentPreset": preset }
                                }
                            }
                        });
                        let body = serde_json::to_vec(&response).unwrap();
                        write_response(&mut stream, 200, "application/json", &body).await;
                        return;
                    }
                    state.lock().unwrap().preset_selects.push(preset.clone());
                    serde_json::json!({ "agentPreset": preset })
                }
                _ => serde_json::json!({}),
            };
            let response = serde_json::json!({
                "type": "server-response",
                "rpcId": rpc_id,
                "result": { "ok": true, "value": value }
            });
            let body = serde_json::to_vec(&response).unwrap();
            write_response(&mut stream, 200, "application/json", &body).await;
        }
        ("GET", _) => {
            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
        _ => {
            let _ = stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    }
}

async fn serve_mux(
    stream: TcpStream,
    head: Vec<u8>,
    drop_tx: mpsc::Sender<()>,
    state: Arc<Mutex<MockState>>,
    config: Arc<Mutex<MockHarnessConfig>>,
    mux_conns: Arc<std::sync::atomic::AtomicUsize>,
) {
    let prefixed = PrefixedStream {
        prefix: head,
        prefix_pos: 0,
        inner: stream,
    };
    let mut ws = match tokio_tungstenite::accept_async(prefixed).await {
        Ok(ws) => ws,
        Err(_) => return,
    };
    // The first client frame identifies the logical stream: `{ type: "open",
    // endpoint: "$events" | "session/follow", … }`. Route accordingly.
    let first = match ws.next().await {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => text,
        _ => return,
    };
    let open: Value = match serde_json::from_str(&first) {
        Ok(value) => value,
        Err(_) => return,
    };
    let endpoint = open
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if endpoint == "session/follow" {
        // Journal frames carry the session id in their payload; the bridge
        // demuxes by stream, not by frame content.
        let follow_script = {
            let mut guard = config.lock().unwrap();
            if guard.follow.is_empty() {
                guard.follow = scripts_for_hold();
            }
            guard.follow.drain(..1).next().unwrap()
        };
        serve_logical_stream(ws, follow_script, "mock-follow", drop_tx, state).await;
        return;
    }
    if endpoint == "session/control" {
        // Host-wide control stream: queues, jobs, and the live projection
        // updates the usage dock depends on.
        let control_script = {
            let mut guard = config.lock().unwrap();
            if guard.control.is_empty() {
                guard.control = scripts_for_hold();
            }
            guard.control.drain(..1).next().unwrap()
        };
        serve_logical_stream(ws, control_script, "mock-control", drop_tx, state).await;
        return;
    }
    // `$events` mux: only this logical stream is scripted by `config.mux` and
    // counted by `mux_connection_count` (the follow/control streams above have
    // their own scripts and would otherwise steal a mux script).
    let script = {
        let mut guard = config.lock().unwrap();
        // Take the next script; keep the remainder for reconnections.
        let next = if guard.mux.is_empty() {
            None
        } else {
            guard.mux.drain(..1).next()
        };
        if next.is_none() {
            // No scripts configured or none remaining: use the steady-state
            // idle keep-alive.
            guard.mux = scripts_for_hold();
        }
        next.unwrap_or_else(|| scripts_for_hold().remove(0))
    };
    mux_conns.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    if script.hold_frames_until == HoldFramesUntil::SessionRegistered {
        wait_for_session_registered(&state).await;
    }
    // Emit the gateway ready item so the bridge captures the client id before
    // waterfall frames arrive. dsh 0.1.5 wraps every logical-stream value in the
    // `item` envelope; ≤ 0.1.4 sent the ready discriminator top-level. The
    // bridge reads that difference as its answer-protocol probe, so the shape
    // must match `legacy_answer_protocol`.
    let legacy_answer_protocol = config.lock().unwrap().legacy_answer_protocol;
    let item_envelope = config
        .lock()
        .unwrap()
        .ready_frame_item_envelope
        .unwrap_or(!legacy_answer_protocol);
    let ready_value = serde_json::json!({
        "type": "ready",
        "clientId": "$events-client-1",
        "host": "mock-host"
    });
    let ready = serde_json::to_string(&if item_envelope {
        serde_json::json!({
            "type": "item",
            "streamId": "mock-$events",
            "value": ready_value
        })
    } else {
        ready_value
    })
    .unwrap();
    let _ = ws.send(Message::Text(ready.into())).await;
    for frame in &script.frames {
        // Record pending questions so the respond handler can validate answers
        // the way dsh's `matchesQuestions` does.
        let is_question_waterfall = frame.get("type").and_then(Value::as_str) == Some("waterfall")
            && frame.get("event").and_then(Value::as_str) == Some("user-questions/request")
            && frame.get("eventId").and_then(Value::as_str).is_some();
        if is_question_waterfall {
            let rpc_id = frame
                .get("eventId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let questions = frame
                .pointer("/request/questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            state
                .lock()
                .unwrap()
                .pending_questions
                .insert(rpc_id, questions);
        }
        // An approval forwarded as a waterfall (dsh 0.1.5): the answer resolves
        // the waterfall with the bare outcome, correlated by `eventId`.
        if frame.get("type").and_then(Value::as_str) == Some("waterfall")
            && frame.get("event").and_then(Value::as_str) == Some("approval/request")
            && let Some(rpc_id) = frame.get("eventId").and_then(Value::as_str)
        {
            let request = frame.get("request").cloned().unwrap_or(Value::Null);
            state
                .lock()
                .unwrap()
                .pending_approvals
                .insert(rpc_id.to_string(), request);
        }
        let payload = serde_json::to_string(&serde_json::json!({
            "type": "item",
            "streamId": "mock-$events",
            "value": frame
        }))
        .unwrap();
        let _ = ws.send(Message::Text(payload.into())).await;
    }
    match script.end {
        MuxEnd::Hold => {
            // Idle keep-alive until the client disconnects. Drain incoming
            // (client→host is a protocol violation; tungstenite closes on it).
            while ws.next().await.is_some() {}
        }
        MuxEnd::Close => {
            let _ = drop_tx.send(()).await;
            let _ = ws.close(None).await;
        }
    }
}

/// Serve one logical Remote stream (the per-session `session/follow` journal or
/// the host-wide `session/control` channel): emit the scripted frames as
/// `{ type: "item", value: <frame> }`, then hold or close per `script.end`.
async fn serve_logical_stream(
    mut ws: tokio_tungstenite::WebSocketStream<PrefixedStream>,
    script: MuxScript,
    stream_id: &str,
    drop_tx: mpsc::Sender<()>,
    state: Arc<Mutex<MockState>>,
) {
    if script.hold_frames_until == HoldFramesUntil::SessionRegistered {
        wait_for_session_registered(&state).await;
    }
    for frame in &script.frames {
        let payload = serde_json::to_string(&serde_json::json!({
            "type": "item",
            "streamId": stream_id,
            "value": frame
        }))
        .unwrap();
        let _ = ws.send(Message::Text(payload.into())).await;
    }
    match script.end {
        MuxEnd::Hold => while ws.next().await.is_some() {},
        MuxEnd::Close => {
            let _ = drop_tx.send(()).await;
            let _ = ws.close(None).await;
        }
    }
}

/// Block until `run_harness_session` has registered its session sink, using the
/// `session/models` POST as the observable barrier (the sink is registered
/// synchronously right after `session.create` returns, strictly before that
/// call). Bounded so a test that never gets there fails visibly.
async fn wait_for_session_registered(state: &Arc<Mutex<MockState>>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !state.lock().unwrap().models_served && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn scripts_for_hold() -> Vec<MuxScript> {
    vec![MuxScript {
        frames: Vec::new(),
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::Connected,
    }]
}

/// Build a `$events` emit frame for one SessionEvent.
pub fn mux_session_event(session_id: &str, seq: u64, type_tag: &str, data: Value) -> Value {
    serde_json::json!({
        "type": "emit",
        "event": "session/event",
        "args": [{
            "type": "session/event",
            "sessionId": session_id,
            "event": { "type": type_tag, "seq": seq, "time": 0.0, "data": data }
        }]
    })
}

/// A live mux `assistant/chunk` frame carrying a text-delta (the real dsh
/// streaming path — finalized `assistant/message` frames do not re-emit text).
pub fn mux_assistant_text_delta(session_id: &str, seq: u64, text: &str) -> Value {
    mux_session_event(
        session_id,
        seq,
        "assistant/chunk",
        serde_json::json!({
            "turn": 1, "step": 1,
            "chunk": { "type": "text-delta", "index": 0, "text": text }
        }),
    )
}

/// A live mux finalized `assistant/message` frame (no usage). Pairs with
/// `mux_assistant_text_delta` the way real dsh streams end a step.
pub fn mux_assistant_final(session_id: &str, seq: u64) -> Value {
    mux_session_event(
        session_id,
        seq,
        "assistant/message",
        serde_json::json!({
            "turn": 1, "step": 1,
            "message": { "role": "assistant", "content": [] }
        }),
    )
}

/// Build a `$events` subscribed emit frame.
pub fn mux_subscribed(session_id: &str, last_seq: i64) -> Value {
    serde_json::json!({
        "type": "emit",
        "event": "session/subscribed",
        "args": [{ "type": "session/subscribed", "sessionId": session_id, "lastSeq": last_seq }]
    })
}

/// Build a `$events` emit frame for one host-scoped remote event.
///
/// dsh delivers session/agent lifecycle facts (`api-session/status`,
/// `api-session/error`, …) as Cordis remote events rather than session events.
pub fn mux_host_event(event: &str, args: Vec<Value>) -> Value {
    serde_json::json!({ "type": "emit", "event": event, "args": args })
}

/// One `session/follow` `assistant-stream` item (dsh 0.1.5+ live model output).
pub fn follow_assistant_stream(frame: Value) -> Value {
    serde_json::json!({ "type": "assistant-stream", "frame": frame })
}

/// A single-connection script list (one mux connection that holds open).
pub fn scripts_with(frames: Vec<Value>) -> Vec<MuxScript> {
    vec![MuxScript {
        frames,
        end: MuxEnd::Hold,
        hold_frames_until: HoldFramesUntil::Connected,
    }]
}

pub fn default_config() -> MockHarnessConfig {
    MockHarnessConfig {
        mux: Vec::new(),
        follow: Vec::new(),
        control: Vec::new(),
        history_failures: Vec::new(),
        history_events: Vec::new(),
        preset_locked: false,
        respond_reject: false,
        // Current dsh (0.1.5) answer protocol; set true to emulate ≤ 0.1.4.
        legacy_answer_protocol: false,
        ready_frame_item_envelope: None,
        require_auth: false,
        history_projections: None,
        // Current dsh descriptor; set `"images"` to emulate dsh ≤ 0.1.4.
        command_attachment_field: "submittedAttachments".to_string(),
        commands_execute_error: None,
    }
}

/// Mirror the typert gateway's `assertExactArguments`: the args object must
/// carry exactly the descriptor's parameter wire names. Returns the
/// `gateway/arguments-invalid` detail the host answers with when it does not.
fn exact_args_rejection(args: &Value, expected: &[&str]) -> Option<String> {
    let Some(args) = args.as_object() else {
        return Some("args must be a plain object".to_string());
    };
    let missing: Vec<String> = expected
        .iter()
        .filter(|key| !args.contains_key(**key))
        .map(|key| format!("{key:?}"))
        .collect();
    let extra: Vec<String> = args
        .keys()
        .filter(|key| !expected.contains(&key.as_str()))
        .map(|key| format!("{key:?}"))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    let mut clauses = Vec::new();
    if !missing.is_empty() {
        clauses.push(format!("missing {}", missing.join(", ")));
    }
    if !extra.is_empty() {
        clauses.push(format!("unexpected {}", extra.join(", ")));
    }
    Some(format!(
        "args fields do not match the descriptor: {}",
        clauses.join("; ")
    ))
}

/// The `gateway/arguments-invalid` `server-response` a real dsh host returns
/// for an args object its descriptor rejects.
fn gateway_args_invalid_response(rpc_id: &str, message: &str) -> Value {
    serde_json::json!({
        "type": "server-response",
        "rpcId": rpc_id,
        "result": {
            "ok": false,
            "error": {
                "code": "gateway/arguments-invalid",
                "message": message,
                "details": {}
            }
        }
    })
}

/// A `SessionEvent` JSON for `session.history` replay.
pub fn history_event(seq: u64, type_tag: &str, data: Value) -> Value {
    serde_json::json!({
        "event": {
            "type": type_tag,
            "seq": seq,
            "time": 0.0,
            "data": data
        }
    })
}
