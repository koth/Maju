use crate::commands::workspace::save_open_workspace_state;
use crate::state::AppState;
use tauri::{AppHandle, Manager, State};
use workspace_model::{
    AgentCliId, ArchivedSessionListItem, ChangeSetFilesResponse, ChangeSetSummary,
    FileChangeRecord, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest,
    ListChangeSetsRequest, PermissionInputResponse, PromptSendOutcome, SessionConfigState,
    SessionFileChange, SessionJobRecord, UiSnapshot, UiSnapshotPatch, UsageDailyBucket,
    UsageSummaryRequest, UsageSummaryRow, UserPromptContent, WorkspaceSessionList,
};

#[tauri::command]
pub fn session_get_state(state: State<'_, AppState>) -> Result<UiSnapshot, String> {
    state.with_app(|app| {
        app.poll_prompt_progress();
        Ok(app.lightweight_ui_snapshot())
    })
}

/// Lightweight revision probe used by the frontend self-heal poll. Returns the
/// current session id + revision without cloning the whole `UiSnapshot`, so the
/// periodic poll stays cheap on long sessions and only pays for a full snapshot
/// when the revision actually advanced.
#[tauri::command]
pub fn session_get_revision(state: State<'_, AppState>) -> Result<(String, u64), String> {
    state.with_app(|app| {
        app.poll_prompt_progress();
        Ok((app.ui.session.id.to_string(), app.ui.revision))
    })
}

/// Incremental self-heal source: the emitted-patch chain continuing from
/// `since_revision`, or `None` when the bridge's replay buffer cannot cover
/// the span (eviction / session change) and the caller must fall back to a
/// full `session_get_state`.
#[tauri::command]
pub fn session_get_patches_since(
    state: State<'_, AppState>,
    since_revision: u64,
) -> Result<Option<Vec<UiSnapshotPatch>>, String> {
    state.with_app(|app| {
        let session_id = app.ui.session.id.to_string();
        Ok(state.get_patches_since(&session_id, since_revision))
    })
}

#[tauri::command]
pub fn session_send_prompt(
    state: State<'_, AppState>,
    prompt: Vec<UserPromptContent>,
) -> Result<PromptSendOutcome, String> {
    state.with_app(|app| {
        app.send_prompt_content_background(prompt)
            .map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub fn session_retry_user_message(
    state: State<'_, AppState>,
    message_id: String,
    text: String,
) -> Result<(), String> {
    state.with_app(|app| {
        app.retry_user_message_background(&message_id, text)
            .map(|_outcome| ())
            .map_err(|e| e.to_string())
    })
}

#[tauri::command]
pub fn session_set_config_control(
    app: AppHandle,
    state: State<'_, AppState>,
    control_id: String,
    value_id: String,
    provider: Option<String>,
) -> Result<SessionConfigState, String> {
    state.with_app(|app_state| {
        let session_config =
            app_state.set_session_config_control(&control_id, &value_id, provider.as_deref())?;
        crate::events::emit_session_config_updated(&app, &app_state.ui);
        Ok(session_config)
    })
}

#[tauri::command]
pub fn session_resolve_permission(
    state: State<'_, AppState>,
    request_id: String,
    option_id: Option<String>,
    guidance: Option<String>,
    input_response: Option<PermissionInputResponse>,
) -> Result<(), String> {
    state.with_app(|app| {
        app.resolve_tool_permission(&request_id, option_id, guidance, input_response)
    })
}

#[tauri::command]
pub fn session_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.cancel_prompt())
}

/// Polish the desktop's local handoff digest into a readable briefing with one
/// model call (the provider configured for session titles).
///
/// Async + blocking pool: the request is a full model round trip, and the dialog
/// stays interactive while it runs. Failures come back as messages so the dialog
/// can keep showing its local digest.
#[tauri::command]
pub async fn session_handoff_summary(app: AppHandle, material: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.with_app(|app_state| app_state.generate_handoff_summary(&material))
    })
    .await
    .map_err(|e| format!("Handoff summary task failed: {e}"))?
}

#[tauri::command]
pub fn session_stop_tool(state: State<'_, AppState>, tool_call_id: String) -> Result<(), String> {
    state.with_app(|app| app.stop_tool(&tool_call_id))
}

#[tauri::command]
pub async fn session_list(app: AppHandle) -> Result<Vec<WorkspaceSessionList>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.list_workspace_sessions()
    })
    .await
    .map_err(|e| format!("Session list task failed: {e}"))?
}

