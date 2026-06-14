use super::*;

mod write_paths;

use write_paths::is_null_redirection_target;
pub(super) use write_paths::{
    extract_write_paths_from_command_text, is_usable_write_path,
    shell_command_directly_mutates_files,
};

pub(super) fn request_shell_write_should_retry_with_apply_patch(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> bool {
    let mut commands = Vec::new();
    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_shell_commands(raw_input, &mut commands);
    }
    if let Some(title) = &request.tool_call.fields.title {
        let title = title.trim();
        if !title.is_empty() {
            commands.push(title.to_string());
        }
    }

    commands
        .iter()
        .any(|command| shell_command_prefers_apply_patch_for_writes(workspace_root, command))
}

fn shell_command_write_paths_prefer_apply_patch(workspace_root: &str, command: &str) -> bool {
    let paths = resolve_paths_against_workspace(
        workspace_root,
        extract_write_paths_from_command_text(command)
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
    );
    !paths.is_empty()
        && paths_are_inside_workspace(workspace_root, &paths)
        && paths.iter().any(|path| path_prefers_apply_patch(path))
}

pub(super) fn resolve_paths_against_workspace(
    workspace_root: &str,
    paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let root = Path::new(workspace_root);
    paths
        .into_iter()
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .collect()
}

pub(in crate::runtime) fn shell_command_prefers_apply_patch_for_writes(
    workspace_root: &str,
    command: &str,
) -> bool {
    shell_command_directly_mutates_files(command)
        && !shell_command_write_is_apply_patch_exception(command)
        && shell_command_write_paths_prefer_apply_patch(workspace_root, command)
}

fn shell_command_write_is_apply_patch_exception(command: &str) -> bool {
    split_shell_pipeline(trim_shell_title(command))
        .iter()
        .any(|segment| {
            let words = shell_words(segment);
            let Some(command) = shell_command_word(&words) else {
                return false;
            };
            let command = shell_command_basename(command);
            matches!(
                command.as_str(),
                "cargo"
                    | "npm"
                    | "pnpm"
                    | "yarn"
                    | "bun"
                    | "deno"
                    | "go"
                    | "rustfmt"
                    | "prettier"
                    | "eslint"
                    | "biome"
                    | "ruff"
                    | "black"
                    | "clang-format"
                    | "taplo"
                    | "stylua"
                    | "npx"
            )
        })
}

