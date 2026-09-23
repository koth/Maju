//! The relay driver: ties the connection (frame pipe + E2E) to a control
//! handler and an event source, running the inbound (request -> response)
//! and outbound (event push) loops concurrently.
//!
//! Layering: this is transport-only. `ControlHandler` and `EventSource`
//! are traits declared here over `relay-protocol` types, so the driver can
//! be unit-tested with mocks. The shell adapts `DesktopRemoteControl`
//! (impl `app_core::RemoteControl`) to `ControlHandler`, and bridges
//! `Application::subscribe_updates` + `UiPatchCursor` into `EventSource`.

use anyhow::Result;
use relay_protocol::{ControlRequest, ControlResponse, Envelope, Message, PairingConfirm};
use std::sync::{Arc, Mutex};

use crate::connection::RelayConnection;
use crate::crypto::SessionKey;
use crate::RelayTransport;

/// Handles an inbound `ControlRequest`, returning the matching
/// `ControlResponse` (or an `Error` response on failure).
pub trait ControlHandler: Clone + Send + 'static {
    fn handle(
        &mut self,
        request: ControlRequest,
    ) -> impl std::future::Future<Output = ControlResponse> + Send;
}

/// Produces outbound event envelopes (already-wrapped `EventFrame`
/// messages). Returns `None` when the event stream is exhausted; the
/// driver then continues the inbound loop alone.
pub trait EventSource: Send {
    fn next_event(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Envelope>> + Send;
}

/// Derives the E2E session key from a `PairingConfirm` received over the
/// relay. The shell implements this with the PC's static X25519 secret and
/// the phone's ephemeral public key carried in `session_key_material`.
/// Returns `(key, peer_device_id, emit_ciphertext_b64)`; the flag reflects
/// whether the phone advertised the `ciphertext_b64` wire capability so the
/// PC emits the compact ciphertext encoding to it.
pub trait PairingHandler: Send {
    fn derive_session_key(
        &mut self,
        confirm: PairingConfirm,
    ) -> impl std::future::Future<Output = Result<(SessionKey, String, bool)>> + Send;

