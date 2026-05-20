use std::path::{Path, PathBuf};
use workspace_model::{FileEntry, FileEntryKind};

pub(crate) fn list_dir(root: &Path, path: &str) -> Result<Vec<FileEntry>, String> {
    let root = canonical_workspace_root(root)?;
    let target = resolve_existing_workspace_path(&root, path)?;

    if !target.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }

    let mut dirs: Vec<FileEntry> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();

    let entries =
        std::fs::read_dir(&target).map_err(|e| format!("Cannot read directory '{path}': {e}"))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Error reading entry: {e}"))?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Cannot determine type of '{file_name}': {e}"))?;
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
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    dirs.extend(files);
    Ok(dirs)
}

pub(crate) fn rename(root: &Path, path: &str, new_name: &str) -> Result<FileEntry, String> {
    let root = canonical_workspace_root(root)?;
    let source = resolve_existing_workspace_path(&root, path)?;
    let new_name = validate_new_name(new_name)?;
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

    std::fs::rename(&source, &target).map_err(|e| format!("Cannot rename '{path}': {e}"))?;
    let canonical_target = target
        .canonicalize()
        .map_err(|e| format!("Cannot resolve renamed path: {e}"))?;
    if !canonical_target.starts_with(&root) {
        return Err("Path traversal not allowed".to_string());
    }
    file_entry_from_path(&root, &canonical_target)
}

pub(crate) fn resolve_existing_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = canonical_workspace_root(root)?;
    resolve_existing_workspace_path(&root, path)
}

fn canonical_workspace_root(root: &Path) -> Result<PathBuf, String> {
    root.canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root: {e}"))
}

fn resolve_existing_workspace_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let target = if path.is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical_target = target
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path '{path}': {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_workspace(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kodex-workspace-files-{name}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn lists_directories_before_files() {
        let root = temp_workspace("list");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();

        let entries = list_dir(&root, "").unwrap();

        assert_eq!(entries[0].name, "src");
        assert_eq!(entries[0].kind, FileEntryKind::Directory);
        assert_eq!(entries[1].name, "Cargo.toml");
        assert_eq!(entries[1].kind, FileEntryKind::File);
    }

    #[test]
    fn rename_rejects_path_separator_in_new_name() {
        let root = temp_workspace("bad-name");
        fs::write(root.join("a.txt"), "").unwrap();

        let err = rename(&root, "a.txt", "../b.txt").unwrap_err();

        assert!(err.contains("single file or folder name"));
    }

    #[test]
    fn resolve_existing_path_rejects_traversal() {
        let root = temp_workspace("traversal");
        let outside = root.parent().unwrap().join("outside.txt");
        fs::write(&outside, "").unwrap();

        let err = resolve_existing_path(&root, "../outside.txt").unwrap_err();

        assert!(err.contains("Path traversal"));
    }
}
