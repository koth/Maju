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
  | { Tool: string };

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
  | { type: "file"; data: string; mime_type: string | null; name: string };

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
}

export type FileChangeType = "Created" | "Modified" | "Deleted";

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
  filePath?: string;
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

export type AgentCliId = "codebuddy" | "opencode";

export interface AppSettings {
  selected_agent: AgentCliId;
  acp_port: number;
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
}

export interface AgentInstallResult {
  agent: AgentCliId;
  success: boolean;
  message: string;
  manual_instruction: string | null;
  snapshot: AgentSettingsSnapshot;
}
