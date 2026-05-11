use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;
use tauri::State;

const MAX_EDITABLE_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorFileVersion {
    pub content_hash: String,
    pub modified_ms: Option<u128>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorFileKind {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorFileSnapshot {
    pub path: String,
    pub content: String,
    pub version: EditorFileVersion,
    pub kind: EditorFileKind,
    pub mime_type: Option<String>,
}

#[tauri::command]
pub fn editor_open_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| read_file_snapshot(&app.ui.workspace.root, &path))
}

#[tauri::command]
pub fn editor_save_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
    base_version: Option<EditorFileVersion>,
    overwrite: Option<bool>,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| {
        let before_text = match read_file_snapshot(&app.ui.workspace.root, &path) {
            Ok(snapshot) => Some(snapshot.content),
            Err(_) => None,
        };
        save_file_snapshot(
            &app.ui.workspace.root,
            &path,
            &content,
            base_version.as_ref(),
            overwrite.unwrap_or(false),
        )
        .map(|snapshot| {
            app.record_manual_editor_save(&snapshot.path, before_text, snapshot.content.clone());
            app.refresh_repository();
            snapshot
        })
    })
}

#[tauri::command]
pub fn editor_get_content(
    state: State<'_, AppState>,
    path: String,
) -> Result<EditorFileSnapshot, String> {
    state.with_app(|app| read_file_snapshot(&app.ui.workspace.root, &path))
}

fn read_file_snapshot(root: &Path, path: &str) -> Result<EditorFileSnapshot, String> {
    let full_path = resolve_workspace_path(root, path, true)?;
    if !full_path.is_file() {
        return Err(format!("Not a file: {}", full_path.display()));
    }

    let metadata = fs::metadata(&full_path)
        .map_err(|e| format!("Cannot stat {}: {}", full_path.display(), e))?;
    if metadata.len() > MAX_EDITABLE_FILE_BYTES {
        return Err(format!(
            "File is too large for safe editing: {} ({} bytes)",
            full_path.display(),
            metadata.len()
        ));
    }

    let bytes =
        fs::read(&full_path).map_err(|e| format!("Cannot read {}: {}", full_path.display(), e))?;
    let version = file_version_from_bytes(&full_path, &bytes)?;
    if let Some(mime_type) = image_mime_type(&full_path) {
        return Ok(EditorFileSnapshot {
            path: normalize_workspace_relative_path(root, &full_path)?,
            content: format!("data:{mime_type};base64,{}", base64_encode(&bytes)),
            version,
            kind: EditorFileKind::Image,
            mime_type: Some(mime_type.to_string()),
        });
    }

    if bytes.contains(&0) {
        return Err(format!(
            "Binary file cannot be edited safely: {}",
            full_path.display()
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        format!(
            "Non-UTF-8 file cannot be edited safely: {}",
            full_path.display()
        )
    })?;

    Ok(EditorFileSnapshot {
        path: normalize_workspace_relative_path(root, &full_path)?,
        content,
        version,
        kind: EditorFileKind::Text,
        mime_type: None,
    })
}

fn save_file_snapshot(
    root: &Path,
    path: &str,
    content: &str,
    base_version: Option<&EditorFileVersion>,
    overwrite: bool,
) -> Result<EditorFileSnapshot, String> {
    let full_path = resolve_workspace_path(root, path, false)?;
    let parent = full_path
        .parent()
        .ok_or_else(|| format!("Invalid file path: {}", full_path.display()))?;
    if !parent.exists() {
        return Err(format!("Directory not found: {}", parent.display()));
    }

    if let Some(base_version) = base_version {
        match current_version(&full_path) {
            Ok(current) if current != *base_version && !overwrite => {
                return Err(format!("File changed on disk: {}", full_path.display()));
            }
            Err(error) if !overwrite => {
                return Err(format!("File missing on disk: {error}"));
            }
            _ => {}
        }
    }

    fs::write(&full_path, content)
        .map_err(|e| format!("Cannot write {}: {}", full_path.display(), e))?;
    read_file_snapshot(root, path)
}

fn current_version(path: &Path) -> Result<EditorFileVersion, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
    file_version_from_bytes(path, &bytes)
}

fn file_version_from_bytes(path: &Path, bytes: &[u8]) -> Result<EditorFileVersion, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("Cannot stat {}: {}", path.display(), e))?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis());

    Ok(EditorFileVersion {
        content_hash: fnv1a64_hex(bytes),
        modified_ms,
        size: metadata.len(),
    })
}

fn image_mime_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        "ico" => Some("image/x-icon"),
        _ => None,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        output.push(TABLE[(b0 >> 2) as usize] as char);
        output.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn resolve_workspace_path(root: &Path, path: &str, must_exist: bool) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("Path is empty".into());
    }

    reject_parent_traversal(Path::new(path))?;

    let root = root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root {}: {}", root.display(), e))?;
    let requested = PathBuf::from(path);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    };

    if must_exist {
        let resolved = candidate
            .canonicalize()
            .map_err(|e| format!("File not found: {} ({})", candidate.display(), e))?;
        ensure_inside_workspace(&root, &resolved)?;
        return Ok(resolved);
    }

    let parent = candidate
        .parent()
        .ok_or_else(|| format!("Invalid file path: {}", candidate.display()))?;
    let resolved_parent = parent
        .canonicalize()
        .map_err(|e| format!("Directory not found: {} ({})", parent.display(), e))?;
    ensure_inside_workspace(&root, &resolved_parent)?;
    Ok(resolved_parent.join(
        candidate
            .file_name()
            .ok_or_else(|| format!("Invalid file path: {}", candidate.display()))?,
    ))
}