pub(super) fn collect_shell_commands(value: &serde_json::Value, commands: &mut Vec<String>) {
    match value {
        serde_json::Value::String(command) => {
            if !command.trim().is_empty() {
                commands.push(command.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                commands.push(parts.join(" "));
            }
            for item in items {
                collect_shell_commands(item, commands);
            }
        }
        serde_json::Value::Object(object) => {
            for key in ["command", "cmd", "shell_command", "command_line", "args"] {
                if let Some(value) = object.get(key) {
                    collect_shell_commands(value, commands);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn shell_command_is_plan_read_only(command: &str) -> bool {
    let command = trim_shell_title(command);
    if command.is_empty()
        || shell_command_directly_mutates_files(command)
        || contains_forbidden_shell_control(command)
    {
        return false;
    }

    let segments = split_shell_pipeline(command);
    !segments.is_empty()
        && segments
            .iter()
            .all(|segment| shell_segment_is_plan_read_only(segment))
}

fn trim_shell_title(command: &str) -> &str {
    let trimmed = command.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('`') && trimmed.ends_with('`') {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    }
}

fn contains_forbidden_shell_control(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut index = 0;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            } else if byte == b'\\' {
                index += 1;
            }
            index += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b';' | b'`' => return true,
            b'&' if bytes.get(index + 1) == Some(&b'&') => return true,
            b'|' if bytes.get(index + 1) == Some(&b'|') => return true,
            b'$' if bytes.get(index + 1) == Some(&b'(') => return true,
            _ => {}
        }
        index += 1;
    }

    false
}

fn split_shell_pipeline(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            current.push(ch);
            if ch == active_quote {
                quote = None;
            } else if ch == '\\'
                && active_quote == '"'
                && let Some(next) = chars.next()
            {
                current.push(next);
            }
            continue;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            '|' => {
                let segment = current.trim();
                if !segment.is_empty() {
                    segments.push(segment.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let segment = current.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    segments
}

fn shell_segment_is_plan_read_only(segment: &str) -> bool {
    let words = shell_words(segment);
    let Some(command) = shell_command_word(&words) else {
        return false;
    };
    let command = shell_command_basename(command);

    match command.as_str() {
        "cat" | "cut" | "dir" | "egrep" | "fgrep" | "file" | "grep" | "head" | "less" | "ls"
        | "more" | "pwd" | "rg" | "sort" | "stat" | "tail" | "tree" | "type" | "uniq" | "wc"
        | "where" | "which" => true,
        "find" => shell_find_is_read_only(&words),
        "git" => shell_git_is_read_only(&words),
        "sed" => shell_sed_is_read_only(&words),
        "get-childitem" | "gci" | "ls.exe" | "get-content" | "gc" | "select-string"
        | "select-object" => true,
        _ => false,
    }
}

fn shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = segment.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else if ch == '\\' && active_quote == '"' {
                if let Some(next) = chars.peek().copied() {
                    if matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                        let _ = chars.next();
                        current.push(next);
                    } else {
                        current.push(ch);
                    }
                } else {
                    current.push(ch);
                }
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn shell_command_word(words: &[String]) -> Option<&str> {
    words.iter().map(String::as_str).find(|word| {
        !word.is_empty() && !word.contains('=') && !word.starts_with(|ch: char| ch.is_ascii_digit())
    })
}

fn shell_command_basename(command: &str) -> String {
    let basename = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_matches('`')
        .to_ascii_lowercase();
    basename
        .strip_suffix(".exe")
        .unwrap_or(&basename)
        .to_string()
}

fn shell_find_is_read_only(words: &[String]) -> bool {
    !words.iter().any(|word| {
        matches!(
            word.to_ascii_lowercase().as_str(),
            "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
        )
    })
}

fn shell_sed_is_read_only(words: &[String]) -> bool {
    !words
        .iter()
        .any(|word| word.to_ascii_lowercase().starts_with("-i"))
}

fn shell_git_is_read_only(words: &[String]) -> bool {
    let Some(subcommand) = words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .map(|word| word.to_ascii_lowercase())
    else {
        return false;
    };

    matches!(
        subcommand.as_str(),
        "blame" | "diff" | "grep" | "log" | "ls-files" | "rev-parse" | "show" | "status"
    )
}

pub(super) fn shell_command_absolute_paths_stay_inside_workspace(
    workspace_root: &str,
    command: &str,
) -> bool {
    let Some(paths) = shell_command_absolute_paths(command) else {
        return false;
    };

    paths.is_empty()
        || paths_are_inside_workspace(workspace_root, &paths)
        || (!Path::new(workspace_root).exists()
            && paths_are_lexically_inside_workspace(workspace_root, &paths))
}

fn shell_command_absolute_paths(command: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for segment in split_shell_pipeline(trim_shell_title(command)) {
        for word in shell_words(&segment) {
            let word = shell_path_word(&word);
            if word.is_empty() || is_null_redirection_target(word) || word.starts_with("/dev/") {
                continue;
            }
            if let Some(path) = normalize_shell_absolute_path(word) {
                paths.push(PathBuf::from(path));
            } else if word.starts_with('/') || word.starts_with("\\\\") {
                return None;
            }
        }
    }
    Some(paths)
}

fn shell_path_word(word: &str) -> &str {
    word.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | ':' | ';'))
}

fn normalize_shell_absolute_path(path: &str) -> Option<String> {
    if looks_windows_drive_path(path) || path.starts_with("\\\\") {
        return Some(path.to_string());
    }

    let normalized = normalize_unix_drive_prefix(path);
    if looks_windows_drive_path(&normalized) {
        return Some(normalized);
    }

    if Path::new(path).is_absolute() {
        return Some(path.to_string());
    }

    None
}

fn paths_are_lexically_inside_workspace(workspace_root: &str, paths: &[PathBuf]) -> bool {
    let root = normalize_lexical_permission_path(workspace_root);
    if root.is_empty() {
        return false;
    }

    paths.iter().all(|path| {
        let candidate = normalize_lexical_permission_path(&path.to_string_lossy());
        candidate == root || candidate.starts_with(&format!("{root}/"))
    })
}

fn normalize_lexical_permission_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn looks_windows_drive_path(path: &str) -> bool {
    let mut chars = path.chars();
    let Some(drive) = chars.next() else {
        return false;
    };
    drive.is_ascii_alphabetic() && chars.next() == Some(':')
}

fn normalize_unix_drive_prefix(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    for prefix in ["/mnt/", "/cygdrive/"] {
        if lower.starts_with(prefix) && normalized.len() > prefix.len() + 1 {
            let drive = normalized[prefix.len()..].chars().next().unwrap();
            let rest_start = prefix.len() + drive.len_utf8();
            if drive.is_ascii_alphabetic() && normalized[rest_start..].starts_with('/') {
                return format!(
                    "{}:{}",
                    drive.to_ascii_uppercase(),
                    &normalized[rest_start..]
                );
            }
        }
    }

    if normalized.len() > 2 && normalized.starts_with('/') {
        let mut chars = normalized.chars();
        let _slash = chars.next();
        if let Some(drive) = chars.next()
            && drive.is_ascii_alphabetic()
            && chars.next() == Some('/')
        {
            let rest_start = 1 + drive.len_utf8();
            return format!(
                "{}:{}",
                drive.to_ascii_uppercase(),
                &normalized[rest_start..]
            );
        }
    }

    path.to_string()
}
