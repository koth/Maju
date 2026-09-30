//! Chrome DevTools Protocol client over WebSocket.
//!
//! The browser view needs one small, honest CDP conversation: send a command,
//! await its `result`, observe the event stream. The endpoint arrives in
//! three shapes — a full `ws://host:port/devtools/browser/<id>` browser
//! websocket, a bare `ws://host:port/devtools` attach-mode setting, or an
//! `http(s)://host:port` base — so connecting starts by normalizing all three
//! into a browser websocket URL, reading `webSocketDebuggerUrl` from
//! `/json/version` when the URL is not already the browser endpoint.
//!
//! The socket is owned by two tasks: one writes outbound commands, one
//! dispatches inbound traffic into per-id oneshot replies plus a broadcast
//! event channel. That keeps `call` a plain future with no socket lock held
//! across awaits, and it makes a dead connection observable exactly once —
//! the writer drains every pending reply on its way out, so no `call` can
//! hang on a connection that will never answer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

/// Errors from a CDP conversation.
///
/// This crate has no `thiserror` dependency, so the error contract is spelled
/// out by hand: `Display` for humans, `std::error::Error` for callers that
/// need `?` into a boxed error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdpError {
    /// The endpoint could not be reached or the WebSocket handshake failed.
    Connect(String),
    /// `/json/version` did not yield a usable `webSocketDebuggerUrl`.
    Discovery(String),
    /// The browser answered the command with a CDP `error` object.
    Protocol { code: i64, message: String },
    /// The transport broke mid-conversation (socket error, malformed frame).
    Transport(String),
    /// The connection is gone; commands on it can never complete.
    Disconnected,
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::Connect(detail) => write!(formatter, "cannot connect to CDP endpoint: {detail}"),
            CdpError::Discovery(detail) => {
                write!(formatter, "CDP endpoint discovery failed: {detail}")
            }
            CdpError::Protocol { code, message } => {
                write!(formatter, "CDP command failed with error {code}: {message}")
            }
            CdpError::Transport(detail) => write!(formatter, "CDP transport error: {detail}"),
            CdpError::Disconnected => write!(formatter, "CDP connection closed"),
        }
    }
}

impl std::error::Error for CdpError {}

/// One unsolicited message from the browser.
#[derive(Debug, Clone, PartialEq)]
pub struct CdpEvent {
    /// The CDP event method, e.g. `Page.loadEventFired`.
    pub method: String,
    /// The event's `params` object (or `Null` when the browser sent none).
    pub params: Value,
    /// The flattened session the event belongs to, absent for browser-level
    /// events such as `Target.targetCreated`.
    pub session_id: Option<String>,
}

/// A live CDP connection. Cheap to share behind an `Arc`.
pub struct CdpClient {
    outbound: mpsc::Sender<Outbound>,
    events: broadcast::Sender<CdpEvent>,
    next_id: AtomicU64,
}

enum Outbound {
    Command {
        id: u64,
        text: String,
        reply: oneshot::Sender<Result<Value, CdpError>>,
    },
    Close {
        ack: oneshot::Sender<()>,
    },
}

/// In-flight commands awaiting their reply.
///
/// The writer inserts and drains, the reader pops matched replies; both sides
/// take the mutex only for the map itself, never across an await. The writer
/// is the only drainer because it inserts every pending entry — once it
/// leaves, nothing more can appear behind its drain.
#[derive(Default)]
struct Wire {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>,
}

impl Wire {
    fn insert(&self, id: u64, reply: oneshot::Sender<Result<Value, CdpError>>) {
        self.lock().insert(id, reply);
    }

    fn take(&self, id: u64) -> Option<oneshot::Sender<Result<Value, CdpError>>> {
        self.lock().remove(&id)
    }

