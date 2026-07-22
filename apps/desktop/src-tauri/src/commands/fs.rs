use crate::state::AppState;
use std::path::Path;
use tauri::{AppHandle, Manager, State};
use workspace_model::{FileEntry, FileEntryKind};

const MAX_MENTION_DIR_ENTRIES: usize = 60;

#[tauri::command]
pub async fn fs_mention_suggest(app: AppHandle, query: String) -> Result<Vec<FileEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let query = query.trim().to_string();

        // Drill-down: the query contains a path separator, so list that
        // directory and filter its direct children by the trailing prefix.
        // This makes `@apps/desktop/comp` browse into `apps/desktop`.
        if let Some(slash) = query.rfind('/') {
            let dir = &query[..slash];
            let prefix = query[slash + 1..].to_lowercase();
            let entries = state.list_workspace_dir(dir.to_string()).unwrap_or_default();
            return Ok(filter_mention_dir_entries(entries, &prefix));
        }

        // Flat: project-wide fuzzy match across files and directories.
        // Remote workspaces delegate to their search endpoint (files only);
        // local workspaces walk the tree cheaply without spawning ripgrep.
        let remote_result = state.with_app(|app| {
            if app.is_remote_workspace() {
                app.search_workspace(&query).map(Some)
            } else {
                Ok(None)
            }
        })?;
        if let Some(result) = remote_result {
            return Ok(result
                .file_suggestions
                .into_iter()
                .map(|suggestion| FileEntry {
                    name: suggestion.name,
                    kind: FileEntryKind::File,
                    path: suggestion.path,
                })
                .collect());
        }

        let workspace_root = state.with_app(|app| Ok(app.ui.workspace.root.clone()))?;
        Ok(crate::commands::search::collect_mention_suggestions(
            &workspace_root,
            &query,
        ))
    })
    .await
    .map_err(|e| format!("Mention suggest task failed: {e}"))?
}

fn filter_mention_dir_entries(entries: Vec<FileEntry>, prefix: &str) -> Vec<FileEntry> {
    if prefix.is_empty() {
        return entries.into_iter().take(MAX_MENTION_DIR_ENTRIES).collect();
    }
    entries
        .into_iter()
        .filter(|entry| entry.name.to_lowercase().starts_with(&prefix))
        .take(MAX_MENTION_DIR_ENTRIES)
        .collect()
}

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

/// Cheap existence check used by the chat renderer to decide whether an
/// inline-code span is a real, openable workspace file before rendering it
/// as a clickable link. Returns false for anything outside the workspace,
/// missing, or not a regular file — never errors.
#[tauri::command]
pub async fn fs_path_exists(app: AppHandle, paths: Vec<String>) -> Result<Vec<bool>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(paths
            .into_iter()
            .map(|path| {
                state
                    .with_app(|app| {
                        if app.is_remote_workspace() {
                            // Remote existence probing is not wired; be
                            // permissive so remote chat links still render.
                            return Ok(true);
                        }
                        Ok(app
                            .resolve_workspace_entry_for_shell(&path)
                            .map(|target| target.is_file())
                            .unwrap_or(false))
                    })
                    .unwrap_or(false)
            })
            .collect::<Vec<bool>>())
    })
    .await
    .map_err(|e| format!("Path exists task failed: {e}"))?
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
