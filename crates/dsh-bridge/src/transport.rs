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
    ANSWER_PROTOCOL_MISMATCH, AnswerProtocol, ClientRequest, CommandAttachmentField,
    CommandsExecutePayload, HostDescribeValue, RpcId, RpcReceipt, RpcResult, ServerRequest,
    ServerResponse, SessionAddress, SessionCancelPayload, SessionCancelValue, SessionCreatePayload,
    SessionCreateValue, SessionForkPayload, SessionForkValue, SessionHistoryPayload,
    SessionHistoryValue, SessionId, SessionListPayload, SessionListValue, SessionModelsPayload,
    SessionPageRequest, SessionPromptPayload, SessionPromptValue, SessionSelectModelPayload,
    is_command_attachment_field_mismatch,
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
/// Cut used to interrogate one session's page cursor — any value past the
/// session's log triggers the harness rejection that names the real cursor (see
/// [`HttpClient::session_cursor`]).
///
/// Capped at the largest integer a JSON number round-trips exactly
/// (`Number.MAX_SAFE_INTEGER`). The gateway validates `throughSeq` as a JS safe
/// integer *before* it compares it against the cursor, so a larger probe —
/// `i64::MAX / 4`, as this used to be — never reaches the "past cursor N"
/// rejection the probe parses. It comes back as
/// "throughSeq must be an integer greater than or equal to -1" instead, the
/// probe fails, and the callers fall back to a cut of 0: `session/page` answers
/// an empty page, so the history walk sees nothing. That silently broke every
/// walk over a sink with no follow cursor yet — including a freshly forked
/// child, whose transcript was then rebuilt from the follow journal alone
/// (which carries tool calls but no messages).
const SESSION_CURSOR_PROBE_SEQ: i64 = 9_007_199_254_740_991;

