export type SessionStatus = "Idle" | "Streaming" | "WaitingForTool" | "Interrupted";
export type ToolStatus = "Pending" | "Running" | "Succeeded" | "Failed" | "Interrupted";
export type PatchStatus = "Proposed" | "Applied" | "Staged" | "Discarded";
export type ChangeSection = "Staged" | "Unstaged" | "Untracked";
export type MessageRole = "User" | "Assistant" | "System";
export type InspectorTab = "Activity" | "Diff" | "Files" | "Sources";
export type SessionConfigCategory = "Model" | "Mode" | "ThoughtLevel" | "Other";
export type SessionConfigSource = "ConfigOption" | "SessionModel" | "LegacyMode" | "LocalMode";

export type TimelineItem =
  | { Message: string }
  | { Tool: string }
  | "Thinking";

export type ThinkingStatus = "Active" | "Completed";

export interface WorkspaceDescriptor {
  id: string;
  name: string;
  root: string;
}

export interface SessionSummary {
  id: string;
  workspace_id: string;
  title: string;
  model: string;
  mode: string | null;
  agent_cli: string | null;
  status: SessionStatus;
}

export interface SessionConfigChoice {
  id: string;
  label: string;
  description: string | null;
}

export interface SessionConfigControl {
  id: string;
  label: string;
  description: string | null;
  category: SessionConfigCategory;
  source: SessionConfigSource;
  current_value_id: string;
  current_value_label: string;
  choices: SessionConfigChoice[];
  enabled: boolean;
}

export interface SessionConfigState {
  hydrated: boolean;
  controls: SessionConfigControl[];
}

export type AgentPlanEntryPriority = "high" | "medium" | "low";
export type AgentPlanEntryStatus = "pending" | "in_progress" | "completed" | "cancelled";

export interface PromptInputCapabilities {
  image: boolean;
  embedded_context: boolean;
}

export interface AvailableCommand {
  name: string;
  description: string;
  input_hint: string | null;
}

export type UserPromptContent =
  | { type: "text"; text: string }
  | {
      type: "image";
      data: string;
      mime_type: string;
      name: string | null;
      thumbnail_data: string | null;
      thumbnail_mime_type: string | null;
    }
  | {
      type: "file";
      data?: string | null;
      text?: string | null;
      uri?: string | null;
      mime_type: string | null;
      name: string;
    };

export interface AgentPlanEntry {
  id?: string | null;
  content: string;
  priority: AgentPlanEntryPriority;
  status: AgentPlanEntryStatus;
}

export interface ChatMessage {
  id: string;
  role: MessageRole;
  body: string;
  created_at?: string;
}

export interface ChatMessageDelta {
  id: string;
  append: string;
}

export interface ToolLogEntry {
  title: string;
  body: string;
}

export interface TerminalOutput {
  exit_code: number | null;
  output: string;
}

export type TerminalSessionStatus = "running" | "exited";

export interface TerminalSession {
  terminal_id: string;
  workspace_root: string;
  cwd: string;
  shell: string;
  status: TerminalSessionStatus;
  exit_code: number | null;
  cols: number;
  rows: number;
}

export interface TerminalOpenRequest {
  workspace_root?: string | null;
  force_new?: boolean;
  cols: number;
  rows: number;
}

export interface TerminalWriteRequest {
  terminal_id: string;
  data: string;
}

export interface TerminalResizeRequest {
  terminal_id: string;
  cols: number;
  rows: number;
}

export interface TerminalIdRequest {
  terminal_id: string;
}

export interface TerminalOutputEvent {
  terminal_id: string;
  workspace_root: string;
  seq: number;
  data: string;
}

export interface TerminalStatusEvent {
  terminal_id: string;
  workspace_root: string;
  status: TerminalSessionStatus;
  cwd: string;
  shell: string;
  exit_code: number | null;
}

export interface TerminalExitEvent {
  terminal_id: string;
  workspace_root: string;
  exit_code: number | null;
}

export interface PermissionOption {
  id: string;
  label: string;
  kind: string;
}

export interface ToolDiffPreview {
  path: string;
  hunks: DiffHunk[];
}

