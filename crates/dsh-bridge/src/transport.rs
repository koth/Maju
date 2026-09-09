//! HTTP + WebSocket transport for the dsh host RPC.
//!
//! Control plane: `POST /api/<method>` with a [`ClientRequest`] body, parsing
//! the [`ServerResponse`] and verifying the echoed `rpcId` (mirrors
//! `AbstractApiClient.callUnary` in
//! `deepseek-harness/packages/host/apiproxy/src/fetch/client.ts`).
//!
//! Event plane: `GET /api/events.mux` and `GET /api/events.host` upgraded to
//! WebSocket (dsh's `client-connection` plugin answers these GETs with 426
//! Upgrade Required and serves frames over WebSocket text messages). Each WS
//! text message is a JSON [`ServerRequest`]; the payload is narrowed to the
//! frame union `F` by the caller. A malformed frame is logged and skipped —
//! one corrupt frame must not kill the stream.

use anyhow::{Context, anyhow};
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::rpc_types::{
    ClientRequest, HostDescribeValue, RpcId, RpcReceipt, ServerRequest, ServerResponse,
    SessionAddress, SessionCancelPayload, SessionCancelValue, SessionCreatePayload,
    SessionCreateValue, SessionForkPayload, SessionForkValue, SessionHistoryPayload,
    SessionHistoryValue, SessionListPayload, SessionListValue, SessionModelsPayload,
    SessionPageRequest, SessionPromptPayload, SessionPromptValue, SessionSelectModelPayload,
};