    /// Fail every unanswered command so no caller waits on a dead socket.
    fn drain(&self) {
        for (_, reply) in self.lock().drain() {
            let _ = reply.send(Err(CdpError::Disconnected));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>
    {
        self.pending.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

impl CdpClient {
    /// Connect to a browser's CDP endpoint.
    ///
    /// A URL containing `/devtools/browser/` is the browser websocket itself
    /// and is used as-is. Anything else — a bare `ws://host:port/devtools`
    /// attach-mode setting or an `http(s)://host:port` base — is normalized to
    /// its http base and the real websocket URL is discovered from
    /// `webSocketDebuggerUrl` in `/json/version`.
    pub async fn connect(endpoint: &str) -> Result<Self, CdpError> {
        let ws_url = resolve_ws_url(endpoint).await?;
        let (websocket, _response) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|error| CdpError::Connect(format!("{ws_url}: {error}")))?;
        let (sink, stream) = websocket.split();

        let (outbound_tx, outbound_rx) = mpsc::channel(64);
        let (events_tx, _) = broadcast::channel(256);
        // The reader signalling its exit is what lets the writer notice a
        // dropped connection even while it is idle.
        let (done_tx, done_rx) = watch::channel(false);
        let wire = Arc::new(Wire::default());

        tokio::spawn(drive_writer(
            sink,
            outbound_rx,
            done_rx,
            Arc::clone(&wire),
        ));
        tokio::spawn(drive_reader(
            stream,
            Arc::clone(&wire),
            events_tx.clone(),
            done_tx,
        ));

        Ok(CdpClient {
            outbound: outbound_tx,
            events: events_tx,
            next_id: AtomicU64::new(0),
        })
    }

    /// Send a command and await its `result`.
    ///
    /// `session` is the `sessionId` returned by
    /// `Target.attachToTarget { flatten: true }`; when absent, no `sessionId`
    /// field is sent and the command runs at browser level. A CDP `error`
    /// answer becomes [`CdpError::Protocol`], a dead connection
    /// [`CdpError::Disconnected`].
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, CdpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut message = serde_json::Map::new();
        message.insert("id".to_string(), json!(id));
        message.insert("method".to_string(), json!(method));
        if !params.is_null() {
            message.insert("params".to_string(), params);
        }
        if let Some(session) = session {
            message.insert("sessionId".to_string(), json!(session));
        }

        let (reply, reply_rx) = oneshot::channel();
        self.outbound
            .send(Outbound::Command {
                id,
                text: Value::Object(message).to_string(),
                reply,
            })
            .await
            .map_err(|_| CdpError::Disconnected)?;
        reply_rx.await.map_err(|_| CdpError::Disconnected)?
    }

    /// Subscribe to unsolicited CDP events.
    ///
    /// Broadcast semantics: several subscribers are supported and a slow one
    /// misses events rather than stalling the connection for everyone.
    pub fn events(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Close the WebSocket. Idempotent; every in-flight or later `call`
    /// resolves with [`CdpError::Disconnected`].
    pub async fn close(&self) {
        let (ack, ack_rx) = oneshot::channel();
        if self.outbound.send(Outbound::Close { ack }).await.is_ok() {
            let _ = ack_rx.await;
        }
    }
}

/// Turn any accepted endpoint shape into a browser WebSocket URL.
async fn resolve_ws_url(endpoint: &str) -> Result<String, CdpError> {
    let trimmed = endpoint.trim();
    let is_browser_ws = (trimmed.starts_with("ws://") || trimmed.starts_with("wss://"))
        && trimmed.contains("/devtools/browser/");
    if is_browser_ws {
        return Ok(trimmed.to_string());
    }

    let base = http_base(trimmed)?;
    let body = fetch_json_version(&base).await?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| CdpError::Discovery(format!("/json/version is not JSON: {error}")))?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CdpError::Discovery("/json/version has no webSocketDebuggerUrl".to_string())
        })
}

/// Reduce an endpoint to the `http://host:port` base serving `/json/version`.
///
/// `ws://host:port/devtools` (a bare attach-mode setting) and
/// `http://host:port` are the same thing for discovery: the path carries no
/// information and the DevTools port speaks plain HTTP.
fn http_base(endpoint: &str) -> Result<String, CdpError> {
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| CdpError::Connect(format!("endpoint {endpoint:?} has no scheme")))?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(CdpError::Connect(format!(
            "endpoint {endpoint:?} has no host"
        )));
    }
    match scheme {
        "ws" | "http" => Ok(format!("http://{authority}")),
        // DevTools endpoints are plain-text local sockets; a TLS transport is
        // deliberately out of scope (the dependency set has no TLS feature).
        "wss" | "https" => Err(CdpError::Connect(format!(
            "endpoint {endpoint:?} needs TLS, which is not supported; DevTools endpoints are plain-text local sockets"
        ))),
        other => Err(CdpError::Connect(format!(
            "endpoint {endpoint:?} has unsupported scheme {other:?}"
        ))),
    }
}

