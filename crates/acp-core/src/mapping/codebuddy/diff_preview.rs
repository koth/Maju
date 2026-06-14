use super::*;

pub(super) fn emit_codebuddy_diff_content(
    tx: &mpsc::Sender<ClientEvent>,
    workspace_root: &str,
    id: &str,
    update: &Value,
) -> anyhow::Result<()> {
    let Some(items) = update.get("content").and_then(Value::as_array) else {
        return Ok(());
    };

    for item in items {
        let Some(path) = codebuddy_content_path(item) else {
            continue;
        };
        let new_text = codebuddy_content_new_text(workspace_root, path, item, update);
        let Some(new_text) = new_text else {
            continue;
        };
        let fallback_old_text = item
            .get("oldText")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let old_text =
            edit_preview_old_text_from_raw_input(update.get("rawInput"), Some(&new_text))
                .or(fallback_old_text);

        tx.send(ClientEvent::ToolDiff {
            id: id.to_string(),
            path: path.to_string(),
            old_text,
            new_text,
        })
        .map_err(|_| anyhow!("failed to emit CodeBuddy diff content"))?;
    }

    Ok(())
}

pub(in crate::mapping) fn emit_tool_diff_previews_from_raw_output(
    tx: &mpsc::Sender<ClientEvent>,
    id: &str,
    raw_output: Option<&Value>,
) -> anyhow::Result<()> {
    let Some(changes) = raw_output
        .and_then(|value| value.get("changes"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };

    for (path, change) in changes {
        let Some(unified_diff) = change
            .get("unified_diff")
            .or_else(|| change.get("unifiedDiff"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let hunks = hunks_from_unified_diff(unified_diff);
        if hunks.is_empty() {
            continue;
        }
        let display_path = change
            .get("move_path")
            .or_else(|| change.get("movePath"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .unwrap_or(path);

        tx.send(ClientEvent::ToolDiffPreview {
            id: id.to_string(),
            path: display_path.to_string(),
            hunks,
        })
        .map_err(|_| anyhow!("failed to emit raw output diff preview"))?;
    }

    Ok(())
}

fn hunks_from_unified_diff(unified_diff: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::<DiffHunk>::new();
    let mut current: Option<DiffHunk> = None;

    for line in unified_diff.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take()
                && hunk.lines.iter().any(is_changed_diff_line)
            {
                hunks.push(hunk);
            }
            current = Some(DiffHunk {
                heading: line.to_string(),
                lines: Vec::new(),
            });
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            continue;
        };
        if line == r"\ No newline at end of file" {
            continue;
        }

        if let Some(content) = line.strip_prefix('+') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Added,
                content: content.to_string(),
            });
        } else if let Some(content) = line.strip_prefix('-') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                content: content.to_string(),
            });
        } else if let Some(content) = line.strip_prefix(' ') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                content: content.to_string(),
            });
        } else if line.is_empty() {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                content: String::new(),
            });
        }
    }

    if let Some(hunk) = current
        && hunk.lines.iter().any(is_changed_diff_line)
    {
        hunks.push(hunk);
    }

    hunks
}

fn is_changed_diff_line(line: &DiffLine) -> bool {
    matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed)
}

fn codebuddy_content_path(item: &Value) -> Option<&str> {
    codebuddy_content_path_from_value(item).or_else(|| {
        item.get("content")
            .and_then(codebuddy_content_path_from_value)
    })
}

fn codebuddy_content_path_from_value(value: &Value) -> Option<&str> {
    value
        .get("path")
        .or_else(|| value.get("file_path"))
        .or_else(|| value.get("filePath"))
        .and_then(Value::as_str)
}

fn codebuddy_content_new_text(
    workspace_root: &str,
    path: &str,
    item: &Value,
    update: &Value,
) -> Option<String> {
    codebuddy_content_new_text_from_value(item)
        .or_else(|| {
            item.get("content")
                .and_then(codebuddy_content_new_text_from_value)
        })
        .or_else(|| edit_preview_new_text_from_raw_input(update.get("rawInput")))
        .or_else(|| read_codebuddy_workspace_text(workspace_root, path))
}

fn codebuddy_content_new_text_from_value(value: &Value) -> Option<String> {
    value
        .get("newText")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn read_codebuddy_workspace_text(workspace_root: &str, path: &str) -> Option<String> {
    if workspace_root.trim().is_empty() || path.trim().is_empty() {
        return None;
    }

    let root = PathBuf::from(workspace_root).canonicalize().ok()?;
    let candidate = PathBuf::from(normalize_unix_drive_prefix(path));
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        root.join(&candidate)
    };
    let candidate = candidate.canonicalize().ok()?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return None;
    }

    fs::read_to_string(candidate).ok()
}

pub(in crate::mapping) fn normalize_unix_drive_prefix(path: &str) -> String {
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

pub(in crate::mapping) fn edit_preview_new_text_from_raw_input(
    raw_input: Option<&Value>,
) -> Option<String> {
    let raw_input = raw_input?;
    let before = edit_preview_before_text(raw_input)?;
    let after = edit_preview_after_text(raw_input)?;
    let current = edit_preview_input_content(raw_input)?;
    let replaced = current.replacen(before, after, 1);
    (replaced != current).then_some(replaced)
}

fn edit_preview_old_text_from_raw_input(
    raw_input: Option<&Value>,
    new_text: Option<&str>,
) -> Option<String> {
    let raw_input = raw_input?;
    let before = edit_preview_before_text(raw_input);
    let after = edit_preview_after_text(raw_input);
    if before.is_some()
        && after.is_some()
        && let Some(content) = edit_preview_input_content(raw_input)
    {
        return Some(content);
    }

    let before = before?;
    let after = after?;
    let new_text = new_text?;
    let replaced = new_text.replacen(after, before, 1);
    (replaced != new_text).then_some(replaced)
}

fn edit_preview_before_text(raw_input: &Value) -> Option<&str> {
    raw_input
        .get("before")
        .or_else(|| raw_input.get("old_string"))
        .or_else(|| raw_input.get("oldString"))
        .and_then(Value::as_str)
}

fn edit_preview_after_text(raw_input: &Value) -> Option<&str> {
    raw_input
        .get("after")
        .or_else(|| raw_input.get("new_string"))
        .or_else(|| raw_input.get("newString"))
        .and_then(Value::as_str)
}

fn edit_preview_input_content(raw_input: &Value) -> Option<String> {
    raw_input
        .get("content")
        .or_else(|| raw_input.get("oldText"))
        .or_else(|| raw_input.get("old_text"))
        .and_then(Value::as_str)
        .map(str::to_string)
}
