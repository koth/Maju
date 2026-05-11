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
    let open_state =
        app_core::startup_perf::measure("workspace/save_open_state/build", "", || {
            state.open_workspace_state()
        })?;
    app_core::startup_perf::measure("workspace/save_open_state/write", "", || {
        open_store()?.save(&open_state);
        Ok::<(), String>(())
    })?;
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
    app_core::startup_perf::mark("workspace_restore_open/start", "");
    let saved = app_core::startup_perf::measure("workspace_restore_open/load_saved", "", || {
        open_store().map(|store| store.load())
    })?;
    app_core::startup_perf::mark(
        "workspace_restore_open/saved",
        format!(
            "workspace_count={} active_path={}",
            saved.workspaces.len(),
            saved.active_path.as_deref().unwrap_or("<none>")
        ),
    );
    if saved.workspaces.is_empty() {
        app_core::startup_perf::mark("workspace_restore_open/end", "empty");
        return Ok(None);
    }

    let workspaces = saved.workspaces;
    let preferred_active = saved
        .active_path
        .clone()
        .or_else(|| workspaces.first().map(|workspace| workspace.path.clone()));
    let mut snapshot = None;
    let mut opened_active_path: Option<String> = None;

    if let Some(active_path) = preferred_active {
        let dir = PathBuf::from(&active_path);
        let exists = app_core::startup_perf::measure(
            "workspace_restore_open/active_is_dir",
            &active_path,
            || dir.is_dir(),
        );
        if exists {
            snapshot = Some(app_core::startup_perf::measure(
                "workspace_restore_open/open_active",
                &active_path,
                || state.open_workspace(dir, None),
            )?);
            opened_active_path = Some(active_path);
        } else {
            app_core::startup_perf::mark("workspace_restore_open/active_missing", active_path);
        }
    }

    if snapshot.is_none() {
        for workspace in &workspaces {
            let dir = PathBuf::from(&workspace.path);
            let exists = app_core::startup_perf::measure(
                "workspace_restore_open/fallback_is_dir",
                &workspace.path,
                || dir.is_dir(),
            );
            if exists {
                snapshot = Some(app_core::startup_perf::measure(
                    "workspace_restore_open/open_fallback",
                    &workspace.path,
                    || state.open_workspace(dir, None),
                )?);
                opened_active_path = Some(workspace.path.clone());
                break;
            }
        }
    }

    for workspace in workspaces {
        if Some(workspace.path.as_str()) != opened_active_path.as_deref() {
            let path = workspace.path;
            let dormant_path = path.clone();
            app_core::startup_perf::measure(
                "workspace_restore_open/register_dormant",
                &path,
                || state.restore_dormant_workspace(PathBuf::from(dormant_path)),
            )?;
        }
    }

    app_core::startup_perf::measure("workspace_restore_open/save_state", "", || {
        save_open_workspace_state(&state)
    })?;
    app_core::startup_perf::mark(
        "workspace_restore_open/end",
        format!(
            "opened={}",
            opened_active_path.as_deref().unwrap_or("<none>")
        ),
    );
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
    app_core::startup_perf::measure("workspace_get_recent", "", || {
        recent_store().map(|store| store.load()).unwrap_or_default()
    })
}

#[tauri::command]
pub fn workspace_remove_recent(path: String) {
    if let Ok(store) = recent_store() {
        store.remove(&path);
    }
}
