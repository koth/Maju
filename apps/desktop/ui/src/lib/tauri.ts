import { invoke } from "@tauri-apps/api/core";
import type { UiSnapshot, RepositorySnapshot, ChangedFile, RecentWorkspace, SessionListItem, SessionFileChange, FileEntry, SessionConfigState, UserPromptContent, SearchResult, AgentCliId, AgentSettingsSnapshot, AgentInstallResult } from "../types";

export async function sessionGetState(): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("session_get_state");
}

export async function sessionSendPrompt(prompt: UserPromptContent[]): Promise<void> {
  return invoke("session_send_prompt", { prompt });
}

export async function sessionSetConfigControl(controlId: string, valueId: string): Promise<SessionConfigState> {
  return invoke<SessionConfigState>("session_set_config_control", { controlId, valueId });
}

export async function sessionResolvePermission(requestId: string, optionId: string | null): Promise<void> {
  return invoke("session_resolve_permission", { requestId, optionId });
}

export async function sessionCancel(): Promise<void> {
  return invoke("session_cancel");
}

export async function gitStatus(): Promise<RepositorySnapshot> {
  return invoke<RepositorySnapshot>("git_status");
}

export async function gitRefresh(): Promise<RepositorySnapshot> {
  return invoke<RepositorySnapshot>("git_refresh");
}

export async function gitStage(paths: string[]): Promise<void> {
  return invoke("git_stage", { paths });
}

export async function gitUnstage(paths: string[]): Promise<void> {
  return invoke("git_unstage", { paths });
}

export async function gitCommit(message: string): Promise<void> {
  return invoke("git_commit", { message });
}

export async function editorOpenFile(path: string): Promise<string> {
  return invoke<string>("editor_open_file", { path });
}

export async function editorSaveFile(path: string, content: string): Promise<void> {
  return invoke("editor_save_file", { path, content });
}

export async function editorGetContent(path: string): Promise<string> {
  return invoke<string>("editor_get_content", { path });
}

export async function reviewGetDiff(path: string): Promise<ChangedFile | null> {
  return invoke<ChangedFile | null>("review_get_diff", { path });
}

export async function reviewGetGitDiffContent(path: string): Promise<SessionFileChange | null> {
  return invoke<SessionFileChange | null>("review_get_git_diff_content", { path });
}

export async function reviewApplyPatch(path: string): Promise<void> {
  return invoke("review_apply_patch", { path });
}

export async function reviewRejectPatch(path: string): Promise<void> {
  return invoke("review_reject_patch", { path });
}

export async function workspaceOpen(path: string): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("workspace_open", { path });
}

export async function workspaceClose(): Promise<void> {
  return invoke("workspace_close");
}

export async function workspaceGetRecent(): Promise<RecentWorkspace[]> {
  return invoke<RecentWorkspace[]>("workspace_get_recent");
}

export async function workspaceRemoveRecent(path: string): Promise<void> {
  return invoke("workspace_remove_recent", { path });
}

export async function sessionList(): Promise<SessionListItem[]> {
  return invoke<SessionListItem[]>("session_list");
}

export async function sessionSwitch(id: string): Promise<void> {
  return invoke("session_switch", { id });
}

export async function sessionCreate(): Promise<void> {
  return invoke("session_create");
}

export async function sessionDelete(id: string): Promise<void> {
  return invoke("session_delete", { id });
}

export async function sessionGetChanges(): Promise<SessionFileChange[]> {
  return invoke<SessionFileChange[]>("session_get_changes");
}

export async function sessionGetFileDiff(path: string): Promise<SessionFileChange> {
  return invoke<SessionFileChange>("session_get_file_diff", { path });
}

export async function fsListDir(path: string): Promise<FileEntry[]> {
  return invoke<FileEntry[]>("fs_list_dir", { path });
}

export async function sessionReconnect(): Promise<void> {
  return invoke("session_reconnect");
}

export async function fsSearch(query: string): Promise<SearchResult> {
  return invoke<SearchResult>("fs_search", { query });
}

export async function settingsGetAgentSnapshot(): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_get_agent_snapshot");
}

export async function settingsDetectAgents(): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_detect_agents");
}

export async function settingsSelectAgent(agent: AgentCliId): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_agent", { agent });
}

export async function settingsInstallAgent(agent: AgentCliId): Promise<AgentInstallResult> {
  return invoke<AgentInstallResult>("settings_install_agent", { agent });
}
