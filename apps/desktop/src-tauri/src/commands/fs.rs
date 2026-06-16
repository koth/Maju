use crate::state::AppState;
use std::path::Path;
use tauri::{AppHandle, Manager, State};
use workspace_model::FileEntry;

#[tauri::command]
pub async fn fs_list_dir(app: AppHandle, path: String) -> Result<Vec<FileEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.list_workspace_dir(path)
    })
    .await
    .map_err(|e| format!("List directory task failed: {e}"))?
}

#[tauri::command]
pub fn fs_rename(
    state: State<'_, AppState>,
    path: String,
    new_name: String,
) -> Result<FileEntry, String> {
    state.with_app(|app| app.rename_workspace_entry(&path, &new_name))
}

#[tauri::command]
pub fn fs_delete_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.with_app(|app| app.delete_workspace_file(&path))
}

#[tauri::command]
pub fn fs_reveal(state: State<'_, AppState>, path: String, select: bool) -> Result<(), String> {
    state.with_app(|app| {
        ensure_local_workspace(app)?;
        let target = app.resolve_workspace_entry_for_shell(&path)?;
        reveal_path(&target, select).map_err(|e| format!("Cannot open file explorer: {e}"))
    })
}

fn ensure_local_workspace(app: &app_core::Application) -> Result<(), String> {
    if app.is_remote_workspace() {
        Err("Remote workspaces do not support local filesystem commands yet".into())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("explorer.exe");
    if select && path.is_file() {
        command.arg(format!("/select,{}", path.display()));
    } else {
        command.arg(path);
    }
    command.spawn().map(|_| ())
}

#[cfg(target_os = "macos")]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("open");
    if select && path.is_file() {
        command.arg("-R").arg(path);
    } else {
        command.arg(path);
    }
    command.spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_path(path: &Path, select: bool) -> std::io::Result<()> {
    let target = if select && path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
}
