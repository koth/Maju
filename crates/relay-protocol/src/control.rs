use serde::{Deserialize, Serialize};
use uuid::Uuid;
use workspace_model::{
    AgentCliId, PermissionInputResponse, UiSnapshot, UserPromptContent, WorkspaceSessionList,
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
    SwitchSession {
        request_id: Uuid,
    },
    SendPrompt {
        request_id: Uuid,
    },
    GetState {
        request_id: Uuid,
        snapshot: UiSnapshot,
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
            | ControlRequest::SwitchSession { request_id, .. }
            | ControlRequest::SendPrompt { request_id, .. }
            | ControlRequest::GetState { request_id }
            | ControlRequest::ResolvePermission { request_id, .. }
            | ControlRequest::Cancel { request_id }
            | ControlRequest::StopTool { request_id, .. } => *request_id,
        }
    }
}
