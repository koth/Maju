use crate::commands::workspace::save_open_workspace_state;
use crate::state::AppState;
use tauri::{AppHandle, State};
use workspace_model::{
    AgentCliId, ChangeSetFilesResponse, ChangeSetSummary, FileChangeRecord,
    GetChangeSetFileDiffRequest, ListChangeSetFilesRequest, ListChangeSetsRequest,
    PermissionInputResponse, SessionConfigState, SessionFileChange, UiSnapshot, UserPromptContent,
    WorkspaceSessionList,
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
pub fn session_list(state: State<'_, AppState>) -> Result<Vec<WorkspaceSessionList>, String> {
    state.list_workspace_sessions()
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
pub fn session_get_changes(state: State<'_, AppState>) -> Result<Vec<SessionFileChange>, String> {
    state.with_app(|app| Ok(app.ui.session_changes.clone()))
}

#[tauri::command]
pub fn session_list_change_sets(
    state: State<'_, AppState>,
    request: Option<ListChangeSetsRequest>,
) -> Result<Vec<ChangeSetSummary>, String> {
    state.with_app(|app| Ok(app.list_change_sets(request.unwrap_or_default())))
}

#[tauri::command]
pub fn session_list_change_set_files(
    state: State<'_, AppState>,
    request: ListChangeSetFilesRequest,
) -> Result<ChangeSetFilesResponse, String> {
    state.with_app(|app| Ok(app.list_change_set_files(request)))
}

#[tauri::command]
pub fn session_get_change_set_file_diff(
    state: State<'_, AppState>,
    request: GetChangeSetFileDiffRequest,
) -> Result<Option<FileChangeRecord>, String> {
    state.with_app(|app| Ok(app.get_change_set_file_diff(request)))
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
