use crate::state::AppState;
use tauri::{AppHandle, Manager, State};
use workspace_model::{EditorFileSnapshot, EditorFileVersion};

#[tauri::command]
pub async fn editor_open_file(app: AppHandle, path: String) -> Result<EditorFileSnapshot, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.open_workspace_file(path)
    })
    .await
    .map_err(|e| format!("Open editor file task failed: {e}"))?
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
pub async fn editor_get_content(
    app: AppHandle,
    path: String,
) -> Result<EditorFileSnapshot, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.open_workspace_file(path)
    })
    .await
    .map_err(|e| format!("Get editor content task failed: {e}"))?
}
