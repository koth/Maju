//! Shell-side adapters that bridge `app_core` to `relay-client`'s driver
//! traits: `DesktopControlHandler` dispatches `ControlRequest` to
//! `DesktopRemoteControl` (impl `RemoteControl`), and
//! `AppUpdateEventSource` drains `Application::subscribe_updates` signals
//! and fetches Full/Patch deltas via `UiPatchCursor` +
//! `poll_active_and_get_update`, wrapping them into `EventFrame` envelopes
//! for the phone.

use app_core::{AppUpdate, RemoteControl, UiPatchCursor, UiSnapshotUpdate};
use relay_client::{ControlHandler, EventSource, PairingHandler, SessionKey};
use relay_protocol::{ControlRequest, ControlResponse, Envelope, EventFrame, Message, PairingConfirm};
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;

use crate::remote_control::DesktopRemoteControl;
use crate::state::AppState;

/// Salt for the E2E session key HKDF. Must match the phone's
/// `RELAY_SALT = "kodex-relay-salt"` and `relay-client`'s default usage.
const RELAY_E2E_SALT: &[u8] = b"kodex-relay-salt";

/// The patch cursor shared between the control handler and the event source.
/// Resetting it from the control side forces the next event poll to emit a
/// Full snapshot — the phone's entry sync for a session the PC already has
/// active (no revision change → no delta → otherwise no push at all).
pub type SharedUiPatchCursor = std::sync::Arc<std::sync::Mutex<UiPatchCursor>>;

fn reset_shared_cursor(cursor: &SharedUiPatchCursor) {
    if let Ok(mut guard) = cursor.lock() {
        *guard = UiPatchCursor::default();
    }
}

/// Adapts the PC's device identity to the driver's `PairingHandler` trait.
/// On `PairingConfirm` it derives the E2E session key from the PC static
/// X25519 secret and the phone ephemeral public key carried in
/// `session_key_material`, and returns it for the driver to install.
pub struct DesktopPairingHandler {
    app: AppHandle,
}

impl DesktopPairingHandler {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl PairingHandler for DesktopPairingHandler {
    async fn derive_session_key(
        &mut self,
        confirm: PairingConfirm,
    ) -> anyhow::Result<(SessionKey, String, bool)> {
        let manager = self.app.state::<AppState>().remote_control();
        let identity = manager.device_identity()?;
        let key = identity.derive_pairing_session_key(
            &confirm.session_key_material,
            RELAY_E2E_SALT,
        )?;
        // Diagnostic: log the derived key prefix so it can be matched
        // against the phone's `resume sessionKey prefix` log line.
        let prefix: String = key
            .bytes_prefix()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        // The phone's advertised wire capabilities decide the outbound
        // ciphertext encoding: base64url (~1.33 chars/byte) for phones that
        // advertise it, the legacy JSON number array (~4 chars/byte) for
        // older builds. Inbound frames decode either way.
        let emit_b64 = confirm
            .capabilities
            .iter()
            .any(|cap| cap == relay_protocol::CAPABILITY_CIPHERTEXT_B64);
        tracing::debug!(target: "remote_control", key_prefix = %prefix, emit_b64, capabilities = ?confirm.capabilities, "derived pairing session key");
        // The phone's device id is the peer for E2E AAD.
        Ok((key, confirm.phone_device_id, emit_b64))
    }

