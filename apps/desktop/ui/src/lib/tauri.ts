import { invoke } from "@tauri-apps/api/core";
import type { UiSnapshot, RepositorySnapshot, ChangedFile, RecentWorkspace, SessionFileChange, FileEntry, SessionConfigState, UserPromptContent, SearchResult, AgentCliId, AgentSettingsSnapshot, AgentInstallResult, OpenWorkspaceItem, WorkspaceSessionList, AppTheme, EditorFileSnapshot, EditorFileVersion, LspServerStatus, LspDiagnostic, LspSettingsSnapshot, LspServerConfigInput, LspProbeResult, ChangeSetSummary, ChangeSetFilesResponse, FileChangeRecord, ListChangeSetsRequest, ListChangeSetFilesRequest, GetChangeSetFileDiffRequest, TerminalOpenRequest, TerminalSession, TerminalWriteRequest, TerminalResizeRequest, TerminalIdRequest, TerminalScrollback, ClaudeWoaConfigInput, ClaudeWoaLoginStart, ClaudeWoaLoginStatus } from "../types";

export async function startupPerfMark(stage: string, detail?: string): Promise<void> {
  try {
    await invoke("startup_perf_mark", { stage, detail });
  } catch {
    // Startup diagnostics must never block the UI.
  }
}

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

export async function editorOpenFile(path: string): Promise<EditorFileSnapshot> {
  return invoke<EditorFileSnapshot>("editor_open_file", { path });
}

export async function editorSaveFile(
  path: string,
  content: string,
  baseVersion?: EditorFileVersion | null,
  overwrite = false,
): Promise<EditorFileSnapshot> {
  return invoke<EditorFileSnapshot>("editor_save_file", {
    path,
    content,
    baseVersion,
    overwrite,
  });
}

export async function editorGetContent(path: string): Promise<EditorFileSnapshot> {
  return invoke<EditorFileSnapshot>("editor_get_content", { path });
}

export async function editorLspOpenDocument(path: string, languageId: string, content: string): Promise<LspServerStatus> {
  return invoke<LspServerStatus>("editor_lsp_open_document", { path, languageId, content });
}

export async function editorLspChangeDocument(path: string, languageId: string, content: string): Promise<number> {
  return invoke<number>("editor_lsp_change_document", { path, languageId, content });
}

export async function editorLspSaveDocument(path: string, languageId: string, content: string): Promise<void> {
  return invoke("editor_lsp_save_document", { path, languageId, content });
}

export async function editorLspCloseDocument(path: string, languageId: string): Promise<void> {
  return invoke("editor_lsp_close_document", { path, languageId });
}

export async function editorLspGetDiagnostics(path: string, languageId: string): Promise<LspDiagnostic[]> {
  return invoke<LspDiagnostic[]>("editor_lsp_get_diagnostics", { path, languageId });
}

export async function editorLspRequest<T = unknown>(
  languageId: string,
  method: string,
  params: Record<string, unknown>,
): Promise<T | null> {
  return invoke<T | null>("editor_lsp_request", { languageId, method, params });
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

export async function workspaceOpen(path: string, agent?: AgentCliId): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("workspace_open", { path, agent });
}

export async function workspaceClose(): Promise<void> {
  return invoke("workspace_close");
}

export async function workspaceListOpen(): Promise<OpenWorkspaceItem[]> {
  return invoke<OpenWorkspaceItem[]>("workspace_list_open");
}

export async function workspaceHasOpen(): Promise<boolean> {
  return invoke<boolean>("workspace_has_open");
}

export async function workspaceRestoreOpen(): Promise<UiSnapshot | null> {
  return invoke<UiSnapshot | null>("workspace_restore_open");
}

export async function workspaceSetActive(path: string): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("workspace_set_active", { path });
}

export async function workspaceGetRecent(): Promise<RecentWorkspace[]> {
  return invoke<RecentWorkspace[]>("workspace_get_recent");
}

export async function workspaceRemoveRecent(path: string): Promise<void> {
  return invoke("workspace_remove_recent", { path });
}

export async function sessionList(): Promise<WorkspaceSessionList[]> {
  return invoke<WorkspaceSessionList[]>("session_list");
}

export async function sessionSwitch(id: string, workspaceRoot?: string): Promise<void> {
  return invoke("session_switch", { id, workspaceRoot });
}

export async function sessionCreate(workspaceRoot?: string, agent?: AgentCliId): Promise<void> {
  return invoke("session_create", { workspaceRoot, agent });
}

export async function sessionDelete(id: string, workspaceRoot?: string): Promise<void> {
  return invoke("session_delete", { id, workspaceRoot });
}

