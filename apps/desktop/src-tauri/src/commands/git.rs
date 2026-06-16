use crate::state::AppState;
use tauri::{AppHandle, Manager, State};
use workspace_model::RepositorySnapshot;

#[tauri::command]
pub fn git_status(state: State<'_, AppState>) -> Result<RepositorySnapshot, String> {
    state.with_app(|app| Ok(app.ui.repository.clone()))
}

#[tauri::command]
pub async fn git_refresh(app: AppHandle) -> Result<RepositorySnapshot, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.git_refresh()
    })
    .await
    .map_err(|e| format!("Git refresh task failed: {e}"))?
}

#[tauri::command]
pub fn git_stage(state: State<'_, AppState>, paths: Vec<String>) -> Result<(), String> {
    state.with_app(|app| app.stage_files(&paths))
}

#[tauri::command]
pub fn git_unstage(state: State<'_, AppState>, paths: Vec<String>) -> Result<(), String> {
    state.with_app(|app| app.unstage_files(&paths))
}

#[tauri::command]
pub fn git_commit(state: State<'_, AppState>, message: String) -> Result<(), String> {
    state.with_app(|app| app.commit_files(&message))
}
