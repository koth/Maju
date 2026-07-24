use crate::state::AppState;
use tauri::State;
use workspace_model::{ChangedFile, SessionFileChange};

#[tauri::command]
pub fn review_get_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<ChangedFile>, String> {
    state.with_app(|app| Ok(app.review_changed_file(&path)))
}

#[tauri::command]
pub fn review_apply_patch(_state: State<'_, AppState>, _path: String) -> Result<(), String> {
    // TODO: implement patch application through app-core
    Ok(())
}

#[tauri::command]
pub fn review_reject_patch(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.with_app(|app| app.reject_review_file_change(&path))
}

#[tauri::command]
pub fn review_get_git_diff_content(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<SessionFileChange>, String> {
    state.with_app(|app| app.review_git_diff_content(&path))
}