    /// A `SubscriptionStatus` frame arrived on this connection. The PC driver
    /// uses the relay's `PairingRegister` ack (also a `SubscriptionStatus`)
    /// as this signal; implementations that don't care keep the default.
    /// Routing it through the frame loop (instead of a one-shot `recv` in the
    /// connect path) means an interleaved peer frame can never consume — and
    /// lose — the ack.
    fn on_subscription_status(
        &mut self,
        _status: relay_protocol::SubscriptionStatus,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

/// Drives a relay connection: routes inbound control requests to a
/// `ControlHandler` and pushes outbound events from an `EventSource`,
/// both over the same E2E connection. Fail-open: any connection error
/// ends `run` without panicking; local sessions are unaffected.
pub struct RelayDriver<T: RelayTransport, H: ControlHandler, E: EventSource, P: PairingHandler> {
    conn: RelayConnection<T>,
    handler: H,
    events: E,
    pairing: P,
    session_sink: Arc<Mutex<Option<(SessionKey, String, bool)>>>,
}

impl<T: RelayTransport, H: ControlHandler, E: EventSource, P: PairingHandler> RelayDriver<T, H, E, P> {
    pub fn new(conn: RelayConnection<T>, handler: H, events: E, pairing: P) -> Self {
        Self::new_with_session_sink(
            conn,
            handler,
            events,
            pairing,
            Arc::new(Mutex::new(None)),
        )
    }

    /// Same as [`RelayDriver::new`], but records the most recently installed
    /// E2E session key (and the peer's ciphertext-encoding capability) so a
    /// caller-owned reconnect loop can reinstall it on the next connection
    /// without a fresh pairing handshake.
    pub fn new_with_session_sink(
        conn: RelayConnection<T>,
        handler: H,
        events: E,
        pairing: P,
        session_sink: Arc<Mutex<Option<(SessionKey, String, bool)>>>,
    ) -> Self {
        Self {
            conn,
            handler,
            events,
            pairing,
            session_sink,
        }
    }

    /// Run the inbound + outbound loops until the connection closes or
    /// errors. Returns Ok on clean close, Err on a connection failure
    /// (caller may reconnect).
    pub async fn run(mut self) -> Result<()> {
        tracing::debug!(target: "remote_control", "driver run started");
        let mut outbound_done = false;
        // Heartbeats are always sent as plaintext Envelope JSON, never
        // encrypted into an EncryptedEnvelope. The relay treats any frame
        // carrying `to_device_id` as routable ciphertext and forwards it
        // blindly to the target — an encrypted heartbeat addressed to the
        // (mostly silent) phone would be dropped and the relay's
        // heartbeat_timeout would reap the PC connection after pairing.
        let heartbeat = Envelope::from_message(None, &Message::Heartbeat)
            .ok()
            .map(|env| serde_json::to_string(&env).ok())
            .flatten();
        let heartbeat_interval = std::time::Duration::from_millis(
            (self.conn.heartbeat().as_millis() / 2).max(1000) as u64,
        );
        let mut heartbeat_tick = tokio::time::interval(heartbeat_interval);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if outbound_done {
                tokio::select! {
                    inbound = self.conn.recv_envelope() => {
                        match inbound {
                            Ok(None) => {
                                tracing::debug!(target: "remote_control", "recv: connection closed by peer");
                                return Ok(());
                            }
                            Err(e) => {
                                tracing::warn!(target: "remote_control", error = %e, "recv error");
                                return Err(e);
                            }
                            Ok(Some(envelope)) => {
                                self.handle_inbound(envelope, Some(&mut heartbeat_tick))
                                    .await?
                            }
                        }
                    }
                    _ = heartbeat_tick.tick() => {
                        if let Some(heartbeat) = &heartbeat {
                            tracing::trace!(target: "remote_control", "heartbeat sent");
                            self.conn.send_heartbeat(heartbeat).await?;
                        }
                    }
                }
            } else {
                tokio::select! {
                    inbound = self.conn.recv_envelope() => {
                        match inbound {
                            Ok(None) => {
                                tracing::debug!(target: "remote_control", "recv: connection closed by peer");
                                return Ok(());
                            }
                            Err(e) => {
                                tracing::warn!(target: "remote_control", error = %e, "recv error");
                                return Err(e);
                            }
                            Ok(Some(envelope)) => {
                                self.handle_inbound(envelope, Some(&mut heartbeat_tick))
                                    .await?
                            }
                        }
                    }
                    outbound = self.events.next_event() => {
                        match outbound {
                            None => outbound_done = true,
                            Some(envelope) => self.conn.send_envelope(&envelope).await?,
                        }
                    }
                    _ = heartbeat_tick.tick() => {
                        if let Some(heartbeat) = &heartbeat {
                            tracing::trace!(target: "remote_control", "heartbeat sent");
                            self.conn.send_heartbeat(heartbeat).await?;
                        }
                    }
                }
            }
        }
    }

    /// `heartbeat_tick` is only `Some` when this call owns the driver loop's
    /// heartbeat duty (i.e. invoked from the select inside the
    /// `ControlRequest` arm). Top-level calls pass `None`; the outer loop's
    /// tick keeps heartbeating between messages.
    fn handle_inbound<'a>(
        &'a mut self,
        envelope: Envelope,
        heartbeat_tick: Option<&'a mut tokio::time::Interval>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.handle_inbound_inner(envelope, heartbeat_tick))
    }

