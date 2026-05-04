use crate::state::AppState;
use serde::Serialize;
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
