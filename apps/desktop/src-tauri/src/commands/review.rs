use crate::state::AppState;
use tauri::State;
use workspace_model::ChangedFile;

#[tauri::command]
pub fn review_get_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<ChangedFile>, String> {
    state.with_app(|app| {
        let file = app
            .ui
            .repository
            .changed_files
            .iter()
            .find(|f| f.path.display().to_string() == path)
            .cloned();
        Ok(file)
    })
}

#[tauri::command]
pub fn review_apply_patch(_state: State<'_, AppState>, _path: String) -> Result<(), String> {
    // TODO: implement patch application through app-core
    Ok(())
}

#[tauri::command]
pub fn review_reject_patch(_state: State<'_, AppState>, _path: String) -> Result<(), String> {
    // TODO: implement patch rejection through app-core
    Ok(())
}
