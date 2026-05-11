use crate::commands::workspace::save_open_workspace_state;
use crate::state::AppState;
use app_core::normalize_tracked_path;
use tauri::{AppHandle, State};
use workspace_model::{
    AgentCliId, SessionConfigState, SessionFileChange, UiSnapshot, UserPromptContent,
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
            .map(|paths| app_core::settings::load_app_settings(&paths).selected_agent)
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

fn normalize_to_ws_relative(path: &str, ws_root: &str) -> String {
    let normalized = normalize_tracked_path(path);
    let ws_norm = normalize_tracked_path(ws_root);
    let ws_prefix = if ws_norm.ends_with('/') {
        ws_norm
    } else {
        format!("{}/", ws_norm)
    };
    normalized
        .strip_prefix(&ws_prefix)
        .unwrap_or(&normalized)
        .to_string()
}

#[tauri::command]
pub fn session_get_file_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<SessionFileChange, String> {
    state.with_app(|app| {
        let normalized = normalize_tracked_path(&path);
        let ws_root = app.ui.workspace.root.display().to_string();
        app.ui
            .session_changes
            .iter()
            .find(|c| {
                normalize_tracked_path(&c.path) == normalized
                    || normalize_to_ws_relative(&c.path, &ws_root) == normalized
            })
            .cloned()
            .ok_or_else(|| format!("No change found for path: {path}"))
    })
}

#[tauri::command]
pub fn session_reconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.with_app(|app| app.reconnect_session())
}
