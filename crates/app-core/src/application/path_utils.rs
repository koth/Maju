use std::path::Path;

pub fn normalize_tracked_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("//?/")
        .or_else(|| normalized.strip_prefix("//./"))
        .unwrap_or(&normalized)
        .to_string();
    let normalized = normalize_unix_drive_prefix(&normalized);
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

fn normalize_unix_drive_prefix(path: &str) -> String {
    let trimmed = path.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["/mnt/", "/cygdrive/"] {
        if lower.starts_with(prefix) && trimmed.len() > prefix.len() + 1 {
            let drive = trimmed[prefix.len()..].chars().next().unwrap();
            let rest_start = prefix.len() + drive.len_utf8();
            if drive.is_ascii_alphabetic() && trimmed[rest_start..].starts_with('/') {
                return format!("{}:{}", drive.to_ascii_lowercase(), &trimmed[rest_start..]);
            }
        }
    }

    if trimmed.len() > 2 && trimmed.starts_with('/') {
        let mut chars = trimmed.chars();
        let _slash = chars.next();
        if let Some(drive) = chars.next()
            && drive.is_ascii_alphabetic()
            && chars.next() == Some('/')
        {
            let rest_start = 1 + drive.len_utf8();
            return format!("{}:{}", drive.to_ascii_lowercase(), &trimmed[rest_start..]);
        }
    }

    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn normalizes_unix_drive_prefix_against_windows_workspace_root() {
        let root = Path::new(r"D:\work\ArtAssets");

        assert_eq!(
            normalize_path_for_storage("/d/work/ArtAssets/docs/tags.md", root),
            "docs/tags.md"
        );
        assert_eq!(
            normalize_path_for_storage("/mnt/d/work/ArtAssets/docs/tags.md", root),
            "docs/tags.md"
        );
    }
}