/// Default timeout for bounded control calls (a hung host must not leave the
/// session pending forever). Matches dsh's `DEFAULT_TIMEOUT_MS`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// `commands/execute` runs user-paced slash commands: `/compact` triggers a
/// full LLM summarization of the session history that routinely takes longer
/// than bounded-call timeouts, and the harness aborts the command the moment
/// the HTTP request dies (the carrier signal follows the caller) — so this
/// call gets a generous cap instead of the 30s default.
const COMMANDS_EXECUTE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Shared HTTP client for a harness host. Connection pooling multiplexes
/// concurrent control POSTs from multiple sessions; the cookie jar is empty for
/// loopback. Cloning is cheap (Arc internals).
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    base_url: reqwest::Url,
    auth_cookie: Option<String>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("base_url", &self.base_url.as_str())
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    pub fn new(endpoint: &str) -> anyhow::Result<Self> {
        let mut base_url = reqwest::Url::parse(endpoint.trim_end_matches('/'))
            .with_context(|| format!("invalid harness endpoint: {endpoint}"))?;
        let launch_token = base_url
            .query_pairs()
            .find(|(name, _)| name == "token")
            .map(|(_, value)| value.to_string());
        let inner = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .cookie_store(true)
            .build()
            .context("failed to build reqwest client")?;
        let auth_cookie = if let Some(token) = launch_token {
            tokio::task::block_in_place(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("failed to build dsh token-exchange runtime")?
                    .block_on(async { exchange_launch_token(&base_url, &token).await })
            })?
        } else {
            None
        };
        base_url.set_query(None);
        base_url.set_fragment(None);
        Ok(Self {
            inner,
            base_url,
            auth_cookie,
        })
    }

    pub fn endpoint(&self) -> &str {
        self.base_url.as_str()
    }

    fn api_url(&self, path: &str) -> reqwest::Url {
        // dsh serves every RPC endpoint under `/api/`:
        //   POST /api/<method>        (e.g. session.create, host.describe)
        //   POST /api/respond
        //   GET  /api/events.mux | /api/events.host
        self.base_url
            .join(&format!("/api/{path}"))
            .unwrap_or_else(|_| self.base_url.clone())
    }

    /// Send a control request and return the parsed business value on success.
    /// Verifies the echoed `rpcId`; returns the dsh `RpcError` on `ok: false`.
    pub async fn call<P, V>(&self, method: &str, rpc_id: RpcId, payload: &P) -> anyhow::Result<V>
    where
        P: serde::Serialize,
        V: DeserializeOwned,
    {
        self.call_bounded(method, rpc_id, payload, DEFAULT_TIMEOUT)
            .await
    }

    /// [`HttpClient::call`] with a per-request timeout cap. `None` means "use
    /// the client default"; a duration overrides the client's bounded-call
    /// timeout for this request only.
    pub async fn call_bounded<P, V>(
        &self,
        method: &str,
        rpc_id: RpcId,
        payload: &P,
        timeout: impl Into<Option<Duration>>,
    ) -> anyhow::Result<V>
    where
        P: serde::Serialize,
        V: DeserializeOwned,
    {
        let endpoint = remote_endpoint(method)?;
        let wire_payload = remote_payload(endpoint, serde_json::to_value(payload)?);
        let body = ClientRequest::new(rpc_id.clone(), endpoint, wire_payload);
        let mut request = self
            .inner
            .post(self.api_url(endpoint))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(cookie) = &self.auth_cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        if let Some(timeout) = timeout.into() {
            request = request.timeout(timeout);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("transport failure for {method}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transport failure for {method}: HTTP {status}: {text}"
            ));
        }
        let server: ServerResponse = response
            .json()
            .await
            .with_context(|| format!("invalid server-response for {method}"))?;
        if server.rpcId != rpc_id {
            return Err(anyhow!(
                "rpcId mismatch for {method}: sent {rpc_id}, got {}",
                server.rpcId
            ));
        }
        match server.result {
            crate::rpc_types::RpcResult::Ok { value, .. } => serde_json::from_value::<V>(value)
                .with_context(|| format!("invalid {method} response value")),
            crate::rpc_types::RpcResult::Err { error, .. } => Err(anyhow!("{error}")),
        }
    }

    /// POST an approval response to the legacy `/api/respond` carrier.
    /// dsh 0.1.2 moved user-question answers to the gateway-internal
    /// `$events/result` endpoint; approvals may still use this path.
    pub async fn respond_legacy(&self, response: &Value) -> anyhow::Result<RpcReceipt> {
        let mut resp_builder = self
            .inner
            .post(self.api_url("respond"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(response);
        if let Some(cookie) = &self.auth_cookie {
            resp_builder = resp_builder.header(reqwest::header::COOKIE, cookie);
        }
        let resp = resp_builder
            .send()
            .await
            .context("transport failure for respond")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transport failure for respond: HTTP {status}: {text}"
            ));
        }
        let text = resp.text().await.context("respond body read")?;
        tracing::debug!(target: "dsh-bridge::respond", body = %text, "respond receipt raw");
        let raw: Value = serde_json::from_str(&text)
            .with_context(|| format!("invalid respond receipt: {text}"))?;
        let accepted = raw
            .get("accepted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reason = raw
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("rejected")
            .to_string();
        Ok(RpcReceipt::Legacy { accepted, reason })
    }

    /// POST a resolved `$events` waterfall result to the gateway-internal
    /// `/api/$events/result` endpoint. `args` must already be the exact
    /// `{ clientId, eventId, outcome }` object the gateway validates.
    pub async fn remote_events_result(&self, args: &Value) -> anyhow::Result<RpcReceipt> {
        let mut resp_builder = self
            .inner
            .post(self.api_url("$events/result"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({ "args": args }));
        if let Some(cookie) = &self.auth_cookie {
            resp_builder = resp_builder.header(reqwest::header::COOKIE, cookie);
        }
        let resp = resp_builder
            .send()
            .await
            .context("transport failure for $events/result")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transport failure for $events/result: HTTP {status}: {text}"
            ));
        }
        let text = resp.text().await.context("$events/result body read")?;
        tracing::debug!(target: "dsh-bridge::respond", body = %text, "$events/result receipt raw");
        let raw: Value = serde_json::from_str(&text)
            .with_context(|| format!("invalid $events/result receipt: {text}"))?;
        let is_ok = raw.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let message = raw
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("gateway error")
            .to_string();
        Ok(RpcReceipt::Gateway { is_ok, message })
    }

    // ---- Typed control-method helpers ----

    pub async fn host_describe(&self, _rpc_id: RpcId) -> anyhow::Result<HostDescribeValue> {
        Ok(HostDescribeValue {
            version: "0.1.2-remote".to_string(),
            cwd: String::new(),
            provider: None,
            model: None,
            attached_sessions: 0,
            can_open_path: false,
        })
    }

    pub async fn probe(&self, rpc_id: RpcId) -> anyhow::Result<()> {
        self.session_list(rpc_id).await.map(|_| ())
    }

    pub async fn session_list(&self, rpc_id: RpcId) -> anyhow::Result<SessionListValue> {
        self.call::<SessionListPayload, SessionListValue>(
            "session.list",
            rpc_id,
            &SessionListPayload::default(),
        )
        .await
    }

    pub async fn session_create(
        &self,
        rpc_id: RpcId,
        payload: &SessionCreatePayload,
    ) -> anyhow::Result<SessionCreateValue> {
        self.call("session.create", rpc_id, payload).await
    }

    /// Fork a session from a completed-turn prefix (`session.fork`). Returns
    /// the child session id; the child inherits the source's cwd, composition,
    /// and seeded history. A fork is a fast control-plane call (no LLM work),
    /// so the default bounded timeout applies.
    pub async fn session_fork(
        &self,
        rpc_id: RpcId,
        payload: &SessionForkPayload,
    ) -> anyhow::Result<SessionForkValue> {
        self.call("session.fork", rpc_id, payload).await
    }

    pub async fn session_prompt(
        &self,
        rpc_id: RpcId,
        payload: &SessionPromptPayload,
    ) -> anyhow::Result<SessionPromptValue> {
        self.call("session.prompt", rpc_id, payload).await
    }

    pub async fn session_cancel(
        &self,
        rpc_id: RpcId,
        payload: &SessionCancelPayload,
    ) -> anyhow::Result<SessionCancelValue> {
        self.call("session.cancel", rpc_id, payload).await
    }

    pub async fn session_history(
        &self,
        rpc_id: RpcId,
        payload: &SessionHistoryPayload,
    ) -> anyhow::Result<SessionHistoryValue> {
        let page = SessionPageRequest {
            address: SessionAddress::session(payload.session_id.clone()),
            through_seq: payload
                .before_seq
                .map(|seq| seq.saturating_sub(1) as i64)
                .unwrap_or(-1),
            before_seq: payload.before_seq,
            max_messages: payload.max_messages,
        };
        self.call("session.history", rpc_id, &page).await
    }

    pub async fn session_models(
        &self,
        rpc_id: RpcId,
        _payload: &SessionModelsPayload,
    ) -> anyhow::Result<Value> {
        // The models catalog is held as opaque JSON (groups/failures shape is
        // rich and not consumed by the bridge in v1 beyond the current selection).
        self.call::<SessionModelsPayload, Value>("session.models", rpc_id, _payload)
            .await
    }

    pub async fn session_select_model(
        &self,
        rpc_id: RpcId,
        payload: &SessionSelectModelPayload,
    ) -> anyhow::Result<Value> {
        self.call::<SessionSelectModelPayload, Value>("session.selectModel", rpc_id, payload)
            .await
    }

    pub async fn agent_preset_list(
        &self,
        rpc_id: RpcId,
    ) -> anyhow::Result<crate::rpc_types::AgentPresetListValue> {
        self.call::<crate::rpc_types::AgentPresetListPayload, crate::rpc_types::AgentPresetListValue>(
            "agentPreset.list",
            rpc_id,
            &crate::rpc_types::AgentPresetListPayload {},
        )
        .await
    }

    pub async fn agent_preset_select(
        &self,
        rpc_id: RpcId,
        payload: &crate::rpc_types::AgentPresetSelectPayload,
    ) -> anyhow::Result<crate::rpc_types::AgentPresetSelectValue> {
        self.call("agentPreset.select", rpc_id, payload).await
    }

    /// Execute one slash-command line against a session's agent via the
    /// typert Remote gateway (`POST /api/commands/execute`).
    ///
    /// Returns `Ok(None)` when the line did not resolve to a registered
    /// command (the wire serializes the void business result with no `value`
    /// field). `Ok(Some(value))` carries the settled execution outcome.
    pub async fn commands_execute(
        &self,
        rpc_id: RpcId,
        session_id: &str,
        line: &str,
    ) -> anyhow::Result<Option<crate::rpc_types::CommandsExecuteValue>> {
        // Bare wire fields; `remote_payload` adds the single `{ "args": … }`
        // envelope (see CommandsExecutePayload for the double-wrap hazard).
        let payload = crate::rpc_types::CommandsExecutePayload {
            agent_id: session_id.to_string(),
            line: line.to_string(),
            images: Vec::new(),
        };
        self.call_bounded(
            "commands/execute",
            rpc_id,
            &payload,
            COMMANDS_EXECUTE_TIMEOUT,
        )
        .await
    }

    // ---- WebSocket event streams ----

    /// Open `GET /api/events.mux` as a WebSocket stream of [`ServerRequest`]s
    /// whose payload is a `MuxFrame`. dsh's `client-connection` plugin requires
    /// a WebSocket upgrade for these paths (a plain GET gets 426). Each WS text
    /// message is a JSON `ServerRequest`; malformed frames are skipped with a
    /// debug log. The stream ends when the host closes the socket or the caller
    /// drops the [`SseStream`].
    pub async fn open_mux(&self) -> anyhow::Result<SseStream> {
        self.open_remote_mux().await
    }

    async fn open_ws(&self, path: &str, open_request: Option<String>) -> anyhow::Result<SseStream> {
        // dsh serves the event streams over WebSocket, not HTTP SSE. The HTTP
        // URL (`http://...`) maps to `ws://...` (and `https://` to `wss://...`).
        let mut request = self
            .api_url(path)
            .to_string()
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1)
            .into_client_request()
            .context("failed to build dsh WebSocket request")?;
        if let Some(cookie) = &self.auth_cookie {
            request.headers_mut().insert(
                reqwest::header::COOKIE,
                reqwest::header::HeaderValue::from_str(cookie)
                    .context("invalid dsh authentication cookie")?,
            );
        }
        // No timeout — streams are long-lived; the caller's shutdown signal
        // aborts by dropping the stream (closing the socket).
        let (stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("transport failure for {path}"))?;
        if let Some(text) = open_request {
            let mut sink = stream;
            use futures::SinkExt;
            sink.send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
                .await
                .context("failed to open dsh remote event stream")?;
            return Ok(SseStream::from_ws(sink));
        }
        Ok(SseStream::from_ws(stream))
    }

    async fn open_remote_mux(&self) -> anyhow::Result<SseStream> {
        let request = serde_json::json!({
            "type": "open",
            "streamId": uuid::Uuid::new_v4().to_string(),
            "endpoint": "$events",
            "payload": { "args": {} }
        });
        let text = serde_json::to_string(&request)?;
        tracing::info!(target: "dsh-bridge::ws", request = %text, "opening dsh remote mux");
        let stream = self.open_ws("remote.mux", Some(text)).await?;
        tracing::info!(target: "dsh-bridge::ws", "dsh remote mux opened");
        Ok(stream)
    }

    /// Open one `session/follow` logical stream for `session_id`.
    ///
    /// The dsh gateway multiplexes Typert Remote streams over a single
    /// WebSocket: each logical stream is opened by sending an `open` frame
    /// carrying the endpoint name and its request payload. Session content
    /// events (assistant chunks, tool calls, …) are delivered on this
    /// per-session journal stream — not on the `$events` mux.
    pub async fn open_session_follow(&self, session_id: &str) -> anyhow::Result<SseStream> {
        let request = serde_json::json!({
            "type": "open",
            "streamId": uuid::Uuid::new_v4().to_string(),
            "endpoint": "session/follow",
            "payload": {
                "args": {
                    "request": {
                        "address": { "kind": "session", "sessionId": session_id }
                    }
                }
            }
        });
        let text = serde_json::to_string(&request)?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "opening dsh session follow");
        let stream = self.open_ws("remote.mux", Some(text)).await?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "dsh session follow opened");
        Ok(stream)
    }
}

