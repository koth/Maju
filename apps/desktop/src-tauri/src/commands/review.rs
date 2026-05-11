use crate::state::AppState;
use app_core::normalize_tracked_path;
use git2::Repository;
use std::path::Path;
use tauri::State;
use workspace_model::{ChangedFile, DiffStats, FileChangeType, SessionFileChange};

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

    let repo = Repository::discover(&ws_root).map_err(|e| format!("failed to open repo: {e}"))?;
    let workdir = repo.workdir().ok_or("no workdir")?;

    let rel_path = {
        let normalized = normalize_tracked_path(&path);
        let ws_norm = normalize_tracked_path(&workdir.display().to_string());
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
    let snapshot_stats = state
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
                .map(|file| file.stats.clone()))
        })
        .unwrap_or(None);

    let full_path = workdir.join(&rel_path);

    let new_text = if full_path.exists() {
        std::fs::read_to_string(&full_path).unwrap_or_default()
    } else {
        String::new()
    };

    let old_text = (|| -> Option<String> {
        let head = repo.head().ok()?;
        let tree = head.peel_to_tree().ok()?;
        let entry = tree.get_path(Path::new(&rel_path)).ok()?;
        let blob = entry.to_object(&repo).ok()?;
        let blob = blob.peel_to_blob().ok()?;
        String::from_utf8(blob.content().to_vec()).ok()
    })();

    let change_type = if old_text.is_none() && !new_text.is_empty() {
        FileChangeType::Created
    } else if old_text.is_some() && new_text.is_empty() {
        FileChangeType::Deleted
    } else {
        FileChangeType::Modified
    };

    let stats =
        snapshot_stats.unwrap_or_else(|| diff_stats_from_text(old_text.as_deref(), &new_text));

    Ok(Some(SessionFileChange {
        path: rel_path,
        change_type,
        old_text,
        new_text,
        added_lines: stats.added,
        removed_lines: stats.removed,
        timestamp: String::new(),
    }))
}

fn diff_stats_from_text(old_text: Option<&str>, new_text: &str) -> DiffStats {
    let hunks = acp_core::diff_to_hunks(old_text, new_text);
    DiffStats {
        added: hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| matches!(line.kind, workspace_model::DiffLineKind::Added))
            .count(),
        removed: hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| matches!(line.kind, workspace_model::DiffLineKind::Removed))
            .count(),
    }
}

#[cfg(test)]
mod tests {
    use super::diff_stats_from_text;

    #[test]
    fn diff_stats_count_changed_lines_not_file_lengths() {
        let old_text = (1..=890)
            .map(|line| {
                if line == 10 {
                    "old a".to_string()
                } else if line == 20 {
                    "old b".to_string()
                } else {
                    format!("same {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let new_text = old_text
            .replace("old a", "new a")
            .replace("old b", "new b\nnew c\nnew d");

        let stats = diff_stats_from_text(Some(&old_text), &new_text);

        assert_eq!(stats.added, 4);
        assert_eq!(stats.removed, 2);
    }
}
