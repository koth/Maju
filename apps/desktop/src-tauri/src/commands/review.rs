use crate::state::AppState;
use app_core::normalize_tracked_path;
use git_service::GitService;
use tauri::State;
use workspace_model::{ChangedFile, SessionFileChange};

fn normalize_to_ws_relative(path: &str, ws_root: &str) -> String {
    let normalized = normalize_tracked_path(path);
    let ws_norm = normalize_tracked_path(ws_root);
    let ws_prefix = if ws_norm.ends_with('/') {
        ws_norm
    } else {
        format!("{}/", ws_norm)
    };
    normalized
        .strip_prefix(&ws_prefix)
        .unwrap_or(&normalized)
        .to_string()
}

#[tauri::command]
pub fn review_get_diff(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<ChangedFile>, String> {
    state.with_app(|app| {
        let normalized = normalize_tracked_path(&path);
        let ws_root = app.ui.workspace.root.display().to_string();
        let file = app
            .ui
            .repository
            .changed_files
            .iter()
            .find(|f| {
                let display = f.path.display().to_string();
                normalize_tracked_path(&display) == normalized
                    || normalize_to_ws_relative(&display, &ws_root) == normalized
            })
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

#[tauri::command]
pub fn review_get_git_diff_content(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<SessionFileChange>, String> {
    let ws_root = state
        .with_app(|app| Ok(app.ui.workspace.root.clone()))
        .unwrap_or_default();

    if ws_root.as_os_str().is_empty() {
        return Ok(None);
    }

    let rel_path = {
        let normalized = normalize_tracked_path(&path);
        let ws_norm = normalize_tracked_path(&ws_root.display().to_string());
        let ws_prefix = if ws_norm.ends_with('/') {
            ws_norm
        } else {
            format!("{}/", ws_norm)
        };
        normalized
            .strip_prefix(&ws_prefix)
            .unwrap_or(&normalized)
            .to_string()
    };
    let snapshot_section = state
        .with_app(|app| {
            Ok(app
                .ui
                .repository
                .changed_files
                .iter()
                .find(|file| {
                    normalize_tracked_path(&file.path.display().to_string())
                        == normalize_tracked_path(&rel_path)
                })
                .map(|file| file.section.clone()))
        })
        .unwrap_or(None);

    let record = if let Some(section) = snapshot_section {
        GitService::file_diff(&ws_root, &rel_path, section)
            .map_err(|e| format!("failed to load git diff: {e}"))?
    } else {
        GitService::file_diff_auto(&ws_root, &rel_path)
            .map_err(|e| format!("failed to load git diff: {e}"))?
    };

    Ok(record.map(|record| SessionFileChange {
        path: record.path,
        change_type: record.change_type,
        old_text: record.old_text,
        new_text: record.new_text.unwrap_or_default(),
        added_lines: record.added_lines,
        removed_lines: record.removed_lines,
        timestamp: record.updated_at,
    }))
}
