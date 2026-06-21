import { invoke } from "@tauri-apps/api/core";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import type { UiSnapshot, RepositorySnapshot, ChangedFile, RecentWorkspace, SessionFileChange, FileEntry, SessionConfigState, UserPromptContent, SearchResult, AgentCliId, AgentProviderFamily, AgentSettingsSnapshot, AgentInstallResult, OpenWorkspaceItem, WorkspaceSessionList, ArchivedSessionListItem, AppTheme, EditorFileSnapshot, EditorFileVersion, LspServerStatus, LspDiagnostic, LspSettingsSnapshot, LspServerConfigInput, LspProbeResult, ChangeSetSummary, ChangeSetFilesResponse, FileChangeRecord, ListChangeSetsRequest, ListChangeSetFilesRequest, GetChangeSetFileDiffRequest, TerminalOpenRequest, TerminalSession, TerminalWriteRequest, TerminalResizeRequest, TerminalIdRequest, TerminalScrollback, RemoteLinuxWorkspace, RemoteMachineProfileInput, RemoteMachineProfilesSnapshot, RemoteMachineValidationRequest, RemoteOpenRequest, PermissionInputResponse } from "../types";

export async function openExternalUrl(url: string): Promise<void> {
  await shellOpen(url);
}

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

export async function sessionRetryUserMessage(messageId: string, text: string): Promise<void> {
  return invoke("session_retry_user_message", { messageId, text });
}

export async function sessionSetConfigControl(
  controlId: string,
  valueId: string,
  provider?: string | null,
): Promise<SessionConfigState> {
  return invoke<SessionConfigState>("session_set_config_control", { controlId, valueId, provider: provider ?? null });
}

export async function sessionResolvePermission(
  requestId: string,
  optionId: string | null,
  guidance?: string | null,
  inputResponse?: PermissionInputResponse | null,
): Promise<void> {
  return invoke("session_resolve_permission", {
    requestId,
    optionId,
    guidance: guidance ?? null,
    inputResponse: inputResponse ?? null,
  });
}

export async function sessionCancel(): Promise<void> {
  return invoke("session_cancel");
}

export async function sessionStopTool(toolCallId: string): Promise<void> {
  return invoke("session_stop_tool", { toolCallId });
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

export async function workspaceOpenRemoteLinux(remote: RemoteLinuxWorkspace): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("workspace_open_remote_linux", { remote });
}

export async function workspaceOpenRemoteProfile(request: RemoteOpenRequest): Promise<UiSnapshot> {
  return invoke<UiSnapshot>("workspace_open_remote_profile", { request });
}

export async function workspaceClose(): Promise<void> {
  return invoke("workspace_close");
}

export async function workspaceArchive(path: string): Promise<UiSnapshot | null> {
  return invoke<UiSnapshot | null>("workspace_archive", { path });
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

export async function sessionListArchived(): Promise<ArchivedSessionListItem[]> {
  return invoke<ArchivedSessionListItem[]>("session_list_archived");
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

export async function sessionArchive(id: string, workspaceRoot?: string): Promise<void> {
  return invoke("session_archive", { id, workspaceRoot });
}

export async function sessionUnarchive(id: string, workspaceRoot?: string): Promise<void> {
  return invoke("session_unarchive", { id, workspaceRoot });
}

export async function sessionDeleteArchived(id: string): Promise<void> {
  return invoke("session_delete_archived", { id });
}

export async function sessionDeleteAllArchived(): Promise<void> {
  return invoke("session_delete_all_archived");
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

export async function fsDeleteFile(path: string): Promise<void> {
  return invoke("fs_delete_file", { path });
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

export async function settingsGetAgentSnapshot(remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_get_agent_snapshot", { remoteProfileId: remoteProfileId ?? null });
}

export async function settingsDetectAgents(remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_detect_agents", { remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectAgent(agent: AgentCliId, remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_agent", { agent, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectTheme(theme: AppTheme, remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_theme", { theme, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsGetRemoteProfiles(): Promise<RemoteMachineProfilesSnapshot> {
  return invoke<RemoteMachineProfilesSnapshot>("settings_get_remote_profiles");
}

export async function settingsSaveRemoteProfile(input: RemoteMachineProfileInput): Promise<RemoteMachineProfilesSnapshot> {
  return invoke<RemoteMachineProfilesSnapshot>("settings_save_remote_profile", { input });
}

export async function settingsDeleteRemoteProfile(profileId: string): Promise<RemoteMachineProfilesSnapshot> {
  return invoke<RemoteMachineProfilesSnapshot>("settings_delete_remote_profile", { profileId });
}

export async function settingsValidateRemoteProfile(request: RemoteMachineValidationRequest): Promise<RemoteMachineProfilesSnapshot> {
  return invoke<RemoteMachineProfilesSnapshot>("settings_validate_remote_profile", { request });
}

export async function settingsSaveWebToolsSettings(enabled: boolean, provider: string): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_web_tools_settings", { enabled, provider });
}

export async function settingsSaveWebToolsProviderKey(provider: string, apiKey: string): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_web_tools_provider_key", { provider, apiKey });
}

export async function settingsSaveCodexAcpProviderKey(provider: string, apiKey: string, remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_codex_acp_provider_key", { provider, apiKey, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectCodexAcpProvider(provider: string, remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_codex_acp_provider", { provider, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectCodexDefaultMode(remoteProfileId?: string | null): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_codex_default_mode", { remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectAgentProviderProfile(
  family: AgentProviderFamily,
  profileId: string,
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_agent_provider_profile", { family, profileId, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSaveAgentProviderSecret(
  family: AgentProviderFamily,
  profileId: string,
  secret: string,
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_agent_provider_secret", { family, profileId, secret, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSaveProviderModels(
  provider: string,
  models: string[],
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_save_provider_models", { provider, models, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSyncProviderModelsFromUrl(
  provider: string,
  modelListUrl: string,
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_sync_provider_models_from_url", {
    provider,
    modelListUrl,
    remoteProfileId: remoteProfileId ?? null,
  });
}

export async function settingsResetProviderModels(
  provider: string,
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_reset_provider_models", { provider, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSelectClaudeFastModel(
  modelId: string | null,
  remoteProfileId?: string | null,
): Promise<AgentSettingsSnapshot> {
  return invoke<AgentSettingsSnapshot>("settings_select_claude_fast_model", { modelId, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsInstallAgent(agent: AgentCliId): Promise<AgentInstallResult> {
  return invoke<AgentInstallResult>("settings_install_agent", { agent });
}

export async function settingsGetLspSnapshot(remoteProfileId?: string | null): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_get_lsp_snapshot", { remoteProfileId: remoteProfileId ?? null });
}

export async function settingsSaveLspServer(config: LspServerConfigInput, remoteProfileId?: string | null): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_save_lsp_server", { config, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsResetLspServer(languageId: string, remoteProfileId?: string | null): Promise<LspSettingsSnapshot> {
  return invoke<LspSettingsSnapshot>("settings_reset_lsp_server", { languageId, remoteProfileId: remoteProfileId ?? null });
}

export async function settingsProbeLspServer(command: string, remoteProfileId?: string | null): Promise<LspProbeResult> {
  return invoke<LspProbeResult>("settings_probe_lsp_server", { command, remoteProfileId: remoteProfileId ?? null });
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
