use std::path::Path;

pub fn normalize_tracked_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("//?/")
        .or_else(|| normalized.strip_prefix("//./"))
        .unwrap_or(&normalized)
        .to_string();
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let mut chars: Vec<char> = normalized.chars().collect();
        chars[0] = chars[0].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        normalized
    }
}

pub fn normalize_path_for_storage(path: &str, workspace_root: &Path) -> String {
    let normalized = normalize_tracked_path(path);
    let ws_root = normalize_tracked_path(&workspace_root.display().to_string());
    let ws_prefix = if ws_root.ends_with('/') {
        ws_root
    } else {
        format!("{}/", ws_root)
    };
    normalized
        .strip_prefix(&ws_prefix)
        .unwrap_or(&normalized)
        .to_string()
}
