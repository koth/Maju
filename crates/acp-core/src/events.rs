use serde::{Deserialize, Serialize};
use workspace_model::{
    AgentPlanEntry, AvailableCommand, MessageRole, PermissionOption, PromptInputCapabilities,
    SessionConfigState, TerminalOutput,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfig {
    pub workspace_root: String,
    pub app_data_root: String,
    pub model: String,
    pub agent_command: String,
    /// ACP session ID from a previous session to resume via `--resume <id>`.
    /// When set, the agent command will have `--resume <id>` appended.
    pub resume_session_id: Option<String>,
    /// Unique identifier for this session's log file (timestamp-based).
    pub log_id: String,
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
        name: Option<String>,
        kind: Option<String>,
        summary: Option<String>,
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
    ToolPermissionRequest {
        id: String,
        name: String,
        options: Vec<PermissionOption>,
    },
    ToolPermissionResolved {
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
    TurnFinished {
        stop_reason: String,
    },
    Interrupted {
        reason: String,
    },
}
