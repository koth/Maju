use crate::state::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

#[derive(Debug, Clone, Serialize)]
pub enum FileEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub kind: FileEntryKind,
    pub path: String,
}

#[tauri::command]
pub fn fs_list_dir(state: State<'_, AppState>, path: String) -> Result<Vec<FileEntry>, String> {
    state.with_app(|app| {
        let root = &app.ui.workspace.root;

        // Resolve target directory: empty string means workspace root
        let target = if path.is_empty() {
            root.clone()
        } else {
            root.join(&path)
        };

        // Path safety: canonicalize and verify within workspace root
        let canonical_root = root
            .canonicalize()
            .map_err(|e| format!("Cannot resolve workspace root: {}", e))?;
        let canonical_target = target
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path '{}': {}", path, e))?;

        if !canonical_target.starts_with(&canonical_root) {
            return Err("Path traversal not allowed".to_string());
        }

        if !canonical_target.is_dir() {
            return Err(format!("Not a directory: {}", path));
        }

        let mut dirs: Vec<FileEntry> = Vec::new();
        let mut files: Vec<FileEntry> = Vec::new();

        let entries = std::fs::read_dir(&canonical_target)
            .map_err(|e| format!("Cannot read directory '{}': {}", path, e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Error reading entry: {}", e))?;
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files/dirs starting with '.'
            // (still include them — design says dotfiles included)

            let file_type = entry
                .file_type()
                .map_err(|e| format!("Cannot determine type of '{}': {}", file_name, e))?;

            // Build relative path from workspace root
            let entry_path = if path.is_empty() {
                file_name.clone()
            } else {
                format!("{}/{}", path.trim_end_matches('/'), file_name)
            };

            if file_type.is_dir() {
                dirs.push(FileEntry {
                    name: file_name,
                    kind: FileEntryKind::Directory,
                    path: entry_path,
                });
            } else if file_type.is_file() {
                files.push(FileEntry {
                    name: file_name,
                    kind: FileEntryKind::File,
                    path: entry_path,
                });
            }
            // Skip symlinks and other special entries
        }

        // Sort: directories first (alphabetical), then files (alphabetical)
        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        dirs.extend(files);
        Ok(dirs)
    })
}

#[tauri::command]
pub fn fs_rename(
    state: State<'_, AppState>,
    path: String,
    new_name: String,
) -> Result<FileEntry, String> {
    state.with_app(|app| {
        let root = canonical_workspace_root(&app.ui.workspace.root)?;
        let source = resolve_existing_workspace_path(&root, &path)?;
        let new_name = validate_new_name(&new_name)?;
        let parent = source
            .parent()
            .ok_or_else(|| "Cannot rename workspace root".to_string())?;
        let target = parent.join(new_name);

        if !target.starts_with(&root) {
            return Err("Path traversal not allowed".to_string());
        }
        if target.exists() {
            return Err("A file or folder with that name already exists".to_string());
        }

        std::fs::rename(&source, &target)
            .map_err(|e| format!("Cannot rename '{}': {}", path, e))?;
        let canonical_target = target
            .canonicalize()
            .map_err(|e| format!("Cannot resolve renamed path: {}", e))?;
        if !canonical_target.starts_with(&root) {
            return Err("Path traversal not allowed".to_string());
        }
        file_entry_from_path(&root, &canonical_target)
    })
}

#[tauri::command]
pub fn fs_reveal(state: State<'_, AppState>, path: String, select: bool) -> Result<(), String> {
    state.with_app(|app| {
        let root = canonical_workspace_root(&app.ui.workspace.root)?;
        let target = resolve_existing_workspace_path(&root, &path)?;
        reveal_path(&target, select).map_err(|e| format!("Cannot open file explorer: {}", e))
    })
}

fn canonical_workspace_root(root: &Path) -> Result<PathBuf, String> {
    root.canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root: {}", e))
}

fn resolve_existing_workspace_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let target = if path.is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical_target = target
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path '{}': {}", path, e))?;
    if !canonical_target.starts_with(root) {
        return Err("Path traversal not allowed".to_string());
    }
    Ok(canonical_target)
}

fn validate_new_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        return Err("Name must be a single file or folder name".to_string());
    }
    Ok(trimmed)
}

fn file_entry_from_path(root: &Path, full_path: &Path) -> Result<FileEntry, String> {
    let name = full_path
        .file_name()
        .ok_or_else(|| "Cannot derive file name".to_string())?
        .to_string_lossy()
        .to_string();
    let metadata = std::fs::metadata(full_path)
        .map_err(|e| format!("Cannot inspect '{}': {}", full_path.display(), e))?;
    let kind = if metadata.is_dir() {
        FileEntryKind::Directory
    } else if metadata.is_file() {
        FileEntryKind::File
    } else {
        return Err("Unsupported filesystem entry".to_string());
    };
    let relative = full_path
        .strip_prefix(root)
        .map_err(|_| "Path traversal not allowed".to_string())?;
    Ok(FileEntry {
        name,
        kind,
        path: normalize_relative_path(relative),
    })
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
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
