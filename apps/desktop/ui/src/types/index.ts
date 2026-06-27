export type SessionStatus =
  | "Idle"
  | "Streaming"
  | "WaitingForTool"
  | "Interrupted";
export type ToolStatus =
  | "Pending"
  | "Running"
  | "Succeeded"
  | "Failed"
  | "Interrupted";
export type PatchStatus = "Proposed" | "Applied" | "Staged" | "Discarded";
export type ChangeSection = "Staged" | "Unstaged" | "Untracked";
export type MessageRole = "User" | "Assistant" | "System";
export type InspectorTab = "Activity" | "Diff" | "Files" | "Sources";
export type SessionConfigCategory = "Model" | "Mode" | "ThoughtLevel" | "Other";
export type SessionConfigSource =
  | "ConfigOption"
  | "SessionModel"
  | "LegacyMode"
  | "LocalMode";

export type TimelineItem = { Message: string } | { Tool: string } | "Thinking";

export type ThinkingStatus = "Active" | "Completed";

export interface WorkspaceDescriptor {
  id: string;
  name: string;
  root: string;
  location?: WorkspaceLocation;
}

export type WorkspaceLocation =
  | { kind: "local" }
  | {
      kind: "remote_linux";
      profile_id?: string | null;
      ssh_target: string;
      ssh_port?: number | null;
      remote_path: string;
      agent_cli?: AgentCliId | null;
      agent_command?: string | null;
      local_port?: number | null;
      remote_port?: number | null;
    };

export interface RemoteLinuxWorkspace {
  profile_id?: string | null;
  ssh_target: string;
  ssh_port?: number | null;
  remote_path: string;
  ssh_password?: string | null;
  agent_cli?: AgentCliId | null;
  agent_command?: string | null;
  local_port?: number | null;
  remote_port?: number | null;
}

export interface RemoteMachineProfile {
  id: string;
  display_name: string;
  ssh_target: string;
  ssh_port?: number | null;
  created_at_ms: number;
  updated_at_ms: number;
  last_validation?: RemoteMachineValidation | null;
}

export interface RemoteMachineProfileInput {
  id?: string | null;
  display_name: string;
  ssh_target: string;
  ssh_port?: number | null;
}

export interface RemoteMachineProfilesSnapshot {
  profiles: RemoteMachineProfile[];
}

export type RemoteValidationPhaseKind =
  | "ssh"
  | "remote_path"
  | "agent_command"
  | "acp_ready";
export type RemoteValidationPhaseStatus = "succeeded" | "failed" | "skipped";

export interface RemoteMachineValidationPhase {
  phase: RemoteValidationPhaseKind;
  status: RemoteValidationPhaseStatus;
  elapsed_ms: number;
  message?: string | null;
}

export interface RemoteMachineValidation {
  ok: boolean;
  checked_at_ms: number;
  remote_path?: string | null;
  phases: RemoteMachineValidationPhase[];
}

export interface RemoteMachineValidationRequest {
  profile_id: string;
  remote_path?: string | null;
  ssh_password?: string | null;
  agent_cli?: AgentCliId | null;
  include_acp?: boolean;
}

export interface RemoteOpenRequest {
  request_id?: string | null;
  profile_id: string;
  remote_path: string;
  ssh_password?: string | null;
  agent_cli: AgentCliId;
}

export type RemoteOpenPhaseKind =
  | "ssh"
  | "platform"
  | "remote_path"
  | "runtime_directory"
  | "agent_install"
  | "agent_verify"
  | "acp_launch";
export type RemoteOpenPhaseStatus =
  | "running"
  | "succeeded"
  | "failed"
  | "skipped";

