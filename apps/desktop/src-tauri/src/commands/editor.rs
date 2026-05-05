use crate::state::AppState;
use std::path::PathBuf;
use tauri::State;

/// Resolve a path: if absolute, use as-is; if relative, join with workspace root.
fn resolve_path(root: &std::path::Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        root.join(path)
    }
}

#[tauri::command]
pub fn editor_open_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    state.with_app(|app| {
        let full_path = resolve_path(&app.ui.workspace.root, &path);
        if !full_path.exists() {
            return Err(format!("File not found: {}", full_path.display()));
        }
        std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Cannot read {}: {}", full_path.display(), e))
    })
}

#[tauri::command]
pub fn editor_save_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> Result<(), String> {
    state.with_app(|app| {
        let full_path = resolve_path(&app.ui.workspace.root, &path);
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                return Err(format!("Directory not found: {}", parent.display()));
            }
        }
        std::fs::write(&full_path, content)
            .map_err(|e| format!("Cannot write {}: {}", full_path.display(), e))
    })
}

#[tauri::command]
pub fn editor_get_content(state: State<'_, AppState>, path: String) -> Result<String, String> {
    state.with_app(|app| {
        let full_path = resolve_path(&app.ui.workspace.root, &path);
        if !full_path.exists() {
            return Err(format!("File not found: {}", full_path.display()));
        }
        std::fs::read_to_string(&full_path)
            .map_err(|e| format!("Cannot read {}: {}", full_path.display(), e))
    })
}