export async function sessionGetChanges(): Promise<SessionFileChange[]> {
  return invoke<SessionFileChange[]>("session_get_changes");
}

export async function sessionListChangeSets(request?: ListChangeSetsRequest): Promise<ChangeSetSummary[]> {
  return invoke<ChangeSetSummary[]>("session_list_change_sets", { request: request ?? null });
}

export async function sessionListChangeSetFiles(request: ListChangeSetFilesRequest): Promise<ChangeSetFilesResponse> {
  return invoke<ChangeSetFilesResponse>("session_list_change_set_files", { request });
}

export async function sessionGetChangeSetFileDiff(request: GetChangeSetFileDiffRequest): Promise<FileChangeRecord | null> {
  return invoke<FileChangeRecord | null>("session_get_change_set_file_diff", { request });
}

export async function sessionGetFileDiff(path: string): Promise<SessionFileChange> {
  return invoke<SessionFileChange>("session_get_file_diff", { path });
}

export async function fsListDir(path: string): Promise<FileEntry[]> {
  return invoke<FileEntry[]>("fs_list_dir", { path });
}

export async function fsRename(path: string, newName: string): Promise<FileEntry> {
  return invoke<FileEntry>("fs_rename", { path, newName });
}

export async function fsReveal(path: string, select = false): Promise<void> {
  return invoke("fs_reveal", { path, select });
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

export async function settingsSelectTheme(theme: AppTheme): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_theme", { theme });
}

export async function settingsSaveCodexAcpVenusKey(venusKey: string): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_codex_acp_venus_key", { venusKey });
}

export async function settingsSaveCodexAcpProviderKey(provider: string, apiKey: string): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_codex_acp_provider_key", { provider, apiKey });
}

export async function settingsSelectCodexAcpProvider(provider: string): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_codex_acp_provider", { provider });
}

export async function settingsSelectCodexDefaultMode(): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_codex_default_mode");
}

export async function settingsSaveClaudeWoaConfig(config: ClaudeWoaConfigInput): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_claude_woa_config", { config });
}

export async function settingsStartClaudeWoaLogin(): Promise<ClaudeWoaLoginStart> {
  return invoke<ClaudeWoaLoginStart>("settings_start_claude_woa_login");
}

export async function settingsGetClaudeWoaLogin(loginId: string): Promise<ClaudeWoaLoginStatus> {
  return invoke<ClaudeWoaLoginStatus>("settings_get_claude_woa_login", { loginId });
}

export async function settingsCancelClaudeWoaLogin(loginId: string): Promise<ClaudeWoaLoginStatus> {
  return invoke<ClaudeWoaLoginStatus>("settings_cancel_claude_woa_login", { loginId });
}

export async function settingsRefreshClaudeWoaToken(): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_refresh_claude_woa_token");
}

export async function settingsInstallAgent(agent: AgentCliId): Promise<AgentInstallResult> {
  return invoke<AgentInstallResult>("settings_install_agent", { agent });
}

export async function settingsGetLspSnapshot(): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_get_lsp_snapshot");
}

export async function settingsSaveLspServer(config: LspServerConfigInput): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_save_lsp_server", { config });
}

export async function settingsResetLspServer(languageId: string): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_reset_lsp_server", { languageId });
}

export async function settingsProbeLspServer(command: string): Promise<LspProbeResult> {
  return invoke<LspProbeResult>("settings_probe_lsp_server", { command });
}

export async function terminalOpen(request: TerminalOpenRequest): Promise<TerminalSession> {
  return invoke<TerminalSession>("terminal_open", { request });
}

export async function terminalWrite(request: TerminalWriteRequest): Promise<void> {
  return invoke("terminal_write", { request });
}

export async function terminalScrollback(request: TerminalIdRequest): Promise<TerminalScrollback> {
  return invoke<TerminalScrollback>("terminal_scrollback", { request });
}

export async function terminalResize(request: TerminalResizeRequest): Promise<TerminalSession> {
  return invoke<TerminalSession>("terminal_resize", { request });
}

export async function terminalTerminate(request: TerminalIdRequest): Promise<void> {
  return invoke("terminal_terminate", { request });
}

export async function terminalRestart(request: TerminalResizeRequest): Promise<TerminalSession> {
  return invoke<TerminalSession>("terminal_restart", { request });
}

export async function terminalList(workspaceRoot?: string | null): Promise<TerminalSession[]> {
  return invoke<TerminalSession[]>("terminal_list", { workspaceRoot: workspaceRoot ?? null });
}