/// GET `/json/version` over a hand-rolled HTTP/1.1 request.
///
/// Discovery only ever targets the local DevTools port, so this is a request
/// line, a host header, and a body read by `Content-Length` — enough to find
/// the browser without pulling an HTTP stack into this crate.
async fn fetch_json_version(base: &str) -> Result<String, CdpError> {
    let authority = base.trim_start_matches("http://").trim_end_matches('/');
    let (host, port) = split_host_port(authority)?;
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| CdpError::Connect(format!("cannot reach {base}: {error}")))?;
    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| CdpError::Connect(format!("cannot reach {base}: {error}")))?;

    let mut response: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(position) = find_subslice(&response, b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| CdpError::Discovery(format!("{base}: {error}")))?;
        if read == 0 {
            return Err(CdpError::Discovery(format!(
                "{base} closed before the end of the headers"
            )));
        }
        response.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&response[..head_end]).to_string();
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(CdpError::Discovery(format!(
            "{base}/json/version answered with status {status}"
        )));
    }
    let content_length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    });
    match content_length {
        Some(length) => {
            while response.len() < head_end + length {
                let read = stream
                    .read(&mut chunk)
                    .await
                    .map_err(|error| CdpError::Discovery(format!("{base}: {error}")))?;
                if read == 0 {
                    break;
                }
                response.extend_from_slice(&chunk[..read]);
            }
            if response.len() < head_end + length {
                return Err(CdpError::Discovery(format!(
                    "{base}/json/version body is truncated"
                )));
            }
            Ok(String::from_utf8_lossy(&response[head_end..head_end + length]).into_owned())
        }
        // No length announced: the body ends when the server closes.
        None => loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| CdpError::Discovery(format!("{base}: {error}")))?;
            if read == 0 {
                return Ok(String::from_utf8_lossy(&response[head_end..]).into_owned());
            }
            response.extend_from_slice(&chunk[..read]);
        },
    }
}

