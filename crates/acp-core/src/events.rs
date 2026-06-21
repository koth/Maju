use agent_client_protocol::schema::McpServer;
use serde::{Deserialize, Serialize};
use workspace_model::{
    AgentPlanEntry, AvailableCommand, DiffHunk, MessageRole, PermissionInputRequest,
    PermissionOption, PromptInputCapabilities, SessionConfigState, TerminalOutput,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentEditPolicy {
    None,
    PreferApplyPatch,
}

impl Default for AgentEditPolicy {
    fn default() -> Self {
        Self::None
    }
}

pub fn agent_edit_policy_for_command(agent_command: &str) -> AgentEditPolicy {
    let normalized = agent_command.to_ascii_lowercase();
    let basename = normalized
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&normalized)
        .trim_matches(['"', '\'', '`']);

    if basename.contains("codex-acp") || basename.contains("kodex-acp") {
        AgentEditPolicy::PreferApplyPatch
    } else {
        AgentEditPolicy::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfig {
    pub workspace_root: String,
    pub app_data_root: String,
    pub model: String,
    pub agent_command: String,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub agent_env: Vec<(String, String)>,
    /// ACP session ID from a previous agent-side session.
    /// When set and the agent advertises load-session support, the runtime
    /// sends ACP `session/load`; otherwise it starts a fresh `session/new`.
    pub resume_session_id: Option<String>,
    /// Unique identifier for this session's log file (timestamp-based).
    pub log_id: String,
    /// TCP port for agents that use TCP transport.
    /// When set to 0, stdio transport is used.
    #[serde(default)]
    pub acp_port: u16,
    #[serde(default)]
    pub remote_ssh: Option<RemoteSshSessionConfig>,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSshSessionConfig {
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    pub remote_workspace_root: String,
    pub local_port: u16,
    pub remote_port: u16,
    #[serde(default)]
    pub reverse_forwards: Vec<RemoteSshReverseForward>,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub ssh_command: Option<String>,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub ssh_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSshReverseForward {
    pub remote_port: u16,
    pub local_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClientEvent {
    SessionStarted {
        session_id: String,
    },
    MessageChunk {
        role: MessageRole,
        content: String,
    },
    ContextCompactionStarted {
        message: String,
    },
    ContextCompacted {
        message: String,
    },
    ToolMessageChunk {
        id: String,
        content: String,
    },
    ToolStarted {
        id: String,
        parent_id: Option<String>,
        name: String,
        kind: String,
        summary: String,
        is_subagent: bool,
        raw_input: Option<String>,
    },
    ToolUpdated {
        id: String,
        parent_id: Option<String>,
        name: Option<String>,
        kind: Option<String>,
        summary: Option<String>,
        is_subagent: bool,
        raw_input: Option<String>,
        raw_output: Option<String>,
        terminal_output: Option<TerminalOutput>,
        is_partial: bool,
    },
    ToolProgress {
        id: String,
        content: String,
    },
    ToolCompleted {
        id: String,
        name: Option<String>,
        outcome: String,
        raw_output: Option<String>,
        terminal_output: Option<TerminalOutput>,
    },
    ToolFailed {
        id: String,
        name: Option<String>,
        error: String,
        raw_output: Option<String>,
        terminal_output: Option<TerminalOutput>,
    },
    ToolDiff {
        id: String,
        path: String,
        old_text: Option<String>,
        new_text: String,
    },
    ToolDiffPreview {
        id: String,
        path: String,
        hunks: Vec<DiffHunk>,
    },
    ToolPermissionRequest {
        id: String,
        name: String,
        options: Vec<PermissionOption>,
        details: Option<String>,
        input: Option<PermissionInputRequest>,
    },
    ToolPermissionResolved {
        id: String,
        outcome: String,
    },
    ToolStopAvailability {
        id: String,
        can_stop: bool,
        stop_kind: Option<String>,
    },
    ToolStopped {
        id: String,
        outcome: String,
    },
    SessionConfigUpdated {
        state: SessionConfigState,
    },
    PromptCapabilitiesUpdated {
        capabilities: PromptInputCapabilities,
    },
    AvailableCommandsUpdated {
        commands: Vec<AvailableCommand>,
    },
    SessionTitleUpdated {
        title: String,
    },
    SessionConfigValueChanged {
        control_id: String,
        value_id: String,
        value_label: Option<String>,
    },
    PlanUpdated {
        entries: Vec<AgentPlanEntry>,
    },
    ThinkingActivity {
        active: bool,
    },
    TurnFinished {
        stop_reason: String,
    },
    Interrupted {
        reason: String,
    },
}