fn reject_parent_traversal(path: &Path) -> Result<(), String> {
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!("Path escapes workspace: {}", path.display()));
        }
    }
    Ok(())
}

fn ensure_inside_workspace(root: &Path, path: &Path) -> Result<(), String> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(format!("Path outside workspace: {}", path.display()))
    }
}

fn normalize_workspace_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root {}: {}", root.display(), e))?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| format!("Path outside workspace: {}", path.display()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
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
        let root = std::env::temp_dir().join(format!("kodex-editor-{name}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn reads_file_snapshot_with_version() {
        let root = temp_workspace("read");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let snapshot = read_file_snapshot(&root, "main.rs").unwrap();

        assert_eq!(snapshot.path, "main.rs");
        assert_eq!(snapshot.content, "fn main() {}\n");
        assert_eq!(snapshot.kind, EditorFileKind::Text);
        assert_eq!(snapshot.mime_type, None);
        assert_eq!(snapshot.version.size, 13);
        assert!(!snapshot.version.content_hash.is_empty());
    }

    #[test]
    fn saves_file_when_base_version_matches() {
        let root = temp_workspace("save");
        fs::write(root.join("main.rs"), "old\n").unwrap();
        let before = read_file_snapshot(&root, "main.rs").unwrap();

        let after =
            save_file_snapshot(&root, "main.rs", "new\n", Some(&before.version), false).unwrap();

        assert_eq!(after.content, "new\n");
        assert_ne!(after.version.content_hash, before.version.content_hash);
        assert_eq!(fs::read_to_string(root.join("main.rs")).unwrap(), "new\n");
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_workspace("traversal");
        let outside = root.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside").unwrap();

        let err = read_file_snapshot(&root, "../outside.txt").unwrap_err();

        assert!(err.contains("escapes workspace"));
    }

    #[test]
    fn rejects_absolute_path_outside_workspace() {
        let root = temp_workspace("absolute");
        let outside_root = temp_workspace("outside");
        let outside = outside_root.join("outside.txt");
        fs::write(&outside, "outside").unwrap();

        let err = read_file_snapshot(&root, &outside.display().to_string()).unwrap_err();

        assert!(err.contains("outside workspace"));
    }

    #[test]
    fn rejects_missing_parent_directory_on_save() {
        let root = temp_workspace("missing-parent");

        let err = save_file_snapshot(&root, "missing/file.rs", "content", None, false).unwrap_err();

        assert!(err.contains("Directory not found"));
    }

    #[test]
    fn rejects_save_when_disk_changed() {
        let root = temp_workspace("conflict");
        fs::write(root.join("main.rs"), "base\n").unwrap();
        let before = read_file_snapshot(&root, "main.rs").unwrap();
        fs::write(root.join("main.rs"), "external\n").unwrap();

        let err = save_file_snapshot(&root, "main.rs", "mine\n", Some(&before.version), false)
            .unwrap_err();

        assert!(err.contains("changed on disk"));
        assert_eq!(
            fs::read_to_string(root.join("main.rs")).unwrap(),
            "external\n"
        );
    }

    #[test]
    fn overwrite_allows_save_when_disk_changed() {
        let root = temp_workspace("overwrite");
        fs::write(root.join("main.rs"), "base\n").unwrap();
        let before = read_file_snapshot(&root, "main.rs").unwrap();
        fs::write(root.join("main.rs"), "external\n").unwrap();

        let after =
            save_file_snapshot(&root, "main.rs", "mine\n", Some(&before.version), true).unwrap();

        assert_eq!(after.content, "mine\n");
    }

    #[test]
    fn rejects_save_when_file_was_deleted() {
        let root = temp_workspace("deleted");
        fs::write(root.join("main.rs"), "base\n").unwrap();
        let before = read_file_snapshot(&root, "main.rs").unwrap();
        fs::remove_file(root.join("main.rs")).unwrap();

        let err = save_file_snapshot(&root, "main.rs", "mine\n", Some(&before.version), false)
            .unwrap_err();

        assert!(err.contains("missing on disk"));
    }

    #[test]
    fn rejects_binary_file_on_open() {
        let root = temp_workspace("binary");
        fs::write(root.join("image.bin"), [0_u8, 159, 146, 150]).unwrap();

        let err = read_file_snapshot(&root, "image.bin").unwrap_err();

        assert!(err.contains("Binary file"));
    }

    #[test]
    fn opens_image_file_for_preview() {
        let root = temp_workspace("image-preview");
        fs::write(root.join("logo.png"), [137_u8, 80, 78, 71, 13, 10, 26, 10]).unwrap();

        let snapshot = read_file_snapshot(&root, "logo.png").unwrap();

        assert_eq!(snapshot.path, "logo.png");
        assert_eq!(snapshot.kind, EditorFileKind::Image);
        assert_eq!(snapshot.mime_type.as_deref(), Some("image/png"));
        assert!(snapshot.content.starts_with("data:image/png;base64,"));
        assert_eq!(snapshot.version.size, 8);
    }

    #[test]
    fn rejects_large_file_on_open() {
        let root = temp_workspace("large");
        fs::write(
            root.join("large.txt"),
            vec![b'a'; (MAX_EDITABLE_FILE_BYTES + 1) as usize],
        )
        .unwrap();

        let err = read_file_snapshot(&root, "large.txt").unwrap_err();

        assert!(err.contains("too large"));
    }
}
