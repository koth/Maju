//! Shell-side adapters that bridge `app_core` to `relay-client`'s driver
//! traits: `DesktopControlHandler` dispatches `ControlRequest` to
//! `DesktopRemoteControl` (impl `RemoteControl`), and
//! `AppUpdateEventSource` drains `Application::subscribe_updates` signals
//! and fetches Full/Patch deltas via `UiPatchCursor` +
//! `poll_active_and_get_update`, wrapping them into `EventFrame` envelopes
//! for the phone.

use app_core::{AppUpdate, RemoteControl, UiPatchCursor, UiSnapshotUpdate};
use relay_client::{ControlHandler, EventSource};
use relay_protocol::{ControlRequest, ControlResponse, Envelope, EventFrame, Message};
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;

use crate::remote_control::DesktopRemoteControl;
use crate::state::AppState;

/// Adapts `DesktopRemoteControl` to the driver's `ControlHandler` trait.
/// Each inbound `ControlRequest` is dispatched to the matching
/// `RemoteControl` method; the result is wrapped into the matching
/// `ControlResponse` (or `Error` on failure).
pub struct DesktopControlHandler {
    control: DesktopRemoteControl,
}

impl DesktopControlHandler {
    pub fn new(app: AppHandle) -> Self {
        Self {
            control: DesktopRemoteControl::new(app),
        }
    }
}

impl ControlHandler for DesktopControlHandler {
    async fn handle(&mut self, request: ControlRequest) -> ControlResponse {
        let request_id = request.request_id();
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
                ..
            } => self
                .control
                .create_session(workspace_root, agent)
                .await
                .map(|session_id| ControlResponse::CreateSession {
                    request_id,
                    session_id,
                }),
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
            ControlRequest::GetState { .. } => self
                .control
                .get_state()
                .await
                .map(|snapshot| ControlResponse::GetState {
                    request_id,
                    snapshot,
                }),
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
        };
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
/// Full/Patch delta via `poll_active_and_get_update` and wraps it into an
/// `EventFrame` envelope. `PermissionRequested` signals become
/// `EventFrame::PermissionRequest`. Ends (returns None) when the signal
/// stream lapses (relay reconnect re-subscribes).
pub struct AppUpdateEventSource {
    rx: broadcast::Receiver<AppUpdate>,
    cursor: UiPatchCursor,
    app: AppHandle,
}

impl AppUpdateEventSource {
    pub fn new(app: AppHandle) -> Self {
        let rx = app
            .state::<AppState>()
            .subscribe_active_updates()
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                let (_, rx) = broadcast::channel(1);
                rx
            });
        Self {
            rx,
            cursor: UiPatchCursor::default(),
            app,
        }
    }
}

impl EventSource for AppUpdateEventSource {
    async fn next_event(&mut self) -> Option<Envelope> {
        loop {
            match self.rx.recv().await {
                Ok(AppUpdate::PermissionRequested { request, .. }) => {
                    let frame = EventFrame::PermissionRequest { request };
                    return Envelope::from_message(None, &Message::Event(frame)).ok();
                }
                Ok(AppUpdate::UiUpdated { .. }) => {
                    // Fetch this subscriber's Full/Patch delta. A failure
                    // (no active workspace) just skips; we keep waiting.
                    if let Ok(Some(update)) = self
                        .app
                        .state::<AppState>()
                        .poll_active_and_get_update(&mut self.cursor)
                    {
                        let frame = match update {
                            UiSnapshotUpdate::Full(snapshot) => {
                                EventFrame::SnapshotFull { snapshot }
                            }
                            UiSnapshotUpdate::Patch(patch) => {
                                EventFrame::SnapshotPatch { patch }
                            }
                        };
                        return Envelope::from_message(None, &Message::Event(frame)).ok();
                    }
                    // No delta available right now; continue draining signals.
                    continue;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
