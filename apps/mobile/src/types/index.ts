// TypeScript mirror of the `workspace-model` DTOs that the phone consumes over
// the relay. Field names are snake_case to match the Rust serde wire format and
// the desktop frontend's existing `src/types/` (same reducer contract).

export type SessionStatus = "Idle" | "Streaming" | "WaitingForTool" | "Interrupted";
export type ToolStatus = "Pending" | "Running" | "Succeeded" | "Failed" | "Interrupted";
export type PatchStatus = "Proposed" | "Applied" | "Staged" | "Discarded";
export type ChangeSection = "Staged" | "Unstaged" | "Untracked";
export type MessageRole = "User" | "Assistant" | "System";
export type InspectorTab = "Activity" | "Diff" | "Files" | "Sources";
export type ThinkingStatus = "Active" | "Completed";
export type AgentCliId = "codebuddy" | "goose" | "codex-acp" | "claude-agent-acp";
export type FileChangeType = "Created" | "Modified" | "Deleted";

export type TimelineItem = { Message: string } | { Tool: string } | "Thinking";

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

export interface SessionSummary {
  id: string;
  workspace_id: string;
  title: string;
  model: string;
  mode: string | null;
  agent_cli: string | null;
  status: SessionStatus;
}

export type SessionConfigCategory = "Model" | "Mode" | "ThoughtLevel" | "Other";
export type SessionConfigSource = "ConfigOption" | "SessionModel" | "LegacyMode" | "LocalMode";
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
export type AgentPlanEntryStatus = "pending" | "in_progress" | "completed" | "cancelled";
export interface AgentPlanEntry {
  id?: string | null;
  content: string;
  priority: AgentPlanEntryPriority;
  status: AgentPlanEntryStatus;
}

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

export interface ChatMessage {
  id: string;
  role: MessageRole;
  body: string;
  created_at?: string;
  is_steer?: boolean;
}
export interface ChatMessageDelta {
  id: string;
  append: string;
}
export interface PendingSteer {
  message_id: string;
  body: string;
  created_at?: string;
}

export interface ToolLogEntry {
  title: string;
  body: string;
}
export interface TerminalOutput {
  exit_code: number | null;
  output: string;
}
export interface PermissionOption {
  id: string;
  label: string;
  kind: string;
}
export interface PermissionInputOption {
  label: string;
  description: string;
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
export interface PermissionInputRequest {
  questions: PermissionInputQuestion[];
}
export interface PermissionInputResponse {
  answers: Record<string, string[]>;
}
export interface DiffLine {
  kind: "Context" | "Added" | "Removed";
  content: string;
}
export interface DiffHunk {
  heading: string;
  lines: DiffLine[];
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
export interface SessionFileChange {
  path: string;
  change_type: FileChangeType;
  old_text: string | null;
  new_text: string;
  added_lines: number;
  removed_lines: number;
  timestamp: string;
}
export interface TurnFileChanges {
  message_id: string;
  changes: SessionFileChange[];
}
export interface UsageContextSnapshot {
  used_tokens?: number | null;
  window_tokens?: number | null;
  updated_at?: string | null;
}
export interface UsageTokenBreakdown {
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_read_tokens?: number | null;
  cache_write_tokens?: number | null;
  reasoning_tokens?: number | null;
  total_tokens?: number | null;
}
export interface UsageModelSummary {
  label: string;
  model?: string | null;
  provider?: string | null;
  agent_cli?: string | null;
  session_id?: string | null;
  workspace_root?: string | null;
  event_count: number;
  request_count: number;
  session_count: number;
  tokens: UsageTokenBreakdown;
  latest_at?: string | null;
}
export interface SessionUsageSnapshot {
  context: UsageContextSnapshot;
  current_turn: UsageTokenBreakdown;
  session_total: UsageTokenBreakdown;
  by_model: UsageModelSummary[];
}

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
  pending_steers?: PendingSteer[];
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
  pending_steers?: PendingSteer[];
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
export interface WorkspaceSessionList {
  workspace: WorkspaceDescriptor;
  sessions: SessionListItem[];
  active_session_id: string;
  is_active: boolean;
  connected: boolean;
}
