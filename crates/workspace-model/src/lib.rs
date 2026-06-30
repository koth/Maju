use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    #[serde(default)]
    pub location: WorkspaceLocation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceLocation {
    Local,
    RemoteLinux(RemoteLinuxWorkspace),
}

impl Default for WorkspaceLocation {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteLinuxWorkspace {
    #[serde(default)]
    pub profile_id: Option<Uuid>,
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    pub remote_path: String,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub ssh_password: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<AgentCliId>,
    #[serde(default)]
    pub agent_command: Option<String>,
    #[serde(default)]
    pub local_port: Option<u16>,
    #[serde(default)]
    pub remote_port: Option<u16>,
}

impl RemoteLinuxWorkspace {
    pub fn key(&self) -> String {
        let port = self
            .ssh_port
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        format!(
            "ssh://{}{}{}",
            self.ssh_target.trim(),
            port,
            normalize_remote_path_for_key(&self.remote_path)
        )
    }

    pub fn display_name(&self) -> String {
        self.remote_path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("远程工作区")
            .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMachineProfile {
    pub id: Uuid,
    pub display_name: String,
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub last_validation: Option<RemoteMachineValidation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMachineProfileInput {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub display_name: String,
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RemoteMachineProfilesSnapshot {
    #[serde(default)]
    pub profiles: Vec<RemoteMachineProfile>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteValidationPhaseKind {
    Ssh,
    RemotePath,
    AgentCommand,
    AcpReady,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteValidationPhaseStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMachineValidationPhase {
    pub phase: RemoteValidationPhaseKind,
    pub status: RemoteValidationPhaseStatus,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMachineValidation {
    pub ok: bool,
    pub checked_at_ms: u64,
    #[serde(default)]
    pub remote_path: Option<String>,
    #[serde(default)]
    pub phases: Vec<RemoteMachineValidationPhase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMachineValidationRequest {
    pub profile_id: Uuid,
    #[serde(default)]
    pub remote_path: Option<String>,
    #[serde(default)]
    pub ssh_password: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<AgentCliId>,
    #[serde(default)]
    pub include_acp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteOpenRequest {
    #[serde(default)]
    pub request_id: Option<Uuid>,
    pub profile_id: Uuid,
    pub remote_path: String,
    #[serde(default, skip_serializing)]
    pub ssh_password: Option<String>,
    pub agent_cli: AgentCliId,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOpenPhaseKind {
    Ssh,
    Platform,
    RemotePath,
    RuntimeDirectory,
    AgentInstall,
    AgentVerify,
    AcpLaunch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOpenPhaseStatus {
    Running,
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteOpenProgressEvent {
    pub request_id: Uuid,
    pub phase: RemoteOpenPhaseKind,
    pub status: RemoteOpenPhaseStatus,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub message: Option<String>,
}

fn normalize_remote_path_for_key(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
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
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_label: Option<String>,
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
    #[serde(default)]
    pub session_steer: bool,
}

/// Native image capabilities resolved for a session's active model/provider.
///
/// Pure DTO: `app-core` resolves these from model/provider/agent signals, and
/// `acp-core` consumes them on `SessionConfig` to override prompt capabilities.
/// `native_edit` is always `false` because Kodex has no native image-editing
/// capability; editing is always delivered through the image MCP `edit_image`
/// tool.
///
/// `view_fallback` is `true` when the `kodex-image` MCP server is attached and
/// offers an image-understanding fallback (`view_image`). It is independent of
/// `native_view`: a text-only model with `view_fallback == true` still allows
/// image attachments — the prompt is degraded through `view_image` before
/// reaching the model. The prompt-capability gate is therefore
/// `native_view || view_fallback`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageCapabilities {
    /// Whether the active model can natively understand image input.
    pub native_view: bool,
    /// Whether the current provider can natively generate images
    /// (codex-acp default provider only).
    pub native_generate: bool,
    /// Whether the active model/provider can natively edit images.
    /// Always `false`; editing is always MCP-backed.
    pub native_edit: bool,
    /// Whether an image-understanding fallback (`view_image`) is available.
    /// `true` iff the `kodex-image` MCP server is attached for this session.
    pub view_fallback: bool,
}

impl ImageCapabilities {
    /// Returns `true` when every native capability is present and no image
    /// MCP fallback tool is required.
    pub fn all_native(&self) -> bool {
        self.native_view && self.native_generate && self.native_edit
    }

    /// The prompt-capability gate: image attachments are allowed when the
    /// model natively understands images or a `view_image` fallback is attached.
    pub fn image_capable(&self) -> bool {
        self.native_view || self.view_fallback
    }

 /// The safe default: assume every native capability is present so that no
    /// fallback override fires when image fallback is inactive or a session's
    /// capabilities have not been resolved yet. (`native_edit` stays `false`
    /// because there is no native editing path, but that only matters once a
    /// session has resolved real capabilities and an image MCP is attached.)
    /// `view_fallback` defaults to `false`; it flips to `true` once the
    /// `kodex-image` MCP server is actually attached.
    pub fn assumed_native() -> Self {
        Self {
            native_view: true,
            native_generate: true,
            native_edit: false,
            view_fallback: false,
        }
    }
}

impl Default for ImageCapabilities {
    fn default() -> Self {
        Self::assumed_native()
    }
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
        #[serde(default)]
        display_url: Option<String>,
        thumbnail_data: Option<String>,
        thumbnail_mime_type: Option<String>,
    },
    File {
        #[serde(default)]
        data: Option<String>,
        #[serde(default)]
        text: Option<String>,
        mime_type: Option<String>,
        name: String,
        #[serde(default)]
        uri: Option<String>,
    },
    WorkspaceFile {
        path: String,
        #[serde(default)]
        start_line: Option<u32>,
        #[serde(default)]
        end_line: Option<u32>,
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
            display_url: None,
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
            display_url: None,
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
            data: Some(data.into()),
            text: None,
            mime_type,
            name: name.into(),
            uri: None,
        }
    }

    pub fn text_file(
        text: impl Into<String>,
        mime_type: Option<String>,
        name: impl Into<String>,
        uri: impl Into<String>,
    ) -> Self {
        Self::File {
            data: None,
            text: Some(text.into()),
            mime_type,
            name: name.into(),
            uri: Some(uri.into()),
        }
    }

    pub fn workspace_file(
        path: impl Into<String>,
        start_line: Option<u32>,
        end_line: Option<u32>,
    ) -> Self {
        Self::WorkspaceFile {
            path: path.into(),
            start_line,
            end_line,
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
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessageDelta {
    pub id: Uuid,
    pub append: String,
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
    pub permission_input: Option<PermissionInputRequest>,
    #[serde(default)]
    pub permission_decision: Option<String>,
    #[serde(default)]
    pub can_stop: bool,
    #[serde(default)]
    pub stop_kind: Option<String>,
    #[serde(default)]
    pub stop_status: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PermissionInputRequest {
    #[serde(default)]
    pub questions: Vec<PermissionInputQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub options: Vec<PermissionInputOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PermissionInputResponse {
    #[serde(default)]
    pub answers: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalOutput {
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalSession {
    pub terminal_id: String,
    pub workspace_root: String,
    pub cwd: String,
    pub shell: String,
    pub status: TerminalSessionStatus,
    pub exit_code: Option<i32>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalOpenRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub force_new: bool,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWriteRequest {
    pub terminal_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalResizeRequest {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalIdRequest {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalScrollback {
    pub terminal_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalOutputEvent {
    pub terminal_id: String,
    pub workspace_root: String,
    pub seq: u64,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalStatusEvent {
    pub terminal_id: String,
    pub workspace_root: String,
    pub status: TerminalSessionStatus,
    pub cwd: String,
    pub shell: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalExitEvent {
    pub terminal_id: String,
    pub workspace_root: String,
    pub exit_code: Option<i32>,
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
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeStatus {
    None,
    Active,
    BackgroundRunning,
    BackgroundIdle,
}

impl Default for SessionRuntimeStatus {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttentionState {
    None,
    CompletedUnviewed,
    NeedsAttention,
}

impl Default for SessionAttentionState {
    fn default() -> Self {
        Self::None
    }
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
    #[serde(default)]
    pub runtime_status: SessionRuntimeStatus,
    #[serde(default)]
    pub attention_state: SessionAttentionState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivedSessionListItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: String,
    pub message_count: i64,
    pub acp_session_id: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<String>,
    pub workspace_root: String,
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
pub enum FileEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub kind: FileEntryKind,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorFileVersion {
    pub content_hash: String,
    pub modified_ms: Option<u128>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorFileKind {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorFileSnapshot {
    pub path: String,
    pub content: String,
    pub version: EditorFileVersion,
    pub kind: EditorFileKind,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeSetSource {
    AgentTurn,
    AgentConversation,
    ManualEdit,
    GitWorktree,
    ToolPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeSetStatus {
    Pending,
    Complete,
    Live,
    LegacyIncomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffQuality {
    Exact,
    LargeFileSkipped,
    BinarySkipped,
    MissingBaseline,
    FragmentRejected,
    LegacyIncomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSetSummary {
    pub id: String,
    pub source: ChangeSetSource,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    pub workspace_root: String,
    #[serde(default)]
    pub message_id: Option<Uuid>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub owner_key: Option<String>,
    pub label: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub file_count: usize,
    pub updated_at: String,
    pub status: ChangeSetStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChangeSummary {
    pub change_set_id: String,
    pub path: String,
    pub change_type: FileChangeType,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub quality: DiffQuality,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChangeRecord {
    pub change_set_id: String,
    pub path: String,
    pub change_type: FileChangeType,
    #[serde(default)]
    pub old_text: Option<String>,
    #[serde(default)]
    pub new_text: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub quality: DiffQuality,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ListChangeSetsRequest {
    #[serde(default)]
    pub source: Option<ChangeSetSource>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListChangeSetFilesRequest {
    pub change_set_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetChangeSetFileDiffRequest {
    pub change_set_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSetFilesResponse {
    pub change_set_id: String,
    pub files: Vec<FileChangeSummary>,
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
pub struct TurnFileChanges {
    pub message_id: Uuid,
    pub changes: Vec<SessionFileChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageEventScope {
    ContextSnapshot,
    TurnDelta,
    SessionTotal,
}

impl Default for UsageEventScope {
    fn default() -> Self {
        Self::ContextSnapshot
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageTokenBreakdown {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageContextSnapshot {
    #[serde(default)]
    pub used_tokens: Option<u64>,
    #[serde(default)]
    pub window_tokens: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageEvent {
    #[serde(default)]
    pub scope: UsageEventScope,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<String>,
    #[serde(default)]
    pub tokens: UsageTokenBreakdown,
    #[serde(default)]
    pub context: UsageContextSnapshot,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub raw_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageModelSummary {
    pub label: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub agent_cli: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub event_count: u64,
    #[serde(default)]
    pub session_count: u64,
    #[serde(default)]
    pub tokens: UsageTokenBreakdown,
    #[serde(default)]
    pub context_peak_tokens: Option<u64>,
    #[serde(default)]
    pub latest_at: Option<String>,
    /// True once a `SessionTotal` event has been observed for this group.
    /// Used by the aggregation layer to avoid double-counting TurnDelta
    /// events on top of an authoritative SessionTotal.
    /// Not serialized to the frontend; this is internal aggregation state.
    #[serde(default, skip)]
    pub has_session_total: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionUsageSnapshot {
    #[serde(default)]
    pub context: UsageContextSnapshot,
    #[serde(default)]
    pub current_turn: UsageTokenBreakdown,
    #[serde(default)]
    pub session_total: UsageTokenBreakdown,
    #[serde(default)]
    pub by_model: Vec<UsageModelSummary>,
    /// True once a `SessionTotal` event has been observed for this session.
    /// When false, `session_total` may be derived from accumulated `TurnDelta`
    /// events. When true, `SessionTotal` is authoritative and `TurnDelta`
    /// events must not add to `session_total` (they are already included).
    /// Not serialized to the frontend; this is internal reducer state.
    #[serde(default, skip)]
    pub has_session_total: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageSummaryGroupBy {
    Model,
    Agent,
    Workspace,
    Session,
}

impl Default for UsageSummaryGroupBy {
    fn default() -> Self {
        Self::Model
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UsageSummaryRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub all_workspaces: bool,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub group_by: UsageSummaryGroupBy,
}

pub type UsageSummaryRow = UsageModelSummary;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiSnapshot {
    #[serde(default)]
    pub revision: u64,
    pub workspace: WorkspaceDescriptor,
    #[serde(default = "default_true")]
    pub workspace_connected: bool,
    pub session: SessionSummary,
    #[serde(default)]
    pub session_config: SessionConfigState,
    #[serde(default)]
    pub prompt_capabilities: PromptInputCapabilities,
    /// Resolved native image capabilities for the session's active
    /// model/provider. The reducer uses `native_view` to override
    /// `prompt_capabilities.image` for text-only models.
    #[serde(default)]
    pub image_capabilities: ImageCapabilities,
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
    pub review_changes: Vec<SessionFileChange>,
    #[serde(default)]
    pub turn_changes: Vec<TurnFileChanges>,
    #[serde(default)]
    pub thinking_status: Option<ThinkingStatus>,
    #[serde(default)]
    pub usage: SessionUsageSnapshot,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiSnapshotPatch {
    pub revision: u64,
    pub session: SessionSummary,
    #[serde(default)]
    pub session_config: SessionConfigState,
    #[serde(default)]
    pub prompt_capabilities: PromptInputCapabilities,
    #[serde(default)]
    pub available_commands: Vec<AvailableCommand>,
    #[serde(default)]
    pub agent_plan: Vec<AgentPlanEntry>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub message_deltas: Vec<ChatMessageDelta>,
    pub timeline_start: usize,
    #[serde(default)]
    pub timeline: Vec<TimelineItem>,
    #[serde(default)]
    pub tools: Vec<ToolInvocation>,
    #[serde(default)]
    pub repository: Option<RepositorySnapshot>,
    pub inspector_tab: InspectorTab,
    #[serde(default)]
    pub inspector_sections: Vec<SidebarSection>,
    #[serde(default)]
    pub session_changes: Vec<SessionFileChange>,
    #[serde(default)]
    pub review_changes: Vec<SessionFileChange>,
    #[serde(default)]
    pub turn_changes: Vec<TurnFileChanges>,
    #[serde(default)]
    pub thinking_status: Option<ThinkingStatus>,
    #[serde(default)]
    pub usage: SessionUsageSnapshot,
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
pub struct SearchFileSuggestion {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchNotice {
    pub message: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub url_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub query: String,
    #[serde(default)]
    pub file_suggestions: Vec<SearchFileSuggestion>,
    pub files: Vec<SearchFileResult>,
    pub total_matches: u32,
    pub truncated: bool,
    #[serde(default)]
    pub notice: Option<SearchNotice>,
}

// ── App settings types ──

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCliId {
    Codebuddy,
    Goose,
    #[serde(rename = "codex-acp")]
    CodexAcp,
    #[serde(rename = "claude-agent-acp")]
    ClaudeAgentAcp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    KodexDark,
    Midnight,
    Graphite,
    Forest,
    Light,
}

impl Default for AppTheme {
    fn default() -> Self {
        Self::Graphite
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexConnectionMode {
    Managed,
    Default,
}

impl Default for CodexConnectionMode {
    fn default() -> Self {
        Self::Managed
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderFamily {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderProxyKind {
    CodexDefault,
    Responses,
    CompletionToResponses,
    ClaudeNative,
    CompletionToClaude,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CustomProviderProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomProviderInput {
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub endpoint: String,
    pub protocol: CustomProviderProtocol,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model_list_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProviderProfile {
    pub family: AgentProviderFamily,
    pub id: String,
    pub label: String,
    pub proxy_kind: AgentProviderProxyKind,
    pub selected: bool,
    pub configured: bool,
    pub base_url: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub custom: bool,
    #[serde(default)]
    pub protocol: Option<CustomProviderProtocol>,
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub model_list_url: Option<String>,
    pub credential_label: Option<String>,
    pub requires_credential: bool,
    pub help_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentModelOption {
    pub id: String,
    pub label: String,
    pub provider_id: String,
    pub provider_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ClaudeProviderSettings {
    #[serde(default)]
    pub available_models: Vec<String>,
    #[serde(default)]
    pub fast_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebToolsSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_tools_provider")]
    pub provider: String,
}

impl Default for WebToolsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_web_tools_provider(),
        }
    }
}

fn default_web_tools_provider() -> String {
    "brave".to_string()
}

/// Image-understanding fallback configuration. `view` selects a multimodal
/// model from the existing model catalog used by the `view_image` tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageViewSettings {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

impl Default for ImageViewSettings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
        }
    }
}

/// Image generation/edit configuration. `generate_image` and `edit_image`
/// share this one independently-configured generation model (e.g.
/// `nana-banana`), which is not part of the text-model catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageGenerateSettings {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default = "default_image_generate_protocol")]
    pub protocol: ImageGenerateProtocol,
    #[serde(default = "default_image_generate_size")]
    pub default_size: String,
}

impl Default for ImageGenerateSettings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            base_url: String::new(),
            api_key_env: String::new(),
            protocol: default_image_generate_protocol(),
            default_size: default_image_generate_size(),
        }
    }
}

/// Wire protocol spoken by the independently-configured image generation
/// model. `generate_image` and `edit_image` dispatch on this.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerateProtocol {
    /// OpenAI-compatible `POST /images/generations` + `POST /images/edits`.
    #[default]
    OpenaiImages,
    /// OpenAI-compatible `POST /chat/completions` where the model returns
    /// image output inline (e.g. multimodal generation models that emit
    /// `image_url` content parts).
    ChatCompletions,
    /// Google Gemini `POST /models/{model}:generateContent` with
    /// `responseModalities` including `IMAGE`.
    Gemini,
}

impl ImageGenerateProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiImages => "openai_images",
            Self::ChatCompletions => "chat_completions",
            Self::Gemini => "gemini",
        }
    }
}

fn default_image_generate_protocol() -> ImageGenerateProtocol {
    ImageGenerateProtocol::default()
}

fn default_image_generate_size() -> String {
    "1024x1024".to_string()
}

/// Unified image capability fallback settings. When `enabled`/`auto_enable`
/// are true and a native image capability is missing, the `kodex-image` MCP
/// server is injected with the corresponding fallback tool(s).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub view: ImageViewSettings,
    #[serde(default)]
    pub generate: ImageGenerateSettings,
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            view: ImageViewSettings::default(),
            generate: ImageGenerateSettings::default(),
        }
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
    #[serde(default)]
    pub lsp_servers: BTreeMap<String, LspServerSettings>,
    #[serde(default)]
    pub codex_connection_mode: CodexConnectionMode,
    #[serde(default)]
    pub selected_codex_provider_profile_id: Option<String>,
    #[serde(default)]
    pub selected_claude_provider_profile_id: Option<String>,
    #[serde(default)]
    pub claude: ClaudeProviderSettings,
    #[serde(default)]
    pub web_tools: WebToolsSettings,
    #[serde(default)]
    pub image: ImageSettings,
}

fn default_acp_port() -> u16 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LspServerSettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspServerConfigInput {
    pub language_id: String,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspProbeResult {
    pub available: bool,
    pub resolved_path: Option<PathBuf>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspServerSettingsEntry {
    pub language_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub default_command: String,
    pub default_args: Vec<String>,
    pub available: bool,
    pub resolved_path: Option<PathBuf>,
    pub running: bool,
    pub message: Option<String>,
    pub customized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspSettingsSnapshot {
    pub servers: Vec<LspServerSettingsEntry>,
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
    pub codex_acp: CodexAcpSettingsStatus,
    pub claude: ClaudeProviderSettingsStatus,
    pub web_tools: WebToolsSettingsStatus,
    #[serde(default)]
    pub image: ImageSettingsStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexAcpSettingsStatus {
    pub provider: String,
    pub selected_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<AgentProviderProfile>,
    pub connection_mode: CodexConnectionMode,
    pub deepseek_key_configured: bool,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeProviderSettingsStatus {
    pub selected_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<AgentProviderProfile>,
    #[serde(default)]
    pub fast_model: Option<String>,
    #[serde(default)]
    pub fast_model_options: Vec<AgentModelOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebToolsSettingsStatus {
    pub enabled: bool,
    pub provider: String,
    pub configured: bool,
}

/// UI-facing image capability fallback status. `view_models` lists catalog
/// models the user can pick for `view_image` (filtered to image-capable
/// providers); `generate_configured` reports whether the generation model has
/// a key resolved.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageSettingsStatus {
    pub enabled: bool,
    pub view_provider: String,
    pub view_model: String,
    pub view_configured: bool,
    /// Catalog models available for the configured view provider, so the UI
    /// picker can list them. Each entry is a plain model id.
    pub view_models: Vec<String>,
    pub generate_protocol: ImageGenerateProtocol,
    pub generate_model: String,
    pub generate_base_url: String,
    pub generate_default_size: String,
    pub generate_configured: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInstallResult {
    pub agent: AgentCliId,
    pub success: bool,
    pub message: String,
    pub manual_instruction: Option<String>,
    pub snapshot: AgentSettingsSnapshot,
}
