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
pub fn git_unstage(_state: State<'_, AppState>, _paths: Vec<String>) -> Result<(), String> {
    // TODO: implement when git-service supports unstaging
    Ok(())
}

#[tauri::command]
pub fn git_commit(_state: State<'_, AppState>, _message: String) -> Result<(), String> {
    // TODO: implement when git-service supports committing
    Ok(())
}
