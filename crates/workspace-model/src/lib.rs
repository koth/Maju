use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Streaming,
    WaitingForTool,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatchStatus {
    Proposed,
    Applied,
    Staged,
    Discarded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeSection {
    Staged,
    Unstaged,
    Untracked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceDescriptor {
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub model: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<String>,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionConfigCategory {
    Model,
    Mode,
    ThoughtLevel,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionConfigSource {
    ConfigOption,
    SessionModel,
    LegacyMode,
    LocalMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfigChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfigControl {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub category: SessionConfigCategory,
    pub source: SessionConfigSource,
    pub current_value_id: String,
    pub current_value_label: String,
    pub choices: Vec<SessionConfigChoice>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfigState {
    pub hydrated: bool,
    pub controls: Vec<SessionConfigControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PromptInputCapabilities {
    pub image: bool,
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserPromptContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
        name: Option<String>,
        thumbnail_data: Option<String>,
        thumbnail_mime_type: Option<String>,
    },
    File {
        data: String,
        mime_type: Option<String>,
        name: String,
    },
}

impl UserPromptContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image(
        data: impl Into<String>,
        mime_type: impl Into<String>,
        name: Option<String>,
    ) -> Self {
        Self::Image {
            data: data.into(),
            mime_type: mime_type.into(),
            name,
            thumbnail_data: None,
            thumbnail_mime_type: None,
        }
    }

    pub fn image_with_thumbnail(
        data: impl Into<String>,
        mime_type: impl Into<String>,
        name: Option<String>,
        thumbnail_data: impl Into<String>,
        thumbnail_mime_type: impl Into<String>,
    ) -> Self {
        Self::Image {
            data: data.into(),
            mime_type: mime_type.into(),
            name,
            thumbnail_data: Some(thumbnail_data.into()),
            thumbnail_mime_type: Some(thumbnail_mime_type.into()),
        }
    }

    pub fn file(
        data: impl Into<String>,
        mime_type: Option<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::File {
            data: data.into(),
            mime_type,
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlanEntryPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlanEntryStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPlanEntry {
    #[serde(default)]
    pub id: Option<String>,
    pub content: String,
    pub priority: AgentPlanEntryPriority,
    pub status: AgentPlanEntryStatus,
}

impl Default for SessionConfigState {
    fn default() -> Self {
        Self {
            hydrated: false,
            controls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: Uuid,
    pub role: MessageRole,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimelineItem {
    Message(Uuid),
    Tool(Uuid),
    Thinking,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThinkingStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolInvocation {
    pub id: Uuid,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub summary: String,
    pub status: ToolStatus,
    pub is_subagent: bool,
    pub detail_text: String,
    pub logs: Vec<ToolLogEntry>,
    pub diff_paths: Vec<PathBuf>,
    #[serde(default)]
    pub diff_previews: Vec<ToolDiffPreview>,
    pub raw_input: Option<String>,
    pub raw_output: Option<String>,
    pub terminal_output: Option<TerminalOutput>,
    pub error: Option<String>,
    #[serde(default)]
    pub permission_options: Vec<PermissionOption>,
    #[serde(default)]
    pub permission_decision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDiffPreview {
    pub path: PathBuf,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalOutput {
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolLogEntry {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub heading: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub section: ChangeSection,
    pub stats: DiffStats,
    pub patch_status: PatchStatus,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub branch: String,
    pub head: String,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InspectorTab {
    Activity,
    Diff,
    Files,
    Sources,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidebarSection {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionListItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
    pub acp_session_id: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenWorkspaceItem {
    pub workspace: WorkspaceDescriptor,
    pub active_session_id: Uuid,
    pub session_count: usize,
    pub is_active: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSessionList {
    pub workspace: WorkspaceDescriptor,
    pub sessions: Vec<SessionListItem>,
    pub active_session_id: Uuid,
    pub is_active: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionFileChange {
    pub path: String,
    pub change_type: FileChangeType,
    pub old_text: Option<String>,
    pub new_text: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiSnapshot {
    pub workspace: WorkspaceDescriptor,
    pub session: SessionSummary,
    #[serde(default)]
    pub session_config: SessionConfigState,
    #[serde(default)]
    pub prompt_capabilities: PromptInputCapabilities,
    #[serde(default)]
    pub available_commands: Vec<AvailableCommand>,
    #[serde(default)]
    pub agent_plan: Vec<AgentPlanEntry>,
    pub messages: Vec<ChatMessage>,
    pub timeline: Vec<TimelineItem>,
    pub tools: Vec<ToolInvocation>,
    pub repository: RepositorySnapshot,
    pub inspector_tab: InspectorTab,
    pub inspector_sections: Vec<SidebarSection>,
    pub session_changes: Vec<SessionFileChange>,
    #[serde(default)]
    pub thinking_status: Option<ThinkingStatus>,
}

// ── Search types ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchMatch {
    pub line_number: u32,
    pub line_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchFileResult {
    pub path: String,
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub query: String,
    pub files: Vec<SearchFileResult>,
    pub total_matches: u32,
    pub truncated: bool,
}

// ── App settings types ──

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCliId {
    Codebuddy,
    Goose,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    KodexDark,
    Midnight,
    Graphite,
    Forest,
}

impl Default for AppTheme {
    fn default() -> Self {
        Self::KodexDark
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub selected_agent: AgentCliId,
    /// Reserved for ACP agents that may require a custom TCP transport.
    #[serde(default = "default_acp_port")]
    pub acp_port: u16,
    #[serde(default)]
    pub theme: AppTheme,
}

fn default_acp_port() -> u16 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCliStatus {
    pub id: AgentCliId,
    pub label: String,
    pub binary: String,
    pub installed: bool,
    pub detected_path: Option<PathBuf>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSettingsSnapshot {
    pub settings: AppSettings,
    pub agents: Vec<AgentCliStatus>,
    pub env_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInstallResult {
    pub agent: AgentCliId,
    pub success: bool,
    pub message: String,
    pub manual_instruction: Option<String>,
    pub snapshot: AgentSettingsSnapshot,
}
