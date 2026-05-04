use crate::recent_workspaces::{RecentEntry, RecentWorkspaces};
use crate::state::AppState;
use std::path::PathBuf;
use tauri::State;

fn recent_store() -> Result<RecentWorkspaces, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(RecentWorkspaces::new(paths.workspaces_dir()))
}

#[tauri::command]
pub fn workspace_open(
    state: State<'_, AppState>,
    path: String,
) -> Result<workspace_model::UiSnapshot, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }
    let snapshot = state.open_workspace(dir)?;
    recent_store()?.add(&path);
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_close(state: State<'_, AppState>) -> Result<(), String> {
    state.close_workspace()
}

#[tauri::command]
pub fn workspace_get_recent() -> Vec<RecentEntry> {
    recent_store().map(|store| store.load()).unwrap_or_default()
}

#[tauri::command]
pub fn workspace_remove_recent(path: String) {
    if let Ok(store) = recent_store() {
        store.remove(&path);
    }
}
