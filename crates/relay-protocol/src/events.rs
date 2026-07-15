use serde::{Deserialize, Serialize};
use workspace_model::{PermissionInputRequest, SessionStatus, ToolInvocation, UiSnapshot, UiSnapshotPatch};

/// An unsolicited push from the PC gateway to the phone. Event frames do
/// not carry a request/response `id`; they are streamed as the reducer
/// produces them.
///
/// Internally tagged by `kind` so each event is self-describing on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventFrame {
    /// Full snapshot sent first when a subscriber attaches.
    SnapshotFull {
        snapshot: UiSnapshot,
    },
    /// Incremental delta thereafter.
    SnapshotPatch {
        patch: UiSnapshotPatch,
    },
    /// A tool/agent is requesting permission; awaits a
    /// [`crate::ControlRequest::ResolvePermission`] reply.
    PermissionRequest {
        request: PermissionInputRequest,
    },
    /// A tool invocation was created or updated.
    ToolUpdated {
        tool: ToolInvocation,
    },
    /// The active session's status transitioned (idle/streaming/etc).
    SessionStatusChanged {
        session_id: String,
        status: SessionStatus,
    },
}