export interface ToolInvocation {
  id: string;
  call_id: string;
  parent_call_id: string | null;
  name: string;
  kind: string;
  summary: string;
  status: ToolStatus;
  is_subagent: boolean;
  detail_text: string;
  logs: ToolLogEntry[];
  diff_paths: string[];
  diff_previews: ToolDiffPreview[];
  raw_input: string | null;
  raw_output: string | null;
  terminal_output: TerminalOutput | null;
  error: string | null;
  permission_options: PermissionOption[];
  permission_decision: string | null;
}

export interface DiffStats {
  added: number;
  removed: number;
}

export interface DiffLine {
  kind: "Context" | "Added" | "Removed";
  content: string;
}

export interface DiffHunk {
  heading: string;
  lines: DiffLine[];
}

export interface ChangedFile {
  path: string;
  section: ChangeSection;
  stats: DiffStats;
  patch_status: PatchStatus;
  hunks: DiffHunk[];
}

export interface RepositorySnapshot {
  branch: string;
  head: string;
  changed_files: ChangedFile[];
}

export interface SidebarSection {
  title: string;
  items: string[];
}

export interface UiSnapshot {
  revision: number;
  workspace: WorkspaceDescriptor;
  session: SessionSummary;
  session_config: SessionConfigState;
  prompt_capabilities: PromptInputCapabilities;
  available_commands: AvailableCommand[];
  agent_plan: AgentPlanEntry[];
  messages: ChatMessage[];
  timeline: TimelineItem[];
  tools: ToolInvocation[];
  repository: RepositorySnapshot;
  inspector_tab: InspectorTab;
  inspector_sections: SidebarSection[];
  session_changes: SessionFileChange[];
  review_changes: SessionFileChange[];
  turn_changes: TurnFileChanges[];
  thinking_status: ThinkingStatus | null;
}

export interface UiSnapshotPatch {
  revision: number;
  session: SessionSummary;
  session_config: SessionConfigState;
  prompt_capabilities: PromptInputCapabilities;
  available_commands: AvailableCommand[];
  agent_plan: AgentPlanEntry[];
  messages: ChatMessage[];
  message_deltas: ChatMessageDelta[];
  timeline_start: number;
  timeline: TimelineItem[];
  tools: ToolInvocation[];
  repository?: RepositorySnapshot | null;
  inspector_tab: InspectorTab;
  inspector_sections: SidebarSection[];
  session_changes: SessionFileChange[];
  review_changes: SessionFileChange[];
  turn_changes: TurnFileChanges[];
  thinking_status: ThinkingStatus | null;
}

export interface TurnFileChanges {
  message_id: string;
  changes: SessionFileChange[];
}

export interface RecentWorkspace {
  path: string;
  exists: boolean;
}

export interface SessionListItem {
  id: string;
  title: string;
  status: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  acp_session_id?: string | null;
  agent_cli?: string | null;
}

export interface OpenWorkspaceItem {
  workspace: WorkspaceDescriptor;
  active_session_id: string;
  session_count: number;
  is_active: boolean;
  connected: boolean;
}

export interface WorkspaceSessionList {
  workspace: WorkspaceDescriptor;
  sessions: SessionListItem[];
  active_session_id: string;
  is_active: boolean;
  connected: boolean;
}

export type FileChangeType = "Created" | "Modified" | "Deleted";
export type ChangeSetSource =
  | "AgentTurn"
  | "AgentConversation"
  | "ManualEdit"
  | "GitWorktree"
  | "ToolPreview";
export type ChangeSetStatus = "Pending" | "Complete" | "Live" | "LegacyIncomplete";
export type DiffQuality =
  | "Exact"
  | "LargeFileSkipped"
  | "BinarySkipped"
  | "MissingBaseline"
  | "FragmentRejected"
  | "LegacyIncomplete";

export interface ChangeSetSummary {
  id: string;
  source: ChangeSetSource;
  session_id: string | null;
  workspace_root: string;
  message_id: string | null;
  tool_call_id: string | null;
  owner_key: string | null;
  label: string;
  added_lines: number;
  removed_lines: number;
  file_count: number;
  updated_at: string;
  status: ChangeSetStatus;
}

export interface FileChangeSummary {
  change_set_id: string;
  path: string;
  change_type: FileChangeType;
  added_lines: number;
  removed_lines: number;
  quality: DiffQuality;
  updated_at: string;
}

