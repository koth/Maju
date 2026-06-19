use crate::commands::workspace::save_open_workspace_state;
use crate::state::AppState;
use tauri::{AppHandle, Manager, State};
use workspace_model::{
    AgentCliId, ArchivedSessionListItem, ChangeSetFilesResponse, ChangeSetSummary,
    FileChangeRecord, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest,
    ListChangeSetsRequest, PermissionInputResponse, SessionConfigState, SessionFileChange,
    UiSnapshot, UserPromptContent, WorkspaceSessionList,
};

#[tauri::command]
pub fn session_get_state(state: State<'_, AppState>) -> Result<UiSnapshot, String> {
    state.with_app(|app| {
        app.poll_prompt_progress();
        Ok(app.lightweight_ui_snapshot())
    })
}

#[tauri::command]
pub fn session_send_prompt(
    state: State<'_, AppState>,
    prompt: Vec<UserPromptContent>,
) -> Result<(), String> {
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
) -> Result<(), String> {
    let default_agent = agent.or_else(|| {
        app_core::AppPaths::resolve()
            .ok()
            .map(|paths| app_core::settings::default_agent_for_new_work(&paths))
    });
    state.with_workspace_app(workspace_root, |app| app.session_create(default_agent))?;
    save_open_workspace_state(&state)
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

#[tauri::command]
pub fn session_get_changes(state: State<'_, AppState>) -> Result<Vec<SessionFileChange>, String> {
    state.with_app(|app| Ok(app.ui.session_changes.clone()))
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
pub fn session_reconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.reconnect_session())
}