    async fn handle_inbound_inner(
        &mut self,
        envelope: Envelope,
        mut heartbeat_tick: Option<&mut tokio::time::Interval>,
    ) -> Result<()> {
        let heartbeat = Envelope::from_message(None, &Message::Heartbeat)
            .ok()
            .and_then(|env| serde_json::to_string(&env).ok());
        let _request_id = envelope.id;
        let message = match envelope.into_message() {
            Ok(message) => message,
            Err(_) => return Ok(()),
        };
        match message {
            Message::ControlRequest(request) => {
                let has_key = self.conn.has_session_key();
                tracing::debug!(
                    target: "remote_control",
                    op = ?std::mem::discriminant(&request),
                    has_key,
                    "received ControlRequest"
                );
                // Detach the handler: desktop handlers take a blocking
                // registry mutex and can stall on slow paths (session-store
                // IO, ACP reconnect). Holding the driver loop hostage means
                // heartbeats stop and every subsequent request times out,
                // so run the handler on a blocking thread and keep pumping
                // frames while it works.
                let mut handler = self.handler.clone();
                let request_id = request.request_id();
                // Hand the request off to a blocking thread and keep
                // pumping inbound frames + heartbeats until the reply is
                // ready; awaiting inline would freeze the driver loop.
                let mut handle = tokio::task::spawn_blocking(move || {
                    futures::executor::block_on(handler.handle(request))
                });
                tracing::debug!(target: "remote_control", "handler detached to blocking thread");
                let response = loop {
                    let tick = heartbeat_tick.as_deref_mut().expect(
                        "ControlRequest arm requires the loop's heartbeat interval",
                    );
                    tokio::select! {
                        joined = &mut handle => {
                            break match joined {
                                Ok(response) => response,
                                Err(e) => ControlResponse::Error {
                                    request_id,
                                    message: format!("handler task failed: {e}"),
                                },
                            };
                        }
                        inbound = self.conn.recv_envelope() => {
                            match inbound? {
                                None => return Ok(()),
                                Some(envelope) => {
                                    self.handle_inbound(
                                        envelope,
                                        heartbeat_tick.as_deref_mut(),
                                    )
                                    .await?;
                                }
                            }
                        }
                        _ = tick.tick() => {
                            if let Some(heartbeat) = &heartbeat {
                                self.conn.send_heartbeat(heartbeat).await?;
                            }
                        }
                    }
                };
                let reply =
                    Envelope::from_message(Some(request_id), &Message::ControlResponse(response))?;
                tracing::debug!(target: "remote_control", "sending ControlResponse");
                self.conn.send_envelope(&reply).await
            }
            Message::PairingConfirm(confirm) => {
                tracing::info!(
                    target: "remote_control",
                    phone_device_id = %confirm.phone_device_id,
                    pc_device_id = %confirm.pc_device_id,
                    material_len = confirm.session_key_material.len(),
                    error = ?&confirm.error,
                    "received PairingConfirm"
                );
                // Failure reply (e.g. from a PairingResume the relay could
                // not complete): nothing to derive, drop any stale session
                // key so the next reconnect re-pairs instead of installing
                // a mismatched key and failing to decrypt every frame.
                if let Some(error) = &confirm.error {
                    tracing::warn!(target: "remote_control", error = %error, "pairing error from relay");
                    self.conn.clear_session_key();
                    if let Ok(mut guard) = self.session_sink.lock() {
                        *guard = None;
                    }
                    return Ok(());
                }
                // The relay forwards the phone's ephemeral public key in
                // `session_key_material` (plus its wire capabilities).
                // Derive the E2E session key and install it so subsequent
                // control requests decrypt.
                let (key, peer_device_id, emit_b64) =
                    match self.pairing.derive_session_key(confirm).await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(target: "remote_control", error = %e, "pairing key derivation failed");
                            return Err(e);
                        }
                    };
                tracing::info!(target: "remote_control", peer = %peer_device_id, emit_b64, "installed session key");
                self.conn.install_session_key(key, peer_device_id, emit_b64);
                if let Ok(mut guard) = self.session_sink.lock() {
                    *guard = self
                        .conn
                        .session_key()
                        .zip(self.conn.peer_device_id())
                        .map(|(key, peer)| (key, peer, emit_b64));
                }
                Ok(())
            }
            Message::PeerSessionReset(_) => {
                // The peer could not decrypt our traffic: its key for us is
                // stale (or absent). Drop ours too; when the peer reconnects
                // it re-runs pairing_resume and PairingConfirm reinstalls a
                // fresh key on both sides.
                tracing::warn!(
                    target: "remote_control",
                    "peer reports undecryptable traffic; clearing session key"
                );
                self.conn.clear_session_key();
                if let Ok(mut guard) = self.session_sink.lock() {
                    *guard = None;
                }
                Ok(())
            }
            Message::SubscriptionStatus(status) => {
                self.pairing.on_subscription_status(status).await;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_protocol::{ControlRequest, ControlResponse, EventFrame, Message};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use uuid::Uuid;
    use workspace_model::SessionStatus;

    /// In-memory transport: a pair of mpsc channels cross-linked so A's
    /// `send_text` lands in B's `recv_text` and vice versa. Avoids real
    /// WebSocket / split-sink deadlocks; tests driver routing logic
    /// (E2E + WS are validated separately in `connection` tests).
    struct ChannelTransport {
        tx: mpsc::Sender<String>,
        rx: mpsc::Receiver<String>,
    }

    impl RelayTransport for ChannelTransport {
        async fn send_text(&mut self, frame: String) -> Result<()> {
            self.tx
                .send(frame)
                .await
                .map_err(|e| anyhow::anyhow!("channel send: {e}"))
        }
        async fn recv_text(&mut self) -> Result<Option<String>> {
            Ok(self.rx.recv().await)
        }
        async fn close(&mut self) {}
    }

    /// Cross-link two in-memory connections: pc.send -> phone.recv and
    /// phone.send -> pc.recv.
    fn linked_pair() -> (
        RelayConnection<ChannelTransport>,
        RelayConnection<ChannelTransport>,
    ) {
        let (pc_tx, phone_rx) = mpsc::channel(32);
        let (phone_tx, pc_rx) = mpsc::channel(32);
        let pc = RelayConnection::new(
            ChannelTransport {
                tx: pc_tx,
                rx: pc_rx,
            },
            Duration::from_secs(30),
        );
        let phone = RelayConnection::new(
            ChannelTransport {
                tx: phone_tx,
                rx: phone_rx,
            },
            Duration::from_secs(30),
        );
        (pc, phone)
    }

    /// Handler that records requests and replies with the matching response
    /// variant (Cancel/StopTool) or an Error for unsupported ops.
    #[derive(Clone)]
    struct EchoHandler {
        seen: Vec<ControlRequest>,
    }

    impl ControlHandler for EchoHandler {
        async fn handle(&mut self, request: ControlRequest) -> ControlResponse {
            let request_id = request.request_id();
            self.seen.push(request.clone());
            match request {
                ControlRequest::Cancel { .. } => ControlResponse::Cancel { request_id },
                ControlRequest::StopTool { .. } => ControlResponse::StopTool { request_id },
                _ => ControlResponse::Error {
                    request_id,
                    message: "unsupported in mock".to_string(),
                },
            }
        }
    }

    /// Pairing handler that never derives a key (no pairing in these tests).
    struct NoopPairing;

    impl PairingHandler for NoopPairing {
        async fn derive_session_key(
            &mut self,
            _confirm: PairingConfirm,
        ) -> Result<(SessionKey, String, bool)> {
            anyhow::bail!("no pairing expected in this test")
        }
    }

    /// Event source that yields N canned event envelopes then stops.
    struct FixedEvents {
        frames: Vec<Envelope>,
    }

    impl EventSource for FixedEvents {
        async fn next_event(&mut self) -> Option<Envelope> {
            if self.frames.is_empty() {
                None
            } else {
                Some(self.frames.remove(0))
            }
        }
    }

    fn event_envelope(frame: EventFrame) -> Envelope {
        Envelope::from_message(None, &Message::Event(frame)).unwrap()
    }

    /// Receive from the phone side, skipping driver heartbeat frames, until
    /// a non-heartbeat envelope arrives. The driver loop's heartbeat
    /// interval fires its FIRST tick immediately and `tokio::select!` picks
    /// randomly among ready branches, so a heartbeat may legitimately race
    /// ahead of the response/event under test — ordering with respect to
    /// heartbeats must not be assumed.
    async fn recv_non_heartbeat(
        phone: &mut RelayConnection<ChannelTransport>,
    ) -> Envelope {
        loop {
            let env = phone
                .recv_envelope()
                .await
                .expect("transport recv")
                .expect("driver should send the expected frame");
            if env.message_type == "heartbeat" {
                continue;
            }
            return env;
        }
    }

    #[tokio::test]
    async fn driver_routes_cancel_request_to_handler_and_responds() {
        let (pc_conn, mut phone) = linked_pair();
        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        let request_id = Uuid::new_v4();
        let request = ControlRequest::Cancel { request_id };
        let req_env =
            Envelope::from_message(Some(request_id), &Message::ControlRequest(request)).unwrap();
        phone.send_envelope(&req_env).await.unwrap();

        let env = recv_non_heartbeat(&mut phone).await;
        match env.into_message().unwrap() {
            Message::ControlResponse(ControlResponse::Cancel { request_id: rid }) => {
                assert_eq!(rid, request_id);
            }
            other => panic!("expected Cancel response, got {other:?}"),
        }
        task.abort();
    }

    #[tokio::test]
    async fn driver_routes_stop_tool_request() {
        let (pc_conn, mut phone) = linked_pair();
        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        let request_id = Uuid::new_v4();
        let request = ControlRequest::StopTool {
            request_id,
            tool_call_id: "tool-7".to_string(),
        };
        let req_env =
            Envelope::from_message(Some(request_id), &Message::ControlRequest(request)).unwrap();
        phone.send_envelope(&req_env).await.unwrap();

        let env = recv_non_heartbeat(&mut phone).await;
        match env.into_message().unwrap() {
            Message::ControlResponse(ControlResponse::StopTool { request_id: rid }) => {
                assert_eq!(rid, request_id);
            }
            other => panic!("expected StopTool response, got {other:?}"),
        }
        task.abort();
    }

    #[tokio::test]
    async fn driver_pushes_events_to_phone() {
        let (pc_conn, mut phone) = linked_pair();
        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let event = event_envelope(EventFrame::SessionStatusChanged {
            session_id: "s-1".to_string(),
            status: SessionStatus::Idle,
        });
        let events = FixedEvents {
            frames: vec![event],
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        let env = recv_non_heartbeat(&mut phone).await;
        match env.into_message().unwrap() {
            Message::Event(EventFrame::SessionStatusChanged { session_id, .. }) => {
                assert_eq!(session_id, "s-1");
            }
            other => panic!("expected SessionStatusChanged event, got {other:?}"),
        }
        task.abort();
    }

    #[tokio::test]
    async fn driver_ends_cleanly_when_connection_closes() {
        // Fail-open / relay-down: dropping the phone side closes the PC's
        // recv channel; the driver's recv returns None and run() returns
        // Ok without panicking. Local state (handler) is untouched.
        let (pc_conn, phone) = linked_pair();
        drop(phone);
        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let result = tokio::time::timeout(Duration::from_secs(5), driver.run()).await;
        assert!(result.is_ok(), "driver run completes (does not hang)");
    }

    #[tokio::test]
    async fn driver_routes_request_over_e2e_encrypted_link() {
        // Same routing test but with a SessionKey installed on both sides:
        // the channel carries EncryptedEnvelope ciphertext, proving the
        // driver + connection E2E path end-to-end.
        let (mut pc_conn, mut phone) = linked_pair();
        let key = crate::SessionKey::derive(b"pairing-secret", b"kodex-relay-salt");
        pc_conn.install_session_key(key.clone(), "phone".to_string(), false);
        phone.install_session_key(key, "pc".to_string(), false);

        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        let request_id = Uuid::new_v4();
        let request = ControlRequest::Cancel { request_id };
        let req_env =
            Envelope::from_message(Some(request_id), &Message::ControlRequest(request)).unwrap();
        phone.send_envelope(&req_env).await.unwrap();

        let env = recv_non_heartbeat(&mut phone).await;
        match env.into_message().unwrap() {
            Message::ControlResponse(ControlResponse::Cancel { request_id: rid }) => {
                assert_eq!(rid, request_id);
            }
            other => panic!("expected Cancel response over E2E, got {other:?}"),
        }
        task.abort();
    }

    #[tokio::test]
    async fn pairing_error_confirm_clears_stale_session_key() {
        // A relay pairing-error reply (e.g. failed PairingResume) must drop
        // the previously installed key; otherwise the reconnect loop
        // reinstalls a stale key and every frame fails to decrypt.
        let (mut pc_conn, _phone) = linked_pair();
        let key = crate::SessionKey::derive(b"old-secret", b"kodex-relay-salt");
        pc_conn.install_session_key(key, "phone".to_string(), false);
        assert!(pc_conn.has_session_key());
        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let mut driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let error_env = Envelope::from_message(
            None,
            &Message::PairingConfirm(relay_protocol::PairingConfirm {
                error: Some("paired PC is offline; scan a new code".to_string()),
                pairing_token: String::new(),
                session_key_material: String::new(),
                pc_device_id: String::new(),
                phone_device_id: String::new(),
                capabilities: Vec::new(),
            }),
        )
        .unwrap();
        // Feed the confirm directly: the relay sends it as a plaintext frame
        // before E2E is negotiated on the fresh connection; transport
        // framing is covered by the connection tests.
        driver.handle_inbound(error_env, None).await.unwrap();
        assert!(!driver.conn.has_session_key(), "stale key cleared");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heartbeats_continue_while_handler_blocks() {
        // A slow desktop handler (registry mutex / session-store IO) must
        // not freeze the driver loop: heartbeats keep flowing so the relay
        // does not reap the connection mid-request.
        #[derive(Clone)]
        struct SleepyHandler;

        impl ControlHandler for SleepyHandler {
            async fn handle(&mut self, request: ControlRequest) -> ControlResponse {
                std::thread::sleep(Duration::from_millis(700));
                ControlResponse::Cancel {
                    request_id: request.request_id(),
                }
            }
        }

        let (pc_conn, mut phone) = linked_pair();
        let handler = SleepyHandler;
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        let request_id = Uuid::new_v4();
        let request = ControlRequest::Cancel { request_id };
        let req_env =
            Envelope::from_message(Some(request_id), &Message::ControlRequest(request)).unwrap();
        phone.send_envelope(&req_env).await.unwrap();

        // The driver must keep heartbeating while the handler sleeps, then
        // deliver the response once the handler finishes.
        let mut heartbeats = 0;
        let mut responded = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let env = tokio::time::timeout_at(deadline, phone.recv_envelope())
                .await
                .expect("frames keep flowing while the handler blocks")
                .expect("frame")
                .expect("envelope");
            match env.into_message().unwrap() {
                Message::Heartbeat => {
                    heartbeats += 1;
                }
                Message::ControlResponse(_) => {
                    responded = true;
                    break;
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
        assert!(responded, "handler response delivered");
        assert!(
            heartbeats >= 1,
            "at least one heartbeat flowed while the handler blocked (got {heartbeats})"
        );
        task.abort();
    }

    #[tokio::test]
    async fn heartbeat_stays_plaintext_after_session_key_installed() {
        // Regression: after pairing, the relay must still see plaintext
        // heartbeats. An encrypted heartbeat carries `to_device_id` and the
        // relay routes it to the (silent) phone instead of counting it,
        // eventually reaping the PC connection on heartbeat_timeout.
        let (mut pc_conn, mut phone) = linked_pair();
        let key = crate::SessionKey::derive(b"pairing-secret", b"kodex-relay-salt");
        pc_conn.install_session_key(key.clone(), "phone".to_string(), false);
        phone.install_session_key(key, "pc".to_string(), false);

        let handler = EchoHandler {
            seen: Vec::new(),
        };
        let events = FixedEvents {
            frames: Vec::new(),
        };
        let driver = RelayDriver::new(pc_conn, handler, events, NoopPairing);
        let task = tokio::spawn(async move { driver.run().await });

        use crate::RelayTransport as _;
        let frame = tokio::time::timeout(Duration::from_secs(5), phone.transport_mut().recv_text())
            .await
            .expect("heartbeat frame arrives")
            .expect("heartbeat recv ok")
            .expect("heartbeat text");
        assert!(
            frame.contains("\"heartbeat\""),
            "heartbeat is plaintext Envelope JSON, got: {frame}"
        );
        assert!(
            !frame.contains("\"to_device_id\""),
            "heartbeat must not be an EncryptedEnvelope, got: {frame}"
        );
        task.abort();
    }

}