async fn exchange_launch_token(
    base_url: &reqwest::Url,
    token: &str,
) -> anyhow::Result<Option<String>> {
    let mut root = base_url.clone();
    root.set_path("/");
    root.set_query(None);
    root.set_fragment(None);
    root.query_pairs_mut().append_pair("token", token);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build dsh token-exchange client")?;
    let response = client
        .get(root)
        .send()
        .await
        .context("dsh launch-token exchange failed")?;
    let status = response.status();
    if !(status.is_success() || status.is_redirection()) {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "dsh launch-token exchange failed: HTTP {status}: {text}"
        ));
    }
    let mut cookie: Option<String> = None;
    for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
        let value = value.to_str().context("dsh returned a non-UTF-8 cookie")?;
        let name_value = value.split(';').next().unwrap_or_default().trim();
        if name_value.starts_with("dsh-auth-") && !name_value.is_empty() {
            cookie = Some(name_value.to_string());
            break;
        }
    }
    cookie
        .map(Some)
        .ok_or_else(|| anyhow!("dsh launch-token exchange did not return an authentication cookie"))
}

fn remote_endpoint(method: &str) -> anyhow::Result<&'static str> {
    match method {
        "session.create" => Ok("session/create"),
        "session.fork" => Ok("session/fork"),
        "session.prompt" => Ok("session/prompt"),
        "session.cancel" => Ok("session/cancel"),
        "session.history" => Ok("session/page"),
        "session.models" => Ok("session/modelCatalog"),
        "session.selectModel" => Ok("session/selectModel"),
        "session.list" => Ok("session/list"),
        "agentPreset.list" => Ok("agentPresets/list"),
        "agentPreset.select" => Ok("agentPresets/select"),
        "commands/execute" => Ok("commands/execute"),
        _ => Err(anyhow!("unsupported dsh remote endpoint: {method}")),
    }
}