/// Split `host:port`, defaulting to port 80, with bracketed IPv6 accepted.
fn split_host_port(authority: &str) -> Result<(String, u16), CdpError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(|| {
            CdpError::Connect(format!("endpoint host {authority:?} has no closing bracket"))
        })?;
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse().map_err(|_| {
                CdpError::Connect(format!("endpoint host {authority:?} has an invalid port"))
            })?,
            None => 80,
        };
        return Ok((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.chars().all(|character| character.is_ascii_digit()) =>
        {
            let port = port.parse().map_err(|_| {
                CdpError::Connect(format!("endpoint host {authority:?} has an invalid port"))
            })?;
            Ok((host.to_string(), port))
        }
        _ => Ok((authority.to_string(), 80)),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Own the write half: register each command's reply before anything can come
/// back for it, and fail every unanswered command when leaving.
async fn drive_writer<S>(
    mut sink: S,
    mut outbound: mpsc::Receiver<Outbound>,
    mut reader_done: watch::Receiver<bool>,
    wire: Arc<Wire>,
) where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin + Send + 'static,
{
    loop {
        // A closed reader means no reply can ever arrive, so the writer stops
        // even when the socket write itself would still succeed.
        let item = tokio::select! {
            _ = reader_done.changed() => None,
            item = outbound.recv() => item,
        };
        match item {
            Some(Outbound::Command { id, text, reply }) => {
                wire.insert(id, reply);
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            Some(Outbound::Close { ack }) => {
                let _ = sink.send(Message::Close(None)).await;
                let _ = ack.send(());
                break;
            }
            // Every client handle is gone; close the socket so the reader
            // finishes too.
            None => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    }
    wire.drain();
}

/// Own the read half: correlate responses to their oneshot, broadcast events.
async fn drive_reader<St>(
    mut stream: St,
    wire: Arc<Wire>,
    events: broadcast::Sender<CdpEvent>,
    reader_done: watch::Sender<bool>,
) where
    St: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(item) = stream.next().await {
        let message = match item {
            Ok(message) => message,
            Err(_) => break,
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            // CDP carries only text frames; pongs and close are tungstenite's
            // business, anything else is not protocol traffic.
            _ => continue,
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            if let Some(reply) = wire.take(id) {
                let outcome = match value.get("error") {
                    Some(error) => Err(CdpError::Protocol {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown CDP error")
                            .to_string(),
                    }),
                    _ => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(outcome);
            }
        } else if let Some(method) = value.get("method").and_then(Value::as_str) {
            let event = CdpEvent {
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
                session_id: value
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            // No subscriber is normal before anyone calls `events()`.
            let _ = events.send(event);
        }
    }
    // This is what turns every unanswered `call` into `Disconnected`: the
    // writer wakes, leaves, and drains the pending map.
    let _ = reader_done.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio_tungstenite::WebSocketStream;

    type Log = Arc<StdMutex<Vec<Value>>>;

    struct FakeCdp {
        port: u16,
        log: Log,
        push: broadcast::Sender<Value>,
        kill: watch::Sender<bool>,
    }

    impl FakeCdp {
        fn endpoint(&self, path: &str) -> String {
            format!("ws://127.0.0.1:{}{}", self.port, path)
        }

        /// Every frame the client sent, in order.
        fn record(&self) -> Vec<Value> {
            self.log.lock().unwrap().clone()
        }

        /// Send a raw frame to every live connection.
        fn push(&self, frame: Value) {
            let _ = self.push.send(frame);
        }

        /// Drop every live connection from the server side.
        fn disconnect(&self) {
            let _ = self.kill.send(true);
        }
    }

    /// A scripted CDP peer: accept WebSocket connections, answer every
    /// command with `responder`'s body (merged next to the request id; `Null`
    /// means never answer), and broadcast whatever the test pushes.
    fn serve<F>(responder: F) -> Arc<FakeCdp>
    where
        F: Fn(&Value) -> Value + Send + Sync + 'static,
    {
        let log: Log = Arc::new(StdMutex::new(Vec::new()));
        let (push, _) = broadcast::channel(64);
        let (kill, _) = watch::channel(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener = TcpListener::from_std(listener).unwrap();
        let responder = Arc::new(responder);
        let accept_log = Arc::clone(&log);
        let accept_push = push.clone();
        let accept_kill = kill.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let log = Arc::clone(&accept_log);
                let push_rx = accept_push.subscribe();
                let mut kill_rx = accept_kill.subscribe();
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    let Ok(websocket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    serve_connection(websocket, log, push_rx, &mut kill_rx, responder).await;
                });
            }
        });
        Arc::new(FakeCdp {
            port,
            log,
            push,
            kill,
        })
    }

    async fn serve_connection<F>(
        websocket: WebSocketStream<TcpStream>,
        log: Log,
        mut push_rx: broadcast::Receiver<Value>,
        kill_rx: &mut watch::Receiver<bool>,
        responder: Arc<F>,
    ) where
        F: Fn(&Value) -> Value,
    {
        let mut websocket = websocket;
        loop {
            tokio::select! {
                incoming = websocket.next() => {
                    let Some(Ok(message)) = incoming else { break };
                    let Message::Text(text) = message else { continue };
                    let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                    log.lock().unwrap().push(frame.clone());
                    if frame.get("id").is_some() {
                        // A `Null` body means "never answer": the call may
                        // only resolve through the disconnect drain.
                        let body = responder(&frame);
                        if !body.is_null() {
                            let mut reply = json!({ "id": frame["id"] });
                            if let Some(body) = body.as_object() {
                                for (key, value) in body {
                                    reply[key] = value.clone();
                                }
                            }
                            let _ = websocket.send(Message::Text(reply.to_string().into())).await;
                        }
                    }
                }
                pushed = push_rx.recv() => {
                    match pushed {
                        Ok(frame) => {
                            let _ = websocket.send(Message::Text(frame.to_string().into())).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = kill_rx.changed() => break,
            }
        }
    }

    /// A minimal `/json/version` server pointing discovery at `ws_url`.
    fn serve_json_version(ws_url: String) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener = TcpListener::from_std(listener).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let ws_url = ws_url.clone();
                tokio::spawn(async move {
                    let mut request: Vec<u8> = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while find_subslice(&request, b"\r\n\r\n").is_none() {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let head = String::from_utf8_lossy(&request).to_string();
                    let (status, body) = if head.starts_with("GET /json/version ") {
                        (
                            "200 OK",
                            json!({ "Browser": "FakeCDP/1.0", "webSocketDebuggerUrl": ws_url })
                                .to_string(),
                        )
                    } else {
                        ("404 Not Found", "{}".to_string())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn commands_round_trip_with_session_passthrough() {
        let fake = serve(|frame| {
            json!({ "result": {
                "method": frame["method"],
                "sessionId": frame.get("sessionId").cloned().unwrap_or(Value::Null),
                "params": frame.get("params").cloned().unwrap_or(Value::Null),
            } })
        });
        let client = CdpClient::connect(&fake.endpoint("/devtools/browser/test-browser"))
            .await
            .unwrap();

        let result = client
            .call("Page.enable", json!({ "verbose": true }), Some("session-1"))
            .await
            .unwrap();
        assert_eq!(result["method"], "Page.enable");
        assert_eq!(result["sessionId"], "session-1");
        assert_eq!(result["params"]["verbose"], true);

        // Browser-level: no sessionId field must appear on the wire.
        let result = client
            .call("Target.getTargets", Value::Null, None)
            .await
            .unwrap();
        assert_eq!(result["method"], "Target.getTargets");
        assert_eq!(result["sessionId"], Value::Null);

        let recorded = fake.record();
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0]["id"].is_number());
        assert_eq!(recorded[0]["method"], "Page.enable");
        assert_eq!(recorded[0]["sessionId"], "session-1");
        assert_eq!(recorded[0]["params"], json!({ "verbose": true }));
        assert_eq!(recorded[1]["method"], "Target.getTargets");
        assert!(recorded[1].get("sessionId").is_none());
        assert!(recorded[1].get("params").is_none());

        client.close().await;
    }

    #[tokio::test]
    async fn events_reach_subscribers_tagged_with_their_session() {
        let fake = serve(|_| json!({ "result": {} }));
        let client = CdpClient::connect(&fake.endpoint("/devtools/browser/test-browser"))
            .await
            .unwrap();
        let mut events = client.events();

        fake.push(json!({
            "method": "Target.targetCreated",
            "params": { "targetInfo": { "targetId": "t-1" } },
        }));
        fake.push(json!({
            "method": "Page.screencastFrame",
            "params": { "data": "abc" },
            "sessionId": "s-1",
        }));

        let first = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("first event must arrive")
            .unwrap();
        assert_eq!(first.method, "Target.targetCreated");
        assert_eq!(first.session_id, None);
        assert_eq!(first.params["targetInfo"]["targetId"], "t-1");

        let second = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("second event must arrive")
            .unwrap();
        assert_eq!(second.method, "Page.screencastFrame");
        assert_eq!(second.session_id.as_deref(), Some("s-1"));
        assert_eq!(second.params["data"], "abc");

        client.close().await;
    }

    #[tokio::test]
    async fn error_responses_become_protocol_errors() {
        let fake = serve(|_| {
            json!({ "error": { "code": -32000, "message": "Target closed" } })
        });
        let client = CdpClient::connect(&fake.endpoint("/devtools/browser/test-browser"))
            .await
            .unwrap();

        let error = client
            .call("Page.enable", json!({}), None)
            .await
            .expect_err("an error answer must not look like a result");
        assert_eq!(
            error,
            CdpError::Protocol {
                code: -32000,
                message: "Target closed".to_string(),
            }
        );

        client.close().await;
    }

    #[tokio::test]
    async fn http_and_bare_ws_endpoints_discover_the_browser_websocket() {
        let fake = serve(|frame| json!({ "result": { "method": frame["method"] } }));
        let version_port = serve_json_version(fake.endpoint("/devtools/browser/discovered"));

        // http(s)://host:port base.
        let client = CdpClient::connect(&format!("http://127.0.0.1:{version_port}"))
            .await
            .unwrap();
        let result = client
            .call("Target.getTargets", Value::Null, None)
            .await
            .unwrap();
        assert_eq!(result["method"], "Target.getTargets");
        client.close().await;

        // Bare ws://host:port/devtools: an attach-mode settings.endpoint.
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{version_port}/devtools"))
            .await
            .unwrap();
        let result = client
            .call("Target.getTargets", Value::Null, None)
            .await
            .unwrap();
        assert_eq!(result["method"], "Target.getTargets");
        client.close().await;
    }

    #[tokio::test]
    async fn calls_after_the_connection_dies_return_disconnected() {
        // The peer never answers: once it drops the socket, only the
        // disconnect drain can resolve a call, so success would be a hang or
        // a lie.
        let fake = serve(|_| Value::Null);
        let client = CdpClient::connect(&fake.endpoint("/devtools/browser/test-browser"))
            .await
            .unwrap();

        fake.disconnect();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.call("Page.enable", json!({}), None),
        )
        .await
        .expect("a call on a dead connection must not hang")
        .expect_err("a call on a dead connection must fail");
        assert_eq!(error, CdpError::Disconnected);

        client.close().await;
    }
}