/// Shared HTTP client for a harness host. Connection pooling multiplexes
/// concurrent control POSTs from multiple sessions; the cookie jar is empty for
/// loopback. Cloning is cheap (Arc internals).
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    base_url: reqwest::Url,
    auth_cookie: Option<String>,
    /// Attachment wire name the host last ACCEPTED for `commands/execute`.
    /// `false` = `submittedAttachments` (dsh ≥ 0.1.5), `true` = `images`
    /// (dsh ≤ 0.1.4). The bridge has no version negotiation for this
    /// surface (`host.describe` returns no invocation manifest), so the answer
    /// is learned from the first call and reused for the rest of the
    /// connection.
    legacy_command_attachments: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The answer protocol this host speaks, learned from the `$events` item
    /// envelope and corrected by a one-shot retry (`false` = `Envelope`,
    /// i.e. dsh ≥ 0.1.5).
    legacy_answer_protocol: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
            legacy_command_attachments: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            legacy_answer_protocol: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub fn endpoint(&self) -> &str {
        self.base_url.as_str()
    }

    /// The answer carrier this host is believed to speak (see
    /// [`AnswerProtocol`]).
    pub fn answer_protocol(&self) -> AnswerProtocol {
        if self
            .legacy_answer_protocol
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            AnswerProtocol::Legacy
        } else {
            AnswerProtocol::Envelope
        }
    }

    /// Remember the answer carrier the host actually speaks, so later answers
    /// skip the protocol probe.
    pub fn set_answer_protocol(&self, protocol: AnswerProtocol) {
        self.legacy_answer_protocol.store(
            protocol == AnswerProtocol::Legacy,
            std::sync::atomic::Ordering::Relaxed,
        );
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

    /// POST one JSON body to `/api/<path>` and return the response text.
    /// Shared by the answer carriers, which are the only endpoints that do not
    /// ride the typed [`HttpClient::call`] envelope path.
    async fn post_json_text(&self, path: &str, body: &Value) -> anyhow::Result<String> {
        let mut resp_builder = self
            .inner
            .post(self.api_url(path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        if let Some(cookie) = &self.auth_cookie {
            resp_builder = resp_builder.header(reqwest::header::COOKIE, cookie);
        }
        let resp = resp_builder
            .send()
            .await
            .with_context(|| format!("transport failure for {path}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transport failure for {path}: HTTP {status}: {text}"
            ));
        }
        resp.text()
            .await
            .with_context(|| format!("{path} body read"))
    }

    /// POST an approval response to the legacy `/api/respond` carrier.
    /// dsh 0.1.2 moved user-question answers to the gateway-internal
    /// `$events/result` endpoint; approvals use this path only on hosts that
    /// predate the forwarded `approval/request` waterfall (see
    /// [`AnswerProtocol::Legacy`]).
    pub async fn respond_legacy(&self, response: &Value) -> anyhow::Result<RpcReceipt> {
        let text = self.post_json_text("respond", response).await?;
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
    /// `{ clientId, eventId, outcome }` object the gateway validates, rendered
    /// for the protocol in use (the answer value itself is protocol-dependent).
    ///
    /// The carrier differs by host generation:
    ///
    /// * `Envelope` (dsh ≥ 0.1.5) — the endpoint sits behind the shared
    ///   Connection RPC interceptor, so the body must be a full
    ///   `client-request` and the receipt is a `server-response`. A bare
    ///   `{ args }` body is refused with `gateway/bad-request: invalid
    ///   client-request message`, which is what made every question submission
    ///   fail with a bare "gateway error" (the old parser found neither `ok`
    ///   nor `error.message` in the envelope).
    /// * `Legacy` (dsh ≤ 0.1.4) — a bare `{ args }` body answering with a bare
    ///   `{ ok, error }` result.
    ///
    /// When the host answers with the *other* protocol's shape this returns the
    /// [`ANSWER_PROTOCOL_MISMATCH`] error, so the caller can rebuild the answer
    /// for that protocol and retry once.
    pub async fn remote_events_result(&self, args: &Value) -> anyhow::Result<RpcReceipt> {
        match self.answer_protocol() {
            AnswerProtocol::Envelope => {
                let rpc_id = uuid::Uuid::new_v4().to_string();
                let body = ClientRequest::new(
                    rpc_id.clone(),
                    "$events/result".to_string(),
                    serde_json::json!({ "args": args }),
                );
                let text = self
                    .post_json_text("$events/result", &serde_json::to_value(&body)?)
                    .await?;
                tracing::debug!(target: "dsh-bridge::respond", body = %text, "$events/result receipt raw");
                let raw: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid $events/result receipt: {text}"))?;
                // A `server-response` is the envelope spine; anything else is a
                // pre-0.1.5 host answering the bare-body carrier.
                if raw.get("result").is_none() {
                    return Err(answer_protocol_mismatch(AnswerProtocol::Envelope, &raw));
                }
                let server: ServerResponse = serde_json::from_value(raw)
                    .with_context(|| "invalid server-response for $events/result".to_string())?;
                if server.rpcId != rpc_id {
                    return Err(anyhow!(
                        "rpcId mismatch for $events/result: sent {rpc_id}, got {}",
                        server.rpcId
                    ));
                }
                match server.result {
                    RpcResult::Ok { .. } => Ok(RpcReceipt::Gateway {
                        is_ok: true,
                        message: String::new(),
                    }),
                    RpcResult::Err { error, .. } => {
                        if error.code == "gateway/bad-request" {
                            // The host refused the envelope itself (rather than
                            // the answer inside it): it speaks the legacy
                            // carrier.
                            return Err(answer_protocol_mismatch(AnswerProtocol::Envelope, &error));
                        }
                        Ok(RpcReceipt::Gateway {
                            is_ok: false,
                            message: error.message,
                        })
                    }
                }
            }
            AnswerProtocol::Legacy => {
                let text = self
                    .post_json_text("$events/result", &serde_json::json!({ "args": args }))
                    .await?;
                tracing::debug!(target: "dsh-bridge::respond", body = %text, "$events/result receipt raw");
                let raw: Value = serde_json::from_str(&text)
                    .with_context(|| format!("invalid $events/result receipt: {text}"))?;
                if raw.get("result").is_some() {
                    // A `server-response`: this host puts `$events/result`
                    // behind the Connection RPC envelope.
                    return Err(answer_protocol_mismatch(AnswerProtocol::Legacy, &raw));
                }
                let is_ok = raw.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let message = raw
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("gateway error")
                    .to_string();
                Ok(RpcReceipt::Gateway { is_ok, message })
            }
        }
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
            through_seq: payload.through_seq as i64,
            before_seq: payload.before_seq,
            max_messages: payload.max_messages,
        };
        self.call("session.history", rpc_id, &page).await
    }

    /// Ask the harness for one session's current page cut (`throughSeq`).
    ///
    /// The cut is what the follow opening frame reports, but that frame is
    /// asynchronous: a session the bridge has only just subscribed to may not
    /// have delivered it, and `session/page` silently answers with an **empty**
    /// page for a cut of 0 (or a missing cut). Rather than depend on frame
    /// timing, ask for an impossible cut — the harness rejects it and names its
    /// own cursor in the rejection — and read the cursor out of that.
    pub async fn session_cursor(&self, session_id: &SessionId) -> anyhow::Result<u64> {
        let page = SessionPageRequest {
            address: SessionAddress::session(session_id.clone()),
            through_seq: SESSION_CURSOR_PROBE_SEQ,
            before_seq: None,
            max_messages: Some(1),
        };
        match self
            .call::<SessionPageRequest, SessionHistoryValue>(
                "session.history",
                uuid::Uuid::new_v4().to_string(),
                &page,
            )
            .await
        {
            // A cut that was not past the cursor (an empty session): report the
            // newest seq the page carried.
            Ok(value) => Ok(value
                .events
                .iter()
                .filter_map(|entry| entry.event.get("seq").and_then(Value::as_u64))
                .max()
                .unwrap_or(0)),
            Err(error) => {
                parse_past_cursor(&error.to_string()).ok_or(error)
            }
        }
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
    ///
    /// The descriptor's attachment parameter was renamed `images` →
    /// `submittedAttachments` in dsh 0.1.5, and the gateway rejects whichever
    /// name its descriptor does not declare (`gateway/arguments-invalid`, which
    /// shipped as "/compact fails with missing submittedAttachments /
    /// unexpected images" on 0.1.5.1-rc.1). Both names are only ever sent as an
    /// empty array, so the call retries once with the other name and remembers
    /// the one that worked.
    pub async fn commands_execute(
        &self,
        rpc_id: RpcId,
        session_id: &str,
        line: &str,
    ) -> anyhow::Result<Option<crate::rpc_types::CommandsExecuteValue>> {
        let preferred = self.command_attachment_field();
        // Bare wire fields; `remote_payload` adds the single `{ "args": … }`
        // envelope (see CommandsExecutePayload for the double-wrap hazard).
        let payload = CommandsExecutePayload::new(session_id, line, preferred);
        match self
            .call_bounded(
                "commands/execute",
                rpc_id.clone(),
                &payload,
                COMMANDS_EXECUTE_TIMEOUT,
            )
            .await
        {
            Ok(value) => Ok(value),
            Err(err) if is_command_attachment_field_mismatch(&err) => {
                let fallback = preferred.toggled();
                tracing::info!(
                    target: "dsh-bridge::transport",
                    from = ?preferred,
                    to = ?fallback,
                    "commands/execute rejected the attachment field name; retrying with the other wire name",
                );
                let payload = CommandsExecutePayload::new(session_id, line, fallback);
                let result = self
                    .call_bounded(
                        "commands/execute",
                        rpc_id,
                        &payload,
                        COMMANDS_EXECUTE_TIMEOUT,
                    )
                    .await;
                if result.is_ok() {
                    self.set_command_attachment_field(fallback);
                }
                result
            }
            Err(err) => Err(err),
        }
    }

    /// Attachment wire name to try first for `commands/execute`.
    fn command_attachment_field(&self) -> CommandAttachmentField {
        if self
            .legacy_command_attachments
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            CommandAttachmentField::Images
        } else {
            CommandAttachmentField::SubmittedAttachments
        }
    }

    fn set_command_attachment_field(&self, field: CommandAttachmentField) {
        self.legacy_command_attachments.store(
            field == CommandAttachmentField::Images,
            std::sync::atomic::Ordering::Relaxed,
        );
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

    /// Open the host-wide `session/control` logical stream.
    ///
    /// dsh 0.1.5 moved the session control channel (per-session queues, jobs,
    /// and the projection updates that carry context occupancy and cumulative
    /// token usage) here from the `$events` mux. It takes no parameters, so the
    /// open payload must carry an empty `args` object — the gateway's
    /// `assertExactArguments` rejects anything else.
    pub async fn open_session_control(&self) -> anyhow::Result<SseStream> {
        let request = serde_json::json!({
            "type": "open",
            "streamId": uuid::Uuid::new_v4().to_string(),
            "endpoint": "session/control",
            "payload": { "args": {} }
        });
        let text = serde_json::to_string(&request)?;
        tracing::info!(target: "dsh-bridge::ws", "opening dsh session control");
        let stream = self.open_ws("remote.mux", Some(text)).await?;
        tracing::info!(target: "dsh-bridge::ws", "dsh session control opened");
        Ok(stream)
    }

    /// Open one `job/list` logical stream for `session_id`.
    ///
    /// dsh 0.1.7 moved background jobs off the `session/control` stream into
    /// the `jobController` Typert Remote service (`dsh-api-job-controller`):
    /// `job/list` mirrors the roster one session can see as whole-set frames
    /// (`{type:"rows", jobs:[…]}`) — one on open, then one after each
    /// coalesced burst of lifecycle commits. The bridge records each roster
    /// into [`crate::jobs`] for the context dock's 后台任务 list.
    pub async fn open_job_list(&self, session_id: &str) -> anyhow::Result<SseStream> {
        let request = serde_json::json!({
            "type": "open",
            "streamId": uuid::Uuid::new_v4().to_string(),
            "endpoint": "job/list",
            "payload": { "args": { "request": { "sessionId": session_id } } }
        });
        let text = serde_json::to_string(&request)?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "opening dsh job list");
        let stream = self.open_ws("remote.mux", Some(text)).await?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "dsh job list opened");
        Ok(stream)
    }

    /// Open one `session/follow` logical stream for `session_id`.
    ///
    /// The dsh gateway multiplexes Typert Remote streams over a single
    /// WebSocket: each logical stream is opened by sending an `open` frame
    /// carrying the endpoint name and its request payload. Session content
    /// events (assistant chunks, tool calls, …) are delivered on this
    /// per-session journal stream — not on the `$events` mux.
    ///
    /// `assistantStream: true` opts into the live model-output frames. dsh
    /// 0.1.5 stopped appending the durable `assistant/chunk` event and serves
    /// streaming text/reasoning only on that opt-in channel, so leaving it out
    /// yields a transcript with no reply text or thinking at all.
    pub async fn open_session_follow(&self, session_id: &str) -> anyhow::Result<SseStream> {
        let request = session_follow_open_message(session_id);
        let text = serde_json::to_string(&request)?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "opening dsh session follow");
        let stream = self.open_ws("remote.mux", Some(text)).await?;
        tracing::info!(target: "dsh-bridge::ws", session_id = %session_id, "dsh session follow opened");
        Ok(stream)
    }
}