fn remote_payload(endpoint: &str, payload: Value) -> Value {
    match endpoint {
        "session/create" | "session/prompt" | "session/page" | "session/selectModel" => {
            serde_json::json!({ "args": { "request": payload } })
        }
        "session/list" => serde_json::json!({ "args": { "_request": payload } }),
        // No-argument methods must send an empty `args` object — dsh's
        // `assertExactArguments` rejects `{ args: { args: null } }` with
        // `gateway/arguments-invalid` ("unexpected args").
        "session/modelCatalog" | "agentPresets/list" => serde_json::json!({ "args": {} }),
        _ => serde_json::json!({ "args": payload }),
    }
}

/// A WebSocket message stream. Yields raw `tungstenite::Message`s; callers
/// parse the dsh remote-mux envelope themselves (the `$events` mux and the
/// per-session `session/follow` stream carry different payload shapes).
pub struct SseStream {
    inner: std::pin::Pin<
        Box<
            dyn Stream<
                    Item = Result<
                        tokio_tungstenite::tungstenite::Message,
                        tokio_tungstenite::tungstenite::Error,
                    >,
                > + Send,
        >,
    >,
    /// Cached ready result: `Some(Some(client_id))` when the gateway assigned
    /// a client id, `Some(None)` when a ready item had none, `None` while
    /// waiting for the first ready item.
    remote_event_ready: Option<Option<String>>,
}

