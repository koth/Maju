use crate::state::AppState;
use tauri::State;
use workspace_model::{EditorFileSnapshot, EditorFileVersion};

#[tauri::command]
pub fn editor_open_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| app.editor_open_file(&path))
}

#[tauri::command]
pub fn editor_save_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
    base_version: Option<EditorFileVersion>,
    overwrite: Option<bool>,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| {
        app.editor_save_file(
            &path,
            &content,
            base_version.as_ref(),
            overwrite.unwrap_or(false),
        )
    })
}

#[tauri::command]
pub fn editor_get_content(
    state: State<'_, AppState>,
    path: String,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| app.editor_open_file(&path))
}