    async fn on_subscription_status(
        &mut self,
        _status: relay_protocol::SubscriptionStatus,
    ) {
        // The relay acks `PairingRegister` with a `SubscriptionStatus` frame.
        // Flip the panel's「正在把配对码注册到 relay…」flag here — routed
        // through the driver's frame loop, so an interleaved request frame
        // from a paired phone can no longer consume the ack (the old one-shot
        // `recv` in the connect path left the panel stuck forever on that
        // warning whenever the phone beat the ack to the wire).
        self.app
            .state::<AppState>()
            .remote_control()
            .mark_pairing_registered();
        tracing::info!(target: "remote_control", "pairing code registered with relay");
    }
}

/// Adapts `DesktopRemoteControl` to the driver's `ControlHandler` trait.
/// Each inbound `ControlRequest` is dispatched to the matching
/// `RemoteControl` method; the result is wrapped into the matching
/// `ControlResponse` (or `Error` on failure).
#[derive(Clone)]
pub struct DesktopControlHandler {
    control: DesktopRemoteControl,
    cursor: SharedUiPatchCursor,
}

impl DesktopControlHandler {
    pub fn new(app: AppHandle, cursor: SharedUiPatchCursor) -> Self {
        Self {
            control: DesktopRemoteControl::new(app),
            cursor,
        }
    }
}

impl ControlHandler for DesktopControlHandler {
    async fn handle(&mut self, request: ControlRequest) -> ControlResponse {
        let request_id = request.request_id();
        let is_list_sessions = matches!(&request, ControlRequest::ListSessions { .. });
        let is_switch_session = matches!(&request, ControlRequest::SwitchSession { .. });
        let is_get_state = matches!(&request, ControlRequest::GetState { .. });
        if is_list_sessions {
            tracing::debug!(target: "remote_control", request_id = %request_id, "handling ListSessions");
        }
        if is_switch_session {
            tracing::debug!(target: "remote_control", request_id = %request_id, "handling SwitchSession");
        }
        if is_get_state {
            tracing::debug!(target: "remote_control", request_id = %request_id, "handling GetState");
        }
        let result = match request {
            ControlRequest::ListSessions { .. } => self
                .control
                .list_sessions()
                .await
                .map(|sessions| ControlResponse::ListSessions {
                    request_id,
                    sessions,
                }),
            ControlRequest::CreateSession {
                workspace_root,
                agent,
                preset,
                ..
            } => self
                .control
                .create_session(workspace_root, agent, preset)
                .await
                .map(|session_id| ControlResponse::CreateSession {
                    request_id,
                    session_id,
                }),
            // Settings + harness host live in the shell, not the active
            // `Application`, so this one bypasses the `RemoteControl` trait
            // (same boundary reasoning as `list_sessions`, which the trait
            // injects from the shell).
            ControlRequest::ListAgentOptions { .. } => {
                crate::remote_control::list_agent_options()
                    .await
                    .map(|options| ControlResponse::AgentOptions {
                        request_id,
                        options,
                    })
            }
            ControlRequest::SetConfigControl {
                control_id,
                value_id,
                provider,
                ..
            } => self
                .control
                .set_config_control(control_id, value_id, provider)
                .await
                .map(|config| ControlResponse::SetConfigControl { request_id, config }),
            ControlRequest::SwitchSession {
                session_id,
                workspace_root,
                ..
            } => self
                .control
                .switch_session(session_id, workspace_root)
                .await
                .map(|_| ControlResponse::SwitchSession { request_id }),
            ControlRequest::SendPrompt { prompt, .. } => self
                .control
                .send_prompt(prompt)
                .await
                .map(|_| ControlResponse::SendPrompt { request_id }),
            ControlRequest::GetState {
                known_session_id,
                known_revision,
                ..
            } => {
                let known = known_session_id.zip(known_revision);
                self.control
                    .get_state(known)
                    .await
                    .map(|result| match result {
                        app_core::RemoteGetState::Snapshot(snapshot) => {
                            ControlResponse::GetState {
                                request_id,
                                snapshot: Some(snapshot),
                                up_to_date: false,
                            }
                        }
                        // Short-circuit: the phone's held (session, revision)
                        // is still current, so no snapshot crosses the relay.
                        app_core::RemoteGetState::UpToDate => ControlResponse::GetState {
                            request_id,
                            snapshot: None,
                            up_to_date: true,
                        },
                    })
            }
            ControlRequest::ResolvePermission {
                permission_request_id,
                option_id,
                guidance,
                input_response,
                ..
            } => self
                .control
                .resolve_permission(permission_request_id, option_id, guidance, input_response)
                .await
                .map(|_| ControlResponse::ResolvePermission { request_id }),
            ControlRequest::Cancel { .. } => self
                .control
                .cancel()
                .await
                .map(|_| ControlResponse::Cancel { request_id }),
            ControlRequest::StopTool {
                tool_call_id, ..
            } => self
                .control
                .stop_tool(tool_call_id)
                .await
                .map(|_| ControlResponse::StopTool { request_id }),
            ControlRequest::GetFileDiff { message_id, path, .. } => self
                .control
                .get_file_diff(message_id.to_string(), path)
                .await
                .map(|change| ControlResponse::FileDiff {
                    request_id,
                    change: Some(change),
                }),
        };
        // A phone-initiated session switch/create must always be followed by a
        // Full snapshot push, even when the target session was already active
        // on the PC (no revision change → no delta). Resetting the shared
        // cursor makes the event source's next poll emit a Full snapshot. This
        // runs after the request completed so the reset never races a poll
        // that was mid-flight against the old session state.
        if result.is_ok() && is_switch_session {
            reset_shared_cursor(&self.cursor);
        }
        if is_list_sessions {
            tracing::debug!(target: "remote_control", request_id = %request_id, "ListSessions finished");
        }
        if is_switch_session {
            tracing::debug!(target: "remote_control", request_id = %request_id, "SwitchSession finished");
        }
        if is_get_state {
            tracing::debug!(target: "remote_control", request_id = %request_id, "GetState finished");
        }
        match result {
            Ok(response) => response,
            Err(message) => ControlResponse::Error {
                request_id,
                message,
            },
        }
    }
}

/// Adapts `Application::subscribe_updates` + `UiPatchCursor` to the
/// driver's `EventSource` trait. On each `AppUpdate` signal it fetches the
/// Full/Patch delta via `poll_active_and_get_remote_update` and wraps it into
/// an `EventFrame` envelope. `PermissionRequested` signals become
/// `EventFrame::PermissionRequest`. Returns None only when the update
/// pipeline is gone (the driver keeps running its inbound loop alone).
///
/// Robustness (mirrors the local snapshot bridge in `main.rs`): the broadcast
/// receiver is bound to ONE `Application`, so it is re-subscribed whenever the
/// active workspace key changes — otherwise a workspace switch (or a relay
/// reconnect that happens before any workspace finished opening) silently
/// kills the phone's event stream and the conversation stops updating. A
/// 220ms fallback wake catches missed signals and workspace switches even
/// when no signal ever fires; the revision-based cursor makes extra polls
/// free (they return `None` until something changed).
pub struct AppUpdateEventSource {
    rx: Option<broadcast::Receiver<AppUpdate>>,
    last_workspace_key: Option<String>,
    /// Shared with `DesktopControlHandler`: a phone-initiated SwitchSession
    /// resets it so the next poll re-emits a Full snapshot even when the PC
    /// has no revision change to report.
    cursor: SharedUiPatchCursor,
    app: AppHandle,
}

const EVENT_FALLBACK_WAKE_MS: u64 = 220;

impl AppUpdateEventSource {
    pub fn new(app: AppHandle, cursor: SharedUiPatchCursor) -> Self {
        let last_workspace_key = app
            .state::<AppState>()
            .active_workspace_key()
            .ok()
            .flatten();
        let rx = Self::subscribe(&app);
        Self {
            rx,
            last_workspace_key,
            cursor,
            app,
        }
    }

