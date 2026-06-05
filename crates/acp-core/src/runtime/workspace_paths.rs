use agent_client_protocol::schema::{ReadTextFileRequest, WriteTextFileRequest};
use anyhow::{Context, anyhow};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn read_workspace_text_file(
    workspace_root: &str,
    request: &ReadTextFileRequest,
) -> anyhow::Result<String> {
    let path = validate_client_file_path(workspace_root, &request.path)?;

    if path.is_dir() {
        return list_workspace_directory(&path);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read text file {}", path.display()))?;

    let selected = select_lines(&content, request.line, request.limit);
    Ok(selected)
}

pub(super) fn write_workspace_text_file(
    workspace_root: &str,
    request: &WriteTextFileRequest,
) -> anyhow::Result<()> {
    let path = validate_client_file_path(workspace_root, &request.path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    fs::write(&path, &request.content)
        .with_context(|| format!("failed to write text file {}", path.display()))?;
    Ok(())
}

pub(super) fn normalize_path(path: PathBuf) -> PathBuf {
    if path.exists() {
        return path.canonicalize().unwrap_or(path);
    }

    lexical_normalize(path)
}

pub(super) fn paths_are_inside_workspace(workspace_root: &str, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }

    let Ok(root) = PathBuf::from(workspace_root).canonicalize() else {
        return false;
    };

    paths.iter().all(|path| {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let normalized = lexical_normalize(candidate);
        resolve_for_workspace_check(&normalized)
            .map(|resolved| resolved.starts_with(&root))
            .unwrap_or(false)
    })
}

fn list_workspace_directory(path: &PathBuf) -> anyhow::Result<String> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to enumerate directory {}", path.display()))?;

    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());

    let listing = entries
        .into_iter()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let suffix = match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => "/",
                _ => "",
            };
            format!("{name}{suffix}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(listing)
}

pub(super) fn validate_workspace_path(
    workspace_root: &str,
    requested_path: &Path,
) -> anyhow::Result<PathBuf> {
    let workspace_root = PathBuf::from(workspace_root)
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace root {workspace_root}"))?;

    let candidate = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        workspace_root.join(requested_path)
    };

    let normalized = lexical_normalize(candidate);
    let resolved = resolve_for_workspace_check(&normalized)?;
    if !resolved.starts_with(&workspace_root) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

pub(super) fn validate_client_file_path(
    workspace_root: &str,
    requested_path: &Path,
) -> anyhow::Result<PathBuf> {
    validate_workspace_path(workspace_root, requested_path)
        .or_else(|_| validate_codebuddy_plan_path(requested_path))
}

fn validate_codebuddy_plan_path(requested_path: &Path) -> anyhow::Result<PathBuf> {
    if !requested_path.is_absolute() {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            requested_path.display()
        ));
    }

    let normalized = lexical_normalize(requested_path.to_path_buf());
    if !matches!(
        normalized
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase()),
        Some(extension) if matches!(extension.as_str(), "md" | "mdx")
    ) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    let plan_root = resolve_for_workspace_check(&codebuddy_plan_root()?)?;
    let resolved = resolve_for_workspace_check(&normalized)?;
    if !resolved.starts_with(&plan_root) {
        return Err(anyhow!(
            "ACP file request is outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

fn codebuddy_plan_root() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow!("failed to resolve user home directory"))?;
    Ok(PathBuf::from(home).join(".codebuddy").join("plans"))
}

fn resolve_for_workspace_check(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", path.display()));
    }

    let mut ancestor = path;
    let mut missing_components = Vec::<OsString>::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return Err(anyhow!("failed to resolve path {}", path.display()));
        };
        missing_components.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("failed to resolve path {}", path.display()))?;
    }

    let mut resolved = ancestor
        .canonicalize()
        .with_context(|| format!("failed to resolve path {}", ancestor.display()))?;
    for component in missing_components.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn lexical_normalize(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn select_lines(content: &str, start_line: Option<u32>, limit: Option<u32>) -> String {
    let Some(start_line) = start_line else {
        return content.to_string();
    };

    let start_index = start_line.saturating_sub(1) as usize;
    let max_lines = limit.unwrap_or(u32::MAX) as usize;

    content
        .lines()
        .skip(start_index)
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn path_permission_check_rejects_empty_path_sets() {
        assert!(!paths_are_inside_workspace("workspace-root", &[]));
    }

    #[test]
    fn path_permission_check_handles_nonexistent_children_inside_workspace() {
        let root = temp_workspace("inside");

        assert!(paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("new/nested/file.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn path_permission_check_rejects_parent_escape() {
        let root = temp_workspace("escape");

        assert!(!paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("../outside.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_allows_codebuddy_plan_markdown() {
        let root = temp_workspace("codebuddy-plan");
        let plan = codebuddy_plan_root().unwrap().join("draft.md");

        assert_eq!(
            validate_client_file_path(root.to_str().unwrap(), &plan).unwrap(),
            lexical_normalize(plan),
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_rejects_non_plan_home_file() {
        let root = temp_workspace("codebuddy-non-plan");
        let path = codebuddy_plan_root()
            .unwrap()
            .parent()
            .unwrap()
            .join("secret.md");

        assert!(validate_client_file_path(root.to_str().unwrap(), &path).is_err());

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn client_file_path_rejects_non_markdown_codebuddy_plan_file() {
        let root = temp_workspace("codebuddy-non-markdown");
        let path = codebuddy_plan_root().unwrap().join("draft.txt");

        assert!(validate_client_file_path(root.to_str().unwrap(), &path).is_err());

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn path_permission_check_rejects_symlink_parent_escape() {
        let root = temp_workspace("symlink");
        let outside = root.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked-out")).unwrap();

        assert!(!paths_are_inside_workspace(
            root.to_str().unwrap(),
            &[PathBuf::from("linked-out/file.txt")]
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("kodex-acp-paths-{label}-{unique}"))
            .join("workspace");
        fs::create_dir_all(&root).unwrap();
        root
    }
}