#[tauri::command]
pub async fn session_list_archived() -> Result<Vec<ArchivedSessionListItem>, String> {
    tokio::task::spawn_blocking(move || {
        let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
        let store =
            session_store::SessionStore::open_global(paths.root()).map_err(|e| e.to_string())?;
        store.list_archived_sessions().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Archived session list task failed: {e}"))?
}

#[tauri::command]
pub fn session_switch(
    state: State<'_, AppState>,
    id: String,
    workspace_root: Option<String>,
) -> Result<(), String> {
    state.with_workspace_app(workspace_root, |app| app.session_switch(&id))?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn session_create(
    state: State<'_, AppState>,
    workspace_root: Option<String>,
    agent: Option<AgentCliId>,
    preset: Option<String>,
) -> Result<(), String> {
    let default_agent = agent.or_else(|| {
        app_core::AppPaths::resolve()
            .ok()
            .map(|paths| app_core::settings::default_agent_for_new_work(&paths))
    });
    state.with_workspace_app(workspace_root, |app| app.session_create(default_agent, preset))?;
    save_open_workspace_state(&state)
}

/// Fork the conversation from a completed assistant message
/// ("从这里创建聊天分支"). The backend creates the branch session and switches
/// the active session to it before returning.
#[tauri::command]
pub fn session_fork(
    state: State<'_, AppState>,
    message_id: String,
    mode: String,
) -> Result<workspace_model::SessionForkOutcome, String> {
    let mode = workspace_model::SessionForkMode::parse(&mode)
        .ok_or_else(|| format!("未知的分叉方式：{mode}"))?;
    let outcome = state.with_app(|app| app.session_fork(&message_id, mode))?;
    save_open_workspace_state(&state)?;
    Ok(outcome)
}

/// Fork branch points for the fork picker: every completed turn of the
/// session (full persisted history, not just the UI's tail window).
#[tauri::command]
pub fn session_fork_candidates(
    state: State<'_, AppState>,
) -> Result<Vec<workspace_model::SessionForkCandidate>, String> {
    state.with_app(|app| app.session_fork_candidates())
}

#[tauri::command]
pub fn session_delete(
    state: State<'_, AppState>,
    id: String,
    workspace_root: Option<String>,
) -> Result<(), String> {
    state.delete_session(workspace_root, &id)?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn session_archive(
    state: State<'_, AppState>,
    id: String,
    workspace_root: Option<String>,
) -> Result<(), String> {
    state.archive_session(workspace_root, &id)?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn session_unarchive(
    state: State<'_, AppState>,
    id: String,
    workspace_root: Option<String>,
) -> Result<(), String> {
    state.unarchive_session(workspace_root, &id)?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn session_delete_archived(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.delete_archived_session(&id)
}

#[tauri::command]
pub fn session_delete_all_archived(state: State<'_, AppState>) -> Result<(), String> {
    state.delete_all_archived_sessions()
}

/// Background jobs (后台任务) the dsh harness reports for the visible session
/// (empty for non-harness agents). The context dock polls this for its
/// "后台任务" section.
#[tauri::command]
pub fn session_list_background_jobs(
    state: State<'_, AppState>,
) -> Result<Vec<SessionJobRecord>, String> {
    state.with_app(|app| Ok(app.session_background_jobs()))
}

#[tauri::command]
pub fn session_get_changes(state: State<'_, AppState>) -> Result<Vec<SessionFileChange>, String> {
    state.with_app(|app| Ok(app.ui.session_changes.clone()))
}

#[tauri::command]
pub fn usage_get_summary(
    state: State<'_, AppState>,
    request: Option<UsageSummaryRequest>,
) -> Result<Vec<UsageSummaryRow>, String> {
    state.with_app(|app| Ok(app.usage_summary(request.unwrap_or_default())))
}

#[tauri::command]
pub fn usage_get_daily_series(
    state: State<'_, AppState>,
    request: Option<UsageSummaryRequest>,
) -> Result<Vec<UsageDailyBucket>, String> {
    state.with_app(|app| Ok(app.usage_daily_series(request.unwrap_or_default())))
}

#[tauri::command]
pub fn usage_get_request_count(
    state: State<'_, AppState>,
    request: Option<UsageSummaryRequest>,
) -> Result<u64, String> {
    state.with_app(|app| Ok(app.usage_request_count(request.unwrap_or_default())))
}

#[tauri::command]
pub async fn session_list_change_sets(
    app: AppHandle,
    request: Option<ListChangeSetsRequest>,
) -> Result<Vec<ChangeSetSummary>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.with_app(|app| Ok(app.list_change_sets(request.unwrap_or_default())))
    })
    .await
    .map_err(|e| format!("List change sets task failed: {e}"))?
}

#[tauri::command]
pub async fn session_list_change_set_files(
    app: AppHandle,
    request: ListChangeSetFilesRequest,
) -> Result<ChangeSetFilesResponse, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.with_app(|app| Ok(app.list_change_set_files(request)))
    })
    .await
    .map_err(|e| format!("List change set files task failed: {e}"))?
}

#[tauri::command]
pub async fn session_get_change_set_file_diff(
    app: AppHandle,
    request: GetChangeSetFileDiffRequest,
) -> Result<Option<FileChangeRecord>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.with_app(|app| Ok(app.get_change_set_file_diff(request)))
    })
    .await
    .map_err(|e| format!("Get change set file diff task failed: {e}"))?
}

#[tauri::command]
pub fn session_get_file_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<SessionFileChange, String> {
    state.with_app(|app| app.session_file_diff(&path))
}

#[tauri::command]
pub fn session_get_turn_file_diff(
    state: State<'_, AppState>,
    message_id: String,
    path: String,
) -> Result<SessionFileChange, String> {
    state.with_app(|app| app.session_turn_file_diff(&message_id, &path))
}

#[tauri::command]
pub fn session_load_history_before(
    state: State<'_, AppState>,
    before_seq: i64,
    limit: usize,
) -> Result<app_core::HistoryPage, String> {
    state.with_app(|app| app.load_history_before(before_seq, limit))
}

#[tauri::command]
pub fn session_get_tool_detail(
    state: State<'_, AppState>,
    tool_id: String,
) -> Result<workspace_model::ToolInvocation, String> {
    state.with_app(|app| app.session_tool_detail(&tool_id))
}

#[tauri::command]
pub fn session_reconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.reconnect_session())
}