    fn subscribe(app: &AppHandle) -> Option<broadcast::Receiver<AppUpdate>> {
        app.state::<AppState>()
            .subscribe_active_updates()
            .ok()
            .flatten()
    }

    /// Re-subscribe when the active workspace changed and reset the cursor so
    /// the next poll emits a Full snapshot for the new target. Returns true
    /// when the subscription was refreshed.
    fn refresh_subscription(&mut self) -> bool {
        let key = self
            .app
            .state::<AppState>()
            .active_workspace_key()
            .ok()
            .flatten();
        if key == self.last_workspace_key {
            return false;
        }
        self.last_workspace_key = key;
        self.rx = Self::subscribe(&self.app);
        reset_shared_cursor(&self.cursor);
        true
    }

    /// Fetch this subscriber's Full/Patch delta and wrap it into an event
    /// envelope. `None` when nothing changed (or no workspace is open).
    fn poll_delta(&mut self) -> Option<Envelope> {
        let mut cursor = self.cursor.lock().ok()?;
        let update = self
            .app
            .state::<AppState>()
            .poll_active_and_get_remote_update(&mut cursor)
            .ok()
            .flatten()?;
        drop(cursor);
        let frame = match update {
            UiSnapshotUpdate::Full(snapshot) => EventFrame::SnapshotFull { snapshot },
            UiSnapshotUpdate::Patch(patch) => EventFrame::SnapshotPatch { patch },
        };
        Envelope::from_message(None, &Message::Event(frame)).ok()
    }
}

impl EventSource for AppUpdateEventSource {
    async fn next_event(&mut self) -> Option<Envelope> {
        loop {
            self.refresh_subscription();
            // Signal-driven wake with a fallback tick (same cadence as the
            // local bridge): a missed broadcast, a receiver bound to a
            // not-yet-open workspace, or a workspace switch must never wedge
            // the phone's stream. A timeout here leaves any late signal in
            // the channel for the next iteration to consume.
            let signal: Option<Result<AppUpdate, broadcast::error::RecvError>> =
                match self.rx.as_mut() {
                    Some(rx) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(EVENT_FALLBACK_WAKE_MS),
                            rx.recv(),
                        )
                        .await
                        {
                            Ok(result) => Some(result),
                            // Timed out without a signal.
                            Err(_elapsed) => None,
                        }
                    }
                    None => {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            EVENT_FALLBACK_WAKE_MS,
                        ))
                        .await;
                        None
                    }
                };
            match signal {
                // Timed out without a signal: poll the cursor anyway so a
                // missed wake still catches up.
                None => {
                    if let Some(envelope) = self.poll_delta() {
                        return Some(envelope);
                    }
                }
                Some(Ok(AppUpdate::PermissionRequested { request, .. })) => {
                    let frame = EventFrame::PermissionRequest { request };
                    return Envelope::from_message(None, &Message::Event(frame)).ok();
                }
                Some(Ok(AppUpdate::UiUpdated { .. })) => {
                    if let Some(envelope) = self.poll_delta() {
                        return Some(envelope);
                    }
                    // No delta available right now; keep draining signals.
                }
                // Missed signals are harmless: the cursor tracks revisions,
                // so the next poll produces the full delta since our revision.
                Some(Err(broadcast::error::RecvError::Lagged(_))) => {}
                // The Application the receiver was bound to is gone (workspace
                // closed). Drop it; refresh_subscription re-subscribes to the
                // new active workspace on the next loop iteration.
                Some(Err(broadcast::error::RecvError::Closed)) => {
                    self.rx = None;
                }
            }
        }
    }
}
