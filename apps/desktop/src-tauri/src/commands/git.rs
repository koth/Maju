use crate::state::AppState;
use tauri::State;
use workspace_model::RepositorySnapshot;

#[tauri::command]
pub fn git_status(state: State<'_, AppState>) -> Result<RepositorySnapshot, String> {
    state.with_app(|app| Ok(app.ui.repository.clone()))
}

#[tauri::command]
pub fn git_refresh(state: State<'_, AppState>) -> Result<RepositorySnapshot, String> {
    state.with_app(|app| {
        app.refresh_repository();
        Ok(app.ui.repository.clone())
    })
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