export interface FileChangeRecord {
  change_set_id: string;
  path: string;
  change_type: FileChangeType;
  old_text: string | null;
  new_text: string | null;
  added_lines: number;
  removed_lines: number;
  quality: DiffQuality;
  updated_at: string;
}

export interface ListChangeSetsRequest {
  source?: ChangeSetSource | null;
  session_id?: string | null;
  workspace_root?: string | null;
}

export interface ListChangeSetFilesRequest {
  change_set_id: string;
}

export interface GetChangeSetFileDiffRequest {
  change_set_id: string;
  path: string;
}

export interface ChangeSetFilesResponse {
  change_set_id: string;
  files: FileChangeSummary[];
}

export interface SessionFileChange {
  path: string;
  change_type: FileChangeType;
  old_text: string | null;
  new_text: string;
  added_lines: number;
  removed_lines: number;
  timestamp: string;
}

export interface TabDescriptor {
  id: string;
  type: "conversation" | "changes" | "diff" | "editor";
  label: string;
  dirty?: boolean;
  filePath?: string;
  diffSource?: "session" | "git" | "change-set";
  changeSetId?: string;
  diffChange?: SessionFileChange;
  diffRecord?: FileChangeRecord;
  lineNumber?: number;
  searchQuery?: string;
  /** Incrementing counter to force navigation even when lineNumber is the same */
  navToken?: number;
}

export interface FileEntry {
  name: string;
  kind: "File" | "Directory";
  path: string;
}

export interface EditorFileVersion {
  content_hash: string;
  modified_ms: number | null;
  size: number;
}

export interface EditorFileSnapshot {
  path: string;
  content: string;
  version: EditorFileVersion;
  kind?: "text" | "image";
  mime_type?: string | null;
}

export interface LspServerStatus {
  languageId: string;
  configured: boolean;
  enabled: boolean;
  available: boolean;
  running: boolean;
  message: string | null;
}

export interface LspDiagnostic {
  path: string;
  message: string;
  severity: number;
  startLine: number;
  startCharacter: number;
  endLine: number;
  endCharacter: number;
}

// Search types

export interface SearchMatch {
  line_number: number;
  line_text: string;
}

export interface SearchFileResult {
  path: string;
  matches: SearchMatch[];
}

export interface SearchResult {
  query: string;
  files: SearchFileResult[];
  total_matches: number;
  truncated: boolean;
}

// App settings types

export type AgentCliId = "codebuddy" | "goose" | "codex-acp";
export type AppTheme = "kodex_dark" | "midnight" | "graphite" | "forest" | "light";
export type CodexConnectionMode = "managed" | "default";

export interface AppSettings {
  selected_agent: AgentCliId;
  acp_port: number;
  theme: AppTheme;
  lsp_servers: Record<string, LspServerSettings>;
  codex_connection_mode: CodexConnectionMode;
}

export interface LspServerSettings {
  enabled?: boolean | null;
  command?: string | null;
  args?: string[] | null;
}

export interface LspServerConfigInput {
  languageId: string;
  enabled: boolean;
  command: string;
  args: string[];
}

export interface LspProbeResult {
  available: boolean;
  resolvedPath: string | null;
  message: string | null;
}

export interface LspServerSettingsEntry {
  languageId: string;
  displayName: string;
  enabled: boolean;
  command: string;
  args: string[];
  defaultCommand: string;
  defaultArgs: string[];
  available: boolean;
  resolvedPath: string | null;
  running: boolean;
  message: string | null;
  customized: boolean;
}

export interface LspSettingsSnapshot {
  servers: LspServerSettingsEntry[];
}

export interface AgentCliStatus {
  id: AgentCliId;
  label: string;
  binary: string;
  installed: boolean;
  detected_path: string | null;
  selected: boolean;
}

export interface AgentSettingsSnapshot {
  settings: AppSettings;
  agents: AgentCliStatus[];
  env_override: string | null;
  codex_acp: CodexAcpSettingsStatus;
}

export interface CodexAcpSettingsStatus {
  provider: "default" | "venus" | "deepseek" | string;
  connection_mode: CodexConnectionMode;
  venus_key_configured: boolean;
  deepseek_key_configured: boolean;
  config_path: string;
}

export interface AgentInstallResult {
  agent: AgentCliId;
  success: boolean;
  message: string;
  manual_instruction: string | null;
  snapshot: AgentSettingsSnapshot;
}