impl SseStream {
    fn from_ws<S>(ws: S) -> Self
    where
        S: Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Send
            + 'static,
    {
        Self {
            inner: Box::pin(ws),
            remote_event_ready: None,
        }
    }

    /// The gateway-assigned client id from the `$events` stream's ready item.
    /// `None` until the gateway proves the stream is ready.
    pub async fn remote_event_client_id(&mut self) -> Option<String> {
        if let Some(ready) = self.remote_event_ready.take() {
            return ready;
        }
        loop {
            let raw = self.next_json().await?;
            if raw.get("type").and_then(Value::as_str) == Some("ready") {
                let client_id = raw
                    .get("clientId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.remote_event_ready = Some(client_id.clone());
                return client_id;
            }
            if remote_message_to_server_request(&raw).is_some() {
                self.remote_event_ready = Some(None);
                return None;
            }
        }
    }

    /// Next text/binary payload parsed as JSON. `None` on Close or transport
    /// error (the stream ends). Malformed frames are skipped.
    pub async fn next_json(&mut self) -> Option<Value> {
        loop {
            let msg = self.inner.next().await?;
            let msg = match msg {
                Ok(msg) => msg,
                Err(err) => {
                    tracing::debug!(target: "dsh-bridge::ws", error = %err, "ws stream error");
                    return None;
                }
            };
            match msg {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<Value>(&text) {
                        Ok(value) => {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                tracing::debug!(
                                    target: "dsh-bridge::ws",
                                    frame = %text,
                                    "dsh ws frame"
                                );
                            }
                            return Some(value);
                        }
                        Err(err) => {
                            tracing::debug!(target: "dsh-bridge::ws", error = %err, "dropping malformed WS frame");
                            continue;
                        }
                    }
                }
                tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                    match serde_json::from_slice::<Value>(&bytes) {
                        Ok(value) => return Some(value),
                        Err(err) => {
                            tracing::debug!(target: "dsh-bridge::ws", error = %err, "dropping malformed WS binary frame");
                            continue;
                        }
                    }
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => return None,
                _ => continue,
            }
        }
    }

    /// Next `$events`-mux frame as a [`ServerRequest`]. `ready` / `end` /
    /// `error` envelopes and non-`emit`/`waterfall` items are skipped.
    pub async fn next(&mut self) -> Option<ServerRequest> {
        loop {
            let raw = self.next_json().await?;
            if raw.get("type").and_then(Value::as_str) == Some("ready") {
                if self.remote_event_ready.is_none() {
                    self.remote_event_ready = Some(
                        raw.get("clientId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    );
                }
                continue;
            }
            if let Some(req) = remote_message_to_server_request(&raw) {
                return Some(req);
            }
        }
    }

    /// Next `item` frame's `value`, skipping `ready` / `end` / `error`
    /// envelopes. Used by the per-session `session/follow` journal stream.
    pub async fn next_item(&mut self) -> Option<Value> {
        loop {
            let raw = self.next_json().await?;
            if raw.get("type").and_then(Value::as_str) == Some("item") {
                return raw.get("value").cloned();
            }
        }
    }
}

fn remote_message_to_server_request(raw: &Value) -> Option<ServerRequest> {
    let type_tag = raw.get("type").and_then(Value::as_str)?;
    match type_tag {
        "item" => {
            let payload = raw.get("value")?.clone();
            let event_type = payload.get("type").and_then(Value::as_str)?;
            match event_type {
                "emit" => Some(ServerRequest {
                    type_tag: "server-request".to_string(),
                    rpcId: "remote-events".to_string(),
                    method: "remote/event".to_string(),
                    payload,
                }),
                "waterfall" => Some(ServerRequest {
                    type_tag: "server-request".to_string(),
                    rpcId: payload.get("eventId")?.as_str()?.to_string(),
                    method: "remote/event".to_string(),
                    payload,
                }),
                _ => None,
            }
        }
        "ready" => None,
        "end" | "error" => None,
        _ => None,
    }
}

/// Translate one `session/follow` WS item into `MuxFrame`s.
///
/// The journal stream yields:
/// - `{ type: "snapshot", cursor, records, hasMore, projections }` — the
///   opening baseline. Projections carry the durable model selection; records
///   are replayed through the mapping layer.
/// - `{ type: "event", event }` — a live session event.
///
/// Returns `(MuxFrame`s from records/events, snapshot projections if any)`.
/// Snapshot `records` are *not* turned into frames here — the caller replays
/// them via `replay_history`-style mapping to preserve dedup semantics.
pub fn follow_item_to_frames(
    session_id: &str,
    value: &Value,
) -> (Vec<crate::frame::MuxFrame>, Option<Value>) {
    let mut frames = Vec::new();
    let mut snapshot_projections = None;
    match value.get("type").and_then(Value::as_str) {
        Some("snapshot") => {
            if let Some(projections) = value.get("projections") {
                snapshot_projections = Some(projections.clone());
            }
            if let Some(records) = value.get("records").and_then(Value::as_array) {
                for record in records {
                    if record.get("type").and_then(Value::as_str) == Some("event")
                        && let Some(event) = record.get("event")
                    {
                        frames.push(crate::frame::MuxFrame::SessionEvent {
                            session_id: session_id.to_string(),
                            event: match serde_json::from_value(event.clone()) {
                                Ok(ev) => ev,
                                Err(_) => continue,
                            },
                            view: record
                                .get("view")
                                .and_then(|v| serde_json::from_value(v.clone()).ok()),
                        });
                    }
                }
            }
        }
        Some("event") => {
            if let Some(event) = value.get("event") {
                if let Ok(ev) = serde_json::from_value(event.clone()) {
                    frames.push(crate::frame::MuxFrame::SessionEvent {
                        session_id: session_id.to_string(),
                        event: ev,
                        view: value
                            .get("view")
                            .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    });
                }
            }
        }
        _ => {}
    }
    (frames, snapshot_projections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn remote_payload_commands_execute_wraps_bare_fields_once() {
        // Regression: `commands/execute` payloads carry the descriptor's
        // wire fields bare, so the default branch must wrap them into exactly
        // one `args` object. The bug this guards against was a payload with
        // its own `args` field double-wrapping into `{ "args": { "args": … } }`,
        // which the typert gateway rejects with `arguments-invalid`
        // ("missing agentId, line, images; unexpected args") — /compact then
        // failed silently for the user (fire-and-forget logs the RPC error).
        let payload = serde_json::json!({
            "agentId": "session-1",
            "line": "/compact",
            "images": [],
        });
        let wire = remote_payload("commands/execute", payload);
        assert_eq!(wire["args"]["agentId"], "session-1");
        assert_eq!(wire["args"]["line"], "/compact");
        assert_eq!(wire["args"]["images"], serde_json::json!([]));
        assert_eq!(
            wire.as_object().unwrap().len(),
            1,
            "wire envelope must contain only `args`, got {wire}"
        );
        assert!(
            wire["args"].get("args").is_none(),
            "double-wrapped args: {wire}"
        );
    }

    #[test]
    fn remote_payload_agent_presets_select_wraps_bare_fields_once() {
        // agentPresets/select also rides the default branch: bare fields,
        // single wrap.
        let payload = serde_json::json!({ "id": "standard", "selected": [] });
        let wire = remote_payload("agentPresets/select", payload);
        assert_eq!(wire["args"]["id"], "standard");
        assert_eq!(wire.as_object().unwrap().len(), 1);
    }

    #[test]
    fn remote_payload_legacy_endpoints_keep_their_request_key() {
        // Dotted-legacy endpoints nest under their own request key.
        let payload = serde_json::json!({ "cwd": "/tmp" });
        let wire = remote_payload("session/create", payload);
        assert_eq!(wire["args"]["request"]["cwd"], "/tmp");
        // No-argument endpoints must send an EMPTY args object — an `args`
        // key inside would be rejected by the gateway's exact-args check.
        let wire = remote_payload("session/modelCatalog", serde_json::json!({}));
        assert_eq!(wire["args"], serde_json::json!({}));
    }

    fn remote_item(value: Value) -> Message {
        Message::Text(
            serde_json::json!({ "type": "item", "streamId": "stream-1", "value": value })
                .to_string()
                .into(),
        )
    }

    #[tokio::test]
    async fn ws_stream_parses_frames() {
        let msgs: Vec<Result<Message, tokio_tungstenite::tungstenite::Error>> = vec![
            Ok(remote_item(serde_json::json!({
                "type": "emit", "event": "test/event", "args": ["r1", "s1"]
            }))),
            Ok(remote_item(serde_json::json!({
                "type": "emit", "event": "test/event", "args": ["r2", "s2"]
            }))),
            Ok(Message::Close(None)),
        ];
        let stream = futures::stream::iter(msgs);
        let mut sse = SseStream::from_ws(stream);
        let f1 = sse.next().await.unwrap();
        assert_eq!(f1.payload["args"][0], "r1");
        assert_eq!(f1.payload["args"][1], "s1");
        let f2 = sse.next().await.unwrap();
        assert_eq!(f2.payload["args"][0], "r2");
        assert_eq!(f2.payload["args"][1], "s2");
        assert!(sse.next().await.is_none());
    }

    #[tokio::test]
    async fn ws_stream_skips_malformed_frame() {
        let msgs: Vec<Result<Message, tokio_tungstenite::tungstenite::Error>> = vec![
            Ok(Message::Text("not-json".into())),
            Ok(remote_item(serde_json::json!({
                "type": "emit", "event": "test/event", "args": ["r1", "s1"]
            }))),
        ];
        let stream = futures::stream::iter(msgs);
        let mut sse = SseStream::from_ws(stream);
        // The malformed frame is skipped; the valid frame arrives.
        let f1 = sse.next().await.unwrap();
        assert_eq!(f1.payload["args"][0], "r1");
        assert!(sse.next().await.is_none());
    }
}
