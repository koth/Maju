use serde::{Deserialize, Serialize};
use uuid::Uuid;
use workspace_model::{
    AgentCliId, AgentOptionsList, PermissionInputResponse, SessionConfigState, SessionFileChange,
    UiSnapshot, UserPromptContent, WorkspaceSessionList,
};

/// A control operation sent from the phone (or relay) to the PC gateway.
///
/// Internally tagged by `op` so each variant is self-describing on the wire.
/// Every variant carries the protocol `request_id` that the matching
/// [`ControlResponse`] echoes back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    ListSessions {
        request_id: Uuid,
    },
    CreateSession {
        request_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentCliId>,
        /// DeepSeek Harness agent preset (mode) for the new session. Only
        /// meaningful when `agent` is the harness; `None` = deployment
        /// default. Peers that predate the field omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    /// Fetch the agent/preset choices for the new-session picker (settings
    /// snapshot + best-effort harness preset list). Answered by the shell,
    /// not the active `Application`.
    ListAgentOptions {
        request_id: Uuid,
    },
    /// Set a session config control (e.g. the model picker) on the active
    /// session. Mirrors the local `session_set_config_control` command;
    /// answers with the refreshed config state.
    SetConfigControl {
        request_id: Uuid,
        control_id: String,
        value_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    SwitchSession {
        request_id: Uuid,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_root: Option<String>,
    },
    SendPrompt {
        request_id: Uuid,
        prompt: Vec<UserPromptContent>,
    },
    GetState {
        request_id: Uuid,
        /// The caller's held (active session id, revision). When both still
        /// match the PC's active state, the PC answers `up_to_date` instead of
        /// re-serializing the full snapshot — reconnect resyncs stop paying
        /// the whole-snapshot cost. Absent on first sync (always Full).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known_revision: Option<u64>,
    },
    ResolvePermission {
        request_id: Uuid,
        /// Domain permission-request id (the `request_id` string the
        /// Tauri `session_resolve_permission` command accepts).
        permission_request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guidance: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_response: Option<PermissionInputResponse>,
    },
    Cancel {
        request_id: Uuid,
    },
    StopTool {
        request_id: Uuid,
        tool_call_id: String,
    },
    /// Fetch the full file change (old/new text) for one file of one turn.
    /// The snapshot projection only carries turn-change METADATA (path +
    /// line counts) — the phone requests the diff on demand when the user
    /// taps a file in the turn-changes bar.
    GetFileDiff {
        request_id: Uuid,
        /// The turn that produced the change (TurnFileChanges.message_id).
        message_id: Uuid,
        /// File path exactly as listed in the turn-change metadata.
        path: String,
    },
}

/// The gateway's answer to a [`ControlRequest`], echoing `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlResponse {
    ListSessions {
        request_id: Uuid,
        sessions: Vec<WorkspaceSessionList>,
    },
    CreateSession {
        request_id: Uuid,
        session_id: String,
    },
    AgentOptions {
        request_id: Uuid,
        options: AgentOptionsList,
    },
    SetConfigControl {
        request_id: Uuid,
        config: SessionConfigState,
    },
    SwitchSession {
        request_id: Uuid,
    },
    SendPrompt {
        request_id: Uuid,
    },
    GetState {
        request_id: Uuid,
        /// `None` when `up_to_date` is true: the caller's held state is still
        /// current, so no snapshot is transferred. Always `Some` otherwise.
        /// Optional on the wire so peers that predate the short-circuit
        /// (which always send a full snapshot) still decode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<UiSnapshot>,
        /// True when the request carried `known_session_id`/`known_revision`
        /// matching the PC's active state — keep the held snapshot as-is.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to_date: bool,
    },
    ResolvePermission {
        request_id: Uuid,
    },
    Cancel {
        request_id: Uuid,
    },
    StopTool {
        request_id: Uuid,
    },
    /// The full file change for a GetFileDiff request. `change` is `None`
    /// when the turn/file is no longer resolvable on the desktop (e.g. the
    /// turn predates the desktop process and its live turn window).
    FileDiff {
        request_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<SessionFileChange>,
    },
    Error {
        request_id: Uuid,
        message: String,
    },
}

impl ControlRequest {
    pub fn request_id(&self) -> Uuid {
        match self {
            ControlRequest::ListSessions { request_id }
            | ControlRequest::CreateSession { request_id, .. }
            | ControlRequest::ListAgentOptions { request_id }
            | ControlRequest::SetConfigControl { request_id, .. }
            | ControlRequest::SwitchSession { request_id, .. }
            | ControlRequest::SendPrompt { request_id, .. }
            | ControlRequest::GetState {
                request_id,
                ..
            }
            | ControlRequest::ResolvePermission { request_id, .. }
            | ControlRequest::Cancel { request_id }
            | ControlRequest::StopTool { request_id, .. }
            | ControlRequest::GetFileDiff { request_id, .. } => *request_id,
        }
    }
}
