use std::path::Path;
use workspace_model::SessionFileChange;

pub(super) fn upsert_loaded_change(items: &mut Vec<SessionFileChange>, item: SessionFileChange) {
    let normalized = normalize_change_path(&item.path);
    if let Some(existing) = items
        .iter_mut()
        .find(|change| normalize_change_path(&change.path) == normalized)
    {
        if item.new_text.len() >= existing.new_text.len() || item.timestamp >= existing.timestamp {
            *existing = item;
        }
    } else {
        items.push(item);
    }
}

pub(super) fn normalize_change_path(path: &str) -> String {
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

pub(super) fn normalize_workspace_root(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalize_change_path(&path.to_string_lossy())
}

pub(super) fn now_iso() -> String {
    // Simple UTC timestamp without chrono dependency
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();
    // Return as epoch seconds string (good enough for ordering)
    format!("{secs}")
}

pub(super) fn cap_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let boundary = (0..=max_bytes)
            .rev()
            .find(|index| s.is_char_boundary(*index))
            .unwrap_or(0);
        s[..boundary].to_string()
    }
}

pub(super) fn decode_json_vec<T>(json: Option<&str>) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    json.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}
