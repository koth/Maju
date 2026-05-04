use crate::state::AppState;
use tauri::{AppHandle, State};
use workspace_model::{
    SessionConfigState, SessionFileChange, SessionListItem, UiSnapshot, UserPromptContent,
};

#[tauri::command]
pub fn session_get_state(state: State<'_, AppState>) -> Result<UiSnapshot, String> {
    state.with_app(|app| {
        app.poll_prompt_progress();
        Ok(app.ui.clone())
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
) -> Result<SessionConfigState, String> {
    state.with_app(|app_state| {
        let session_config = app_state.set_session_config_control(&control_id, &value_id)?;
        crate::events::emit_session_config_updated(&app, &app_state.ui);
        Ok(session_config)
    })
}

#[tauri::command]
pub fn session_resolve_permission(
    state: State<'_, AppState>,
    request_id: String,
    option_id: Option<String>,
) -> Result<(), String> {
    state.with_app(|app| app.resolve_tool_permission(&request_id, option_id))
}

#[tauri::command]
pub fn session_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.cancel_prompt())
}

#[tauri::command]
pub fn session_list(state: State<'_, AppState>) -> Result<Vec<SessionListItem>, String> {
    state.with_app(|app| app.session_list())
}

#[tauri::command]
pub fn session_switch(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.with_app(|app| app.session_switch(&id))
}

#[tauri::command]
pub fn session_create(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.session_create())
}

#[tauri::command]
pub fn session_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.with_app(|app| app.session_delete(&id))
}

#[tauri::command]
pub fn session_get_changes(state: State<'_, AppState>) -> Result<Vec<SessionFileChange>, String> {
    state.with_app(|app| Ok(app.ui.session_changes.clone()))
}

#[tauri::command]
pub fn session_get_file_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<SessionFileChange, String> {
    state.with_app(|app| {
        app.ui
            .session_changes
            .iter()
            .find(|c| c.path == path)
            .cloned()
            .ok_or_else(|| format!("No change found for path: {path}"))
    })
}

#[tauri::command]
pub fn session_reconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.reconnect_session())
}