export interface RemoteOpenProgressEvent {
  request_id: string;
  phase: RemoteOpenPhaseKind;
  status: RemoteOpenPhaseStatus;
  elapsed_ms: number;
  message?: string | null;
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
  provider: string | null;
  provider_label?: string | null;
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
export type AgentPlanEntryStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "cancelled";

export interface PromptInputCapabilities {
  image: boolean;
  embedded_context: boolean;
  session_steer: boolean;
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
      display_url?: string | null;
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
    }
  | {
      type: "workspace_file";
      path: string;
      start_line?: number | null;
      end_line?: number | null;
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

export interface TerminalScrollback {
  terminal_id: string;
  data: string;
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

export interface PermissionInputRequest {
  questions: PermissionInputQuestion[];
}

export interface PermissionInputQuestion {
  id: string;
  header: string;
  question: string;
  is_other: boolean;
  is_secret: boolean;
  multi_select: boolean;
  options: PermissionInputOption[];
}

export interface PermissionInputOption {
  label: string;
  description: string;
}

export interface PermissionInputResponse {
  answers: Record<string, string[]>;
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
  permission_input: PermissionInputRequest | null;
  permission_decision: string | null;
  can_stop: boolean;
  stop_kind: string | null;
  stop_status: string | null;
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

export type UsageEventScope =
  | "context_snapshot"
  | "turn_delta"
  | "session_total";
export type UsageSummaryGroupBy = "model" | "agent" | "workspace" | "session";

export interface UsageTokenBreakdown {
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_read_tokens?: number | null;
  cache_write_tokens?: number | null;
  reasoning_tokens?: number | null;
  total_tokens?: number | null;
}

export interface UsageContextSnapshot {
  used_tokens?: number | null;
  window_tokens?: number | null;
  updated_at?: string | null;
}

export interface UsageModelSummary {
  label: string;
  model?: string | null;
  provider?: string | null;
  agent_cli?: string | null;
  session_id?: string | null;
  workspace_root?: string | null;
  event_count: number;
  session_count: number;
  tokens: UsageTokenBreakdown;
  context_peak_tokens?: number | null;
  latest_at?: string | null;
}

export interface SessionUsageSnapshot {
  context: UsageContextSnapshot;
  current_turn: UsageTokenBreakdown;
  session_total: UsageTokenBreakdown;
  by_model: UsageModelSummary[];
}

export interface UsageSummaryRequest {
  workspace_root?: string | null;
  session_id?: string | null;
  from?: string | null;
  to?: string | null;
  all_workspaces?: boolean;
  include_archived?: boolean;
  group_by?: UsageSummaryGroupBy;
}

export type UsageSummaryRow = UsageModelSummary;

export interface UiSnapshot {
  revision: number;
  workspace: WorkspaceDescriptor;
  workspace_connected?: boolean;
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
  usage?: SessionUsageSnapshot;
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
  usage?: SessionUsageSnapshot;
}

export interface TurnFileChanges {
  message_id: string;
  changes: SessionFileChange[];
}

export interface RecentWorkspace {
  path: string;
  exists: boolean;
  remote?: RemoteLinuxWorkspace | null;
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
  runtime_status?: "none" | "active" | "background_running" | "background_idle";
  attention_state?: "none" | "completed_unviewed" | "needs_attention";
}

export interface ArchivedSessionListItem {
  id: string;
  title: string;
  status: string;
  created_at: string;
  updated_at: string;
  archived_at: string;
  message_count: number;
  acp_session_id?: string | null;
  agent_cli?: string | null;
  workspace_root: string;
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
export type ChangeSetStatus =
  | "Pending"
  | "Complete"
  | "Live"
  | "LegacyIncomplete";
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
  ephemeral?: boolean;
  filePath?: string;
  diffSource?: "session" | "git" | "change-set";
  changeSetId?: string;
  diffChange?: SessionFileChange;
  diffRecord?: FileChangeRecord;
  lineNumber?: number;
  searchQuery?: string;
  /** Incrementing counter to force navigation even when lineNumber is the same */
  navToken?: number;
  /** Whether the user has interacted with this editor (scrolled, typed, etc.) */
  hasUserInteraction?: boolean;
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

export interface SearchFileSuggestion {
  path: string;
  name: string;
}

export interface SearchNotice {
  message: string;
  url?: string | null;
  url_label?: string | null;
}

export interface SearchResult {
  query: string;
  file_suggestions: SearchFileSuggestion[];
  files: SearchFileResult[];
  total_matches: number;
  truncated: boolean;
  notice?: SearchNotice | null;
}

// App settings types

export type AgentCliId =
  | "codebuddy"
  | "goose"
  | "codex-acp"
  | "claude-agent-acp";
export type AppTheme =
  | "kodex_dark"
  | "midnight"
  | "graphite"
  | "forest"
  | "light";
export type CodexConnectionMode = "managed" | "default";
export type AgentProviderFamily = "codex" | "claude";
export type AgentProviderProxyKind =
  | "codex_default"
  | "responses"
  | "completion_to_responses"
  | "claude_native"
  | "completion_to_claude";
export type CustomProviderProtocol =
  | "chat_completions"
  | "responses"
  | "anthropic_messages";

export interface CustomProviderInput {
  providerId?: string | null;
  label: string;
  endpoint: string;
  protocol: CustomProviderProtocol;
  apiKey: string;
  modelListUrl?: string | null;
}

export interface AgentProviderProfile {
  family: AgentProviderFamily;
  id: string;
  label: string;
  proxy_kind: AgentProviderProxyKind;
  selected: boolean;
  configured: boolean;
  base_url: string | null;
  hidden?: boolean;
  custom: boolean;
  protocol: CustomProviderProtocol | null;
  default_model: string | null;
  models: string[];
  model_list_url: string | null;
  credential_label: string | null;
  requires_credential: boolean;
  help_text: string;
}

export interface AgentModelOption {
  id: string;
  label: string;
  provider_id: string;
  provider_label: string;
}

export interface AppSettings {
  selected_agent: AgentCliId;
  acp_port: number;
  theme: AppTheme;
  lsp_servers: Record<string, LspServerSettings>;
  codex_connection_mode: CodexConnectionMode;
  selected_codex_provider_profile_id: string | null;
  selected_claude_provider_profile_id: string | null;
  claude: ClaudeProviderSettings;
  web_tools: WebToolsSettings;
}

export interface ClaudeProviderSettings {
  available_models: string[];
  fast_model: string | null;
}

export interface WebToolsSettings {
  enabled: boolean;
  provider: "brave" | "tavily" | string;
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
  claude: ClaudeProviderSettingsStatus;
  web_tools: WebToolsSettingsStatus;
  image?: ImageSettingsStatus;
}

export interface CodexAcpSettingsStatus {
  provider: "default" | "byok" | "deepseek" | string;
  selected_profile_id: string;
  profiles: AgentProviderProfile[];
  connection_mode: CodexConnectionMode;
  deepseek_key_configured: boolean;
  config_path: string;
}

export interface ClaudeProviderSettingsStatus {
  selected_profile_id: string;
  profiles: AgentProviderProfile[];
  fast_model: string | null;
  fast_model_options: AgentModelOption[];
}

export interface WebToolsSettingsStatus {
  enabled: boolean;
  provider: "brave" | "tavily" | string;
  configured: boolean;
}

export type ImageGenerateProtocol =
  | "openai_images"
  | "chat_completions"
  | "gemini";

export interface ImageSettingsStatus {
  enabled: boolean;
  view_provider: string;
  view_model: string;
  view_configured: boolean;
  view_models: string[];
  generate_protocol: ImageGenerateProtocol;
  generate_model: string;
  generate_base_url: string;
  generate_default_size: string;
  generate_configured: boolean;
}

export interface AgentInstallResult {
  agent: AgentCliId;
  success: boolean;
  message: string;
  manual_instruction: string | null;
  snapshot: AgentSettingsSnapshot;
}
