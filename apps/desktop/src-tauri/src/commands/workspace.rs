use crate::open_workspaces::OpenWorkspaces;
use crate::recent_workspaces::{RecentEntry, RecentWorkspaces};
use crate::state::AppState;
use std::path::PathBuf;
use tauri::State;
use workspace_model::{AgentCliId, OpenWorkspaceItem, UiSnapshot};

fn recent_store() -> Result<RecentWorkspaces, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(RecentWorkspaces::new(paths.workspaces_dir()))
}

fn open_store() -> Result<OpenWorkspaces, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(OpenWorkspaces::new(paths.workspaces_dir()))
}

pub fn save_open_workspace_state(state: &AppState) -> Result<(), String> {
    let open_state = state.open_workspace_state()?;
    open_store()?.save(&open_state);
    Ok(())
}

#[tauri::command]
pub fn workspace_open(
    state: State<'_, AppState>,
    path: String,
    agent: Option<AgentCliId>,
) -> Result<UiSnapshot, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }
    let snapshot = state.open_workspace(dir, agent)?;
    recent_store()?.add(&path);
    save_open_workspace_state(&state)?;
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_close(state: State<'_, AppState>) -> Result<(), String> {
    state.close_workspace()?;
    save_open_workspace_state(&state)
}

#[tauri::command]
pub fn workspace_list_open(state: State<'_, AppState>) -> Result<Vec<OpenWorkspaceItem>, String> {
    state.list_open_workspaces()
}

#[tauri::command]
pub fn workspace_has_open(state: State<'_, AppState>) -> Result<bool, String> {
    state.has_open_workspaces()
}

#[tauri::command]
pub fn workspace_restore_open(state: State<'_, AppState>) -> Result<Option<UiSnapshot>, String> {
    let saved = open_store()?.load();
    if saved.workspaces.is_empty() {
        return Ok(None);
    }

    let active_path = saved
        .active_path
        .clone()
        .filter(|path| PathBuf::from(path).is_dir());
    let fallback_active = active_path.clone().or_else(|| {
        saved
            .workspaces
            .first()
            .map(|workspace| workspace.path.clone())
    });
    let mut snapshot = None;

    for workspace in saved.workspaces {
        let dir = PathBuf::from(&workspace.path);
        if !dir.is_dir() {
            continue;
        }
        if Some(workspace.path.as_str()) == fallback_active.as_deref() {
            snapshot = Some(state.open_workspace(dir, None)?);
        } else {
            state.restore_dormant_workspace(dir)?;
        }
    }

    if let Some(active_path) = active_path {
        if let Ok(active_snapshot) = state.set_active_workspace(active_path) {
            snapshot = Some(active_snapshot);
        }
    }

    save_open_workspace_state(&state)?;
    Ok(snapshot)
}

#[tauri::command]
pub fn workspace_set_active(
    state: State<'_, AppState>,
    path: String,
) -> Result<UiSnapshot, String> {
    let snapshot = state.set_active_workspace(path)?;
    save_open_workspace_state(&state)?;
    Ok(snapshot)
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