/// The `open` message for a `session/follow` logical stream.
///
/// `assistantStream: true` is required from dsh 0.1.5 on: the durable
/// `assistant/chunk` event no longer exists, so a follower that omits the opt-in
/// receives no reply text and no reasoning at all. Older harness schemas reject
/// unknown keys by stripping them, so sending it is backward compatible.
fn session_follow_open_message(session_id: &str) -> Value {
    serde_json::json!({
        "type": "open",
        "streamId": uuid::Uuid::new_v4().to_string(),
        "endpoint": "session/follow",
        "payload": {
            "args": {
                "request": {
                    "address": { "kind": "session", "sessionId": session_id },
                    "assistantStream": true
                }
            }
        }
    })
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

/// Read the session cursor out of the harness's page-cut rejection:
/// `session page through seq 1000000000 is past cursor 8847`.
fn parse_past_cursor(message: &str) -> Option<u64> {
    const MARKER: &str = "past cursor ";
    let start = message.rfind(MARKER)? + MARKER.len();
    let digits: String = message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn remote_endpoint(method: &str) -> anyhow::Result<&'static str> {    match method {
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

/// Shape one Remote call's wire body.
///
/// The typert gateway validates `args` against the endpoint's **generated
/// descriptor** and rejects any missing or extra key with
/// `gateway/arguments-invalid` ("args fields do not match the descriptor"), so
/// the args keys must be the descriptor's parameter *wire names* — never the
/// fields the request object happens to carry. Endpoints whose single
/// parameter is named `request` therefore need one extra nesting level:
///
/// | endpoint | descriptor parameters (wire name) | args |
/// |---|---|---|
/// | `session/create` `session/fork` `session/page` `session/prompt` `session/cancel` `session/selectModel` | `request` | `{ "request": … }` |
/// | `session/list` | `_request` | `{ "_request": … }` |
/// | `session/modelCatalog` `agentPresets/list` | *(none)* | `{}` |
/// | `commands/execute` `agentPresets/select` | bare wire fields (`agentId`, `line`, …) | the payload itself |
///
/// Source of truth: the `TYPERT_REMOTE.descriptors` tables the installed
/// harness ships (`@deepseek-ai/dsh-api-session-controller/lib/
/// typert.remote-client.js`, `@deepseek-ai/dsh-agent-presets/lib/
/// typert.remote-client.js`). `session/cancel` used to fall through to the
/// default branch and ship `{ sessionId }` bare, which the gateway rejects —
/// the stop button then did nothing while the turn kept streaming (the RPC
/// error is invisible: `cancel_prompt` is fire-and-forget).
fn remote_payload(endpoint: &str, payload: Value) -> Value {
    match endpoint {
        "session/create"
        | "session/fork"
        | "session/page"
        | "session/prompt"
        | "session/cancel"
        | "session/selectModel" => {
            serde_json::json!({ "args": { "request": payload } })
        }
        "session/list" => serde_json::json!({ "args": { "_request": payload } }),
        // No-argument methods must send an empty `args` object — dsh's
        // `assertExactArguments` rejects `{ args: { args: null } }` with
        // `gateway/arguments-invalid` ("unexpected args").
        "session/modelCatalog" | "agentPresets/list" => serde_json::json!({ "args": {} }),
        // `commands/execute` and `agentPresets/select` declare their wire
        // fields directly, so their payload structs are already keyed by wire
        // name (`CommandsExecutePayload`, `AgentPresetSelectPayload`).
        _ => serde_json::json!({ "args": payload }),
    }
}

/// The error raised when the host answered an answer carrier with the *other*
/// answer protocol's response shape. Carries the [`ANSWER_PROTOCOL_MISMATCH`]
/// marker so the caller can rebuild the (protocol-dependent) answer value and
/// retry once.
fn answer_protocol_mismatch(
    used: AnswerProtocol,
    detail: &impl std::fmt::Display,
) -> anyhow::Error {
    anyhow!(
        "{ANSWER_PROTOCOL_MISMATCH}: the host answered the {used:?} $events/result carrier with \
         the other protocol's shape: {detail}"
    )
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
    /// Whether the first logical-stream value arrived wrapped in the dsh 0.1.5
    /// `{ type: "item", value }` envelope (`false` = the pre-0.1.5 top-level
    /// form). The same release moved `$events/result` behind the Connection RPC
    /// envelope, so this doubles as the answer-protocol probe.
    item_envelope: bool,
}

/// One `session/follow` journal frame.
#[derive(Debug)]
pub enum FollowStreamItem {
    /// A journal frame (snapshot / event / assistant-stream).
    Item(Value),
    /// The host ended this logical stream (`{type:"end"}`) without closing the
    /// socket; the caller must reopen it.
    Ended,
    /// The host rejected or failed the logical stream (`{type:"error"}`).
    Failed(Value),
}

/// The `ready` payload of a `$events` frame, in either wire form.
///
/// dsh ≤ 0.1.4 sent the `ready` discriminator top-level; 0.1.5 wraps every
/// logical-stream value in an `item` envelope, so the same facts arrive as
/// `{ type: "item", value: { type: "ready", clientId, host } }`.
fn ready_payload(raw: &Value) -> Option<&Value> {
    let candidate = if raw.get("type").and_then(Value::as_str) == Some("item") {
        raw.get("value")?
    } else {
        raw
    };
    (candidate.get("type").and_then(Value::as_str) == Some("ready")).then_some(candidate)
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
            item_envelope: false,
        }
    }

    /// Whether this stream's values arrived in the dsh 0.1.5 `item` envelope.
    /// Only meaningful after the first frame was read (the ready item).
    pub fn uses_item_envelope(&self) -> bool {
        self.item_envelope
    }

    /// Record the wire form of a raw frame (see [`Self::uses_item_envelope`]).
    fn note_envelope(&mut self, raw: &Value) {
        self.item_envelope = raw.get("type").and_then(Value::as_str) == Some("item");
    }

    /// The gateway-assigned client id from the `$events` stream's ready item.
    /// `None` until the gateway proves the stream is ready.
    pub async fn remote_event_client_id(&mut self) -> Option<String> {
        if let Some(ready) = self.remote_event_ready.take() {
            return ready;
        }
        loop {
            let raw = self.next_json().await?;
            self.note_envelope(&raw);
            if let Some(ready) = ready_payload(&raw) {
                let client_id = ready
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
            self.note_envelope(&raw);
            if let Some(ready) = ready_payload(&raw) {
                if self.remote_event_ready.is_none() {
                    self.remote_event_ready = Some(
                        ready
                            .get("clientId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    );
                }
                continue;
            }
            // Each logical stream rides its own socket here, so a terminal
            // envelope means this stream is over: report the end instead of
            // leaving the reader looping on a dead stream.
            match raw.get("type").and_then(Value::as_str) {
                Some("end") => return None,
                Some("error") => {
                    tracing::warn!(
                        target: "dsh-bridge::ws",
                        frame = %raw,
                        "dsh remote stream reported an error; ending stream"
                    );
                    return None;
                }
                _ => {}
            }
            if let Some(req) = remote_message_to_server_request(&raw) {
                return Some(req);
            }
        }
    }

    /// Next `session/follow` journal item. Used by the per-session journal
    /// stream; terminal envelopes are reported so the caller can reopen the
    /// stream instead of waiting on a stream the host has already ended.
    pub async fn next_item(&mut self) -> Option<FollowStreamItem> {
        loop {
            let raw = self.next_json().await?;
            if raw.get("type").and_then(Value::as_str) == Some("item") {
                match raw.get("value") {
                    Some(value) => return Some(FollowStreamItem::Item(value.clone())),
                    None => continue,
                }
            }
            match raw.get("type").and_then(Value::as_str) {
                Some("end") => return Some(FollowStreamItem::Ended),
                Some("error") => return Some(FollowStreamItem::Failed(raw)),
                _ => {}
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

/// One decoded `session/follow` journal item.
///
/// The journal stream carries three kinds of item:
/// - `{ type: "snapshot", … }` — the opening baseline. `projections` holds
///   durable session metadata (model selection, preset, usage) and `records`
///   the durable history prefix; `assistantStream`, present only when the
///   request opted in, names an attempt that was already streaming when this
///   generation opened.
/// - `{ type: "event", event }` — one live durable session event. These are
///   `seq`-ordered so the caller can dedup a re-delivered replay.
/// - `{ type: "assistant-stream", frame }` — dsh 0.1.5+ live model output.
///   Transient: no durable `seq`, so it must never advance the caller's
///   re-baseline cursor.
#[derive(Debug)]
pub enum FollowItem {
    Snapshot {
        frames: Vec<crate::frame::MuxFrame>,
        projections: Option<Value>,
        active_attempt: Option<crate::frame::AssistantStreamAttempt>,
    },
    /// One live durable session event.
    Event(Box<crate::frame::MuxFrame>),
    /// One transient assistant live-chunk frame.
    Assistant(Box<crate::frame::AssistantStreamFrame>),
    /// An item the bridge does not consume (unknown type, or unparseable).
    Ignored,
}

/// Translate one `session/follow` WS item into a [`FollowItem`].
///
/// Snapshot `records` become `MuxFrame`s like `event` entries; the caller
/// applies them with the same dedup/`seq` rules it uses for live events.
pub fn decode_follow_item(session_id: &str, value: &Value) -> FollowItem {
    match value.get("type").and_then(Value::as_str) {
        Some("snapshot") => {
            let mut frames = Vec::new();
            if let Some(records) = value.get("records").and_then(Value::as_array) {
                for record in records {
                    if record.get("type").and_then(Value::as_str) != Some("event") {
                        continue;
                    }
                    let Some(event) = record.get("event") else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_value(event.clone()) else {
                        continue;
                    };
                    frames.push(crate::frame::MuxFrame::SessionEvent {
                        session_id: session_id.to_string(),
                        event,
                        view: record
                            .get("view")
                            .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    });
                }
            }
            let active_attempt = value
                .get("assistantStream")
                .and_then(|baseline| {
                    serde_json::from_value::<crate::frame::AssistantStreamBaseline>(
                        baseline.clone(),
                    )
                    .ok()
                })
                .and_then(|baseline| baseline.active_attempt);
            FollowItem::Snapshot {
                frames,
                projections: value.get("projections").cloned(),
                active_attempt,
            }
        }
        Some("event") => {
            let Some(event) = value.get("event") else {
                return FollowItem::Ignored;
            };
            let Ok(event) = serde_json::from_value(event.clone()) else {
                return FollowItem::Ignored;
            };
            FollowItem::Event(Box::new(crate::frame::MuxFrame::SessionEvent {
                session_id: session_id.to_string(),
                event,
                view: value
                    .get("view")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
            }))
        }
        Some("assistant-stream") => {
            let Some(frame) = value.get("frame") else {
                return FollowItem::Ignored;
            };
            match serde_json::from_value::<crate::frame::AssistantStreamFrame>(frame.clone()) {
                Ok(frame) => FollowItem::Assistant(Box::new(frame)),
                Err(_) => FollowItem::Ignored,
            }
        }
        _ => FollowItem::Ignored,
    }
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
        let payload = CommandsExecutePayload::new(
            "session-1",
            "/compact",
            CommandAttachmentField::SubmittedAttachments,
        );
        let wire = remote_payload("commands/execute", serde_json::to_value(payload).unwrap());
        assert_eq!(wire["args"]["agentId"], "session-1");
        assert_eq!(wire["args"]["line"], "/compact");
        assert_eq!(wire["args"]["submittedAttachments"], serde_json::json!([]));
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
    fn remote_payload_agent_presets_select_uses_descriptor_wire_names() {
        // `agentPresets/select` rides the default branch: the descriptor
        // declares `select(agent: Agent, agentPreset: string)` and the Agent
        // lookup's wire name is `agentId`, so the session id must travel as
        // `agentId` — not as the request object's `sessionId`.
        let payload = crate::rpc_types::AgentPresetSelectPayload {
            session_id: "session-1".into(),
            agent_preset: "standard".into(),
        };
        let wire = remote_payload(
            "agentPresets/select",
            serde_json::to_value(payload).unwrap(),
        );
        assert_eq!(wire["args"]["agentId"], "session-1");
        assert_eq!(wire["args"]["agentPreset"], "standard");
        assert!(
            wire["args"].get("sessionId").is_none(),
            "the bare request field name is rejected by the gateway: {wire}"
        );
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

    /// Regression: the stop button used to ship `{ "sessionId": … }` bare, and
    /// the gateway answered `missing "request"` — `session/cancel` never
    /// reached the host, so the turn kept streaming and the button looked dead
    /// (the RPC error was invisible: `cancel_prompt` is fire-and-forget).
    #[test]
    fn remote_payload_session_cancel_nests_under_request() {        let payload = SessionCancelPayload {
            session_id: "session-1".into(),
        };
        let wire = remote_payload("session/cancel", serde_json::to_value(payload).unwrap());
        assert_eq!(wire["args"]["request"]["sessionId"], "session-1");
        assert!(
            wire["args"].get("sessionId").is_none(),
            "the request object must not sit at the args level: {wire}"
        );
    }

    #[test]
    fn remote_payload_session_fork_nests_under_request() {
        let payload = SessionForkPayload {
            session_id: "session-1".into(),
            at_seq: Some(30),
        };
        let wire = remote_payload("session/fork", serde_json::to_value(payload).unwrap());
        assert_eq!(wire["args"]["request"]["sessionId"], "session-1");
        assert_eq!(wire["args"]["request"]["atSeq"], 30);
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

    /// The follow request must opt into the live model-output channel: dsh
    /// 0.1.5 serves streaming text/reasoning only to followers that ask for it.
    #[test]
    fn follow_request_opts_into_assistant_stream() {
        let request = session_follow_open_message("session-1");
        assert_eq!(request["endpoint"], "session/follow");
        assert_eq!(
            request["payload"]["args"]["request"]["assistantStream"],
            true
        );
        assert_eq!(
            request["payload"]["args"]["request"]["address"]["sessionId"],
            "session-1"
        );
    }

    #[test]
    fn decode_follow_item_transient_chunk() {
        let value = serde_json::json!({
            "type": "assistant-stream",
            "frame": {
                "type": "chunk",
                "attemptId": "attempt-1",
                "revision": 1,
                "index": 0,
                "time": 1.5,
                "chunk": { "type": "text-delta", "index": 0, "text": "hi" }
            }
        });
        match decode_follow_item("session-1", &value) {
            FollowItem::Assistant(frame) => match *frame {
                crate::frame::AssistantStreamFrame::Chunk { attempt_id, .. } => {
                    assert_eq!(attempt_id, "attempt-1");
                }
                _ => panic!("wrong assistant frame"),
            },
            other => panic!("wrong follow item: {other:?}"),
        }
    }

    #[test]
    fn decode_follow_item_snapshot_exposes_active_attempt() {
        let value = serde_json::json!({
            "type": "snapshot",
            "cursor": 3,
            "records": [],
            "hasMore": false,
            "projections": { "asOfSeq": 3, "values": {} },
            "assistantStream": {
                "revision": 4,
                "activeAttempt": {
                    "attemptId": "attempt-9",
                    "startedAfterSeq": 3,
                    "turn": 1,
                    "step": 2,
                    "nextIndex": 7,
                    "stream": []
                }
            }
        });
        match decode_follow_item("session-1", &value) {
            FollowItem::Snapshot {
                active_attempt,
                frames,
                projections,
            } => {
                assert!(frames.is_empty());
                assert!(projections.is_some());
                let attempt = active_attempt.expect("baseline attempt");
                assert_eq!(attempt.attempt_id, "attempt-9");
                assert_eq!((attempt.turn, attempt.step), (1, 2));
            }
            other => panic!("wrong follow item: {other:?}"),
        }
    }

    #[test]
    fn decode_follow_item_unknown_is_ignored() {
        let value = serde_json::json!({ "type": "future-frame", "payload": {} });
        assert!(matches!(
            decode_follow_item("session-1", &value),
            FollowItem::Ignored
        ));
    }

    /// dsh ≤ 0.1.4 sent the `$events` ready frame top-level; 0.1.5 wraps every
    /// logical-stream value in an `item` envelope. Both must yield the client
    /// id, which `$events/result` echoes to resolve approvals and questions.
    #[test]
    fn ready_payload_accepts_both_wire_forms() {
        let top_level = serde_json::json!({ "type": "ready", "clientId": "c-1" });
        assert_eq!(ready_payload(&top_level).unwrap()["clientId"], "c-1");

        let nested = serde_json::json!({
            "type": "item",
            "streamId": "stream-1",
            "value": { "type": "ready", "clientId": "c-2", "host": { "home": "/home/x" } }
        });
        assert_eq!(ready_payload(&nested).unwrap()["clientId"], "c-2");

        let event = serde_json::json!({
            "type": "item",
            "streamId": "stream-1",
            "value": { "type": "emit", "event": "api-session/status", "args": [] }
        });
        assert!(ready_payload(&event).is_none());
    }

    /// The page cut is learnt from the harness's own rejection. Without it a
    /// history walk ran on an empty page and read a finished turn as still
    /// running, so forking a dsh conversation always failed with 轮次尚未完成.
    #[test]
    fn past_cursor_rejection_names_the_session_cursor() {
        assert_eq!(
            parse_past_cursor("session page through seq 1000000000 is past cursor 8847"),
            Some(8847)
        );
        assert_eq!(
            parse_past_cursor(
                "gateway/bad-request: session page through seq 2305843009213693951 is past cursor 0"
            ),
            Some(0)
        );
        assert_eq!(parse_past_cursor("something else entirely"), None);
    }
}
