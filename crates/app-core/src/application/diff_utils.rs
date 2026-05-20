use acp_core::diff_to_hunks;
use workspace_model::{DiffHunk, DiffLineKind, DiffQuality, FileChangeType, SessionFileChange};

#[derive(Clone)]
pub(super) struct ExactEditText {
    pub(super) old_text: String,
    pub(super) new_text: String,
    pub(super) hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CanonicalTextDiff {
    pub(super) old_text: Option<String>,
    pub(super) new_text: Option<String>,
    pub(super) hunks: Vec<DiffHunk>,
    pub(super) added_lines: usize,
    pub(super) removed_lines: usize,
    pub(super) quality: DiffQuality,
}

pub(super) fn normalize_diff_text_for_session_change(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub(super) fn canonical_text_diff(
    change_type: &FileChangeType,
    old_text: Option<&str>,
    new_text: Option<&str>,
    quality_hint: Option<DiffQuality>,
) -> CanonicalTextDiff {
    let normalized_old = old_text.map(normalize_diff_text_for_session_change);
    let normalized_new = new_text.map(normalize_diff_text_for_session_change);

    if let Some(quality) = quality_hint.filter(|quality| *quality != DiffQuality::Exact) {
        return CanonicalTextDiff {
            old_text: normalized_old,
            new_text: normalized_new.filter(|text| !text.is_empty()),
            hunks: Vec::new(),
            added_lines: 0,
            removed_lines: 0,
            quality,
        };
    }

    if *change_type == FileChangeType::Deleted {
        let Some(old_text) = normalized_old else {
            return canonical_unavailable_diff(DiffQuality::MissingBaseline, None, None);
        };
        let hunks = diff_to_hunks(Some(&old_text), "");
        let (added_lines, removed_lines) = count_changed_lines(&hunks);
        return CanonicalTextDiff {
            old_text: Some(old_text),
            new_text: None,
            hunks,
            added_lines,
            removed_lines,
            quality: DiffQuality::Exact,
        };
    }

    let Some(new_text) = normalized_new else {
        return canonical_unavailable_diff(DiffQuality::MissingBaseline, normalized_old, None);
    };

    match normalized_old {
        Some(old_text) => {
            if looks_like_fragment_to_full_file_text(&old_text, &new_text) {
                return canonical_unavailable_diff(
                    DiffQuality::FragmentRejected,
                    Some(old_text),
                    Some(new_text),
                );
            }
            let hunks = diff_to_hunks(Some(&old_text), &new_text);
            let (added_lines, removed_lines) = count_changed_lines(&hunks);
            CanonicalTextDiff {
                old_text: Some(old_text),
                new_text: Some(new_text),
                hunks,
                added_lines,
                removed_lines,
                quality: DiffQuality::Exact,
            }
        }
        None if *change_type == FileChangeType::Created => {
            let hunks = diff_to_hunks(None, &new_text);
            let (added_lines, removed_lines) = count_changed_lines(&hunks);
            CanonicalTextDiff {
                old_text: None,
                new_text: Some(new_text),
                hunks,
                added_lines,
                removed_lines,
                quality: DiffQuality::Exact,
            }
        }
        None => canonical_unavailable_diff(DiffQuality::MissingBaseline, None, Some(new_text)),
    }
}

pub(super) fn sanitize_session_file_changes(changes: &mut Vec<SessionFileChange>) -> bool {
    let original_len = changes.len();
    let mut changed = false;

    for change in changes.iter_mut() {
        let previous_added = change.added_lines;
        let previous_removed = change.removed_lines;
        let normalized_old = change
            .old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let normalized_new = normalize_diff_text_for_session_change(&change.new_text);
        if change.old_text != normalized_old || change.new_text != normalized_new {
            change.old_text = normalized_old;
            change.new_text = normalized_new;
            changed = true;
        }

        let canonical = canonical_text_diff(
            &change.change_type,
            change.old_text.as_deref(),
            Some(&change.new_text),
            None,
        );
        change.added_lines = canonical.added_lines;
        change.removed_lines = canonical.removed_lines;
        if change.added_lines != previous_added || change.removed_lines != previous_removed {
            changed = true;
        }
    }

    changes.retain(|change| change.added_lines > 0 || change.removed_lines > 0);
    changed || changes.len() != original_len
}

pub(super) fn is_trustworthy_review_change_text(
    change_type: &FileChangeType,
    old_text: Option<&str>,
    new_text: &str,
) -> bool {
    matches!(
        canonical_text_diff(change_type, old_text, Some(new_text), None).quality,
        DiffQuality::Exact
    )
}

pub(super) fn tool_diff_hunks(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    diff_to_hunks(previous_session_new_text.or(tool_old_text), tool_new_text)
}

pub(super) fn edit_input_before_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("before")
        .or_else(|| input.get("old_string"))
        .or_else(|| input.get("oldString"))
        .and_then(|value| value.as_str())
}

pub(super) fn edit_input_after_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("after")
        .or_else(|| input.get("new_string"))
        .or_else(|| input.get("newString"))
        .and_then(|value| value.as_str())
}

pub(super) fn tool_event_hint_paths(raw_input: Option<&str>) -> Vec<String> {
    let Some(raw_input) = raw_input else {
        return Vec::new();
    };

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        let mut paths = Vec::new();
        collect_path_like_values(&value, &mut paths);
        collect_command_write_hint_paths(&value, &mut paths);
        paths.sort();
        paths.dedup();
        return paths;
    }

    let mut paths = extract_write_paths_from_command_text(raw_input);
    if paths.is_empty() && looks_like_standalone_path(raw_input) {
        paths.push(raw_input.to_string());
    }
    paths.sort();
    paths.dedup();
    paths
}

pub(super) fn tool_command_write_hint_paths(raw_input: Option<&str>) -> Vec<String> {
    let Some(raw_input) = raw_input else {
        return Vec::new();
    };

    let mut paths = if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        let mut paths = Vec::new();
        collect_command_write_hint_paths(&value, &mut paths);
        paths
    } else {
        extract_write_paths_from_command_text(raw_input)
    };
    paths.sort();
    paths.dedup();
    paths
}

fn collect_path_like_values(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if (key.contains("path") || key == "file" || key == "cwd" || key.ends_with("file"))
                    && let Some(path) = value.as_str()
                {
                    paths.push(path.to_string());
                    continue;
                }
                collect_path_like_values(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_like_values(item, paths);
            }
        }
        _ => {}
    }
}

fn collect_command_write_hint_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "command" | "cmd" | "shell_command" | "script" | "source"
                ) {
                    collect_command_value_write_paths(value, paths);
                }
                collect_command_write_hint_paths(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_command_write_hint_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn collect_command_value_write_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::String(command) => {
            paths.extend(extract_write_paths_from_command_text(command));
        }
        serde_json::Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            for part in &parts {
                paths.extend(extract_write_paths_from_command_text(part));
            }
            if parts.len() > 1 {
                paths.extend(extract_write_paths_from_command_text(&parts.join(" ")));
            }
        }
        _ => {}
    }
}

fn extract_write_paths_from_command_text(command: &str) -> Vec<String> {
    let command = strip_powershell_here_strings(command);
    let mut paths = Vec::new();
    collect_powershell_write_cmdlet_paths(&command, &mut paths);
    collect_shell_redirection_paths(&command, &mut paths);
    paths.retain(|path| is_usable_write_path(path));
    paths.sort();
    paths.dedup();
    paths
}

fn strip_powershell_here_strings(command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut index = 0;
    while index < command.len() {
        let rest = &command[index..];
        let Some((quote, marker_len)) = rest
            .strip_prefix("@\"")
            .map(|_| ('"', 2))
            .or_else(|| rest.strip_prefix("@'").map(|_| ('\'', 2)))
        else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            output.push(ch);
            index += ch.len_utf8();
            continue;
        };

        index += marker_len;
        let end_marker_lf = format!("\n{quote}@");
        let end_marker_crlf = format!("\r\n{quote}@");
        let remainder = &command[index..];
        let end_lf = remainder.find(&end_marker_lf);
        let end_crlf = remainder.find(&end_marker_crlf);
        let end = match (end_lf, end_crlf) {
            (Some(lf), Some(crlf)) => Some(lf.min(crlf)),
            (Some(lf), None) => Some(lf),
            (None, Some(crlf)) => Some(crlf),
            (None, None) => None,
        };
        if let Some(end) = end {
            index += end;
            let tail = &command[index..];
            if tail.starts_with(&end_marker_crlf) {
                index += end_marker_crlf.len();
            } else {
                index += end_marker_lf.len();
            }
            output.push(' ');
        } else {
            break;
        }
    }
    output
}

fn collect_powershell_write_cmdlet_paths(command: &str, paths: &mut Vec<String>) {
    for segment in command.split([';', '\n']) {
        let lower = segment.to_ascii_lowercase();
        if contains_command_token(&lower, "set-content")
            || contains_command_token(&lower, "add-content")
        {
            paths.extend(extract_param_values(
                segment,
                &["-literalpath", "-filepath", "-path"],
            ));
            paths.extend(extract_positional_write_path_values(
                segment,
                &["set-content", "add-content"],
            ));
        } else if contains_command_token(&lower, "out-file") {
            paths.extend(extract_param_values(segment, &["-filepath", "-path"]));
            paths.extend(extract_positional_write_path_values(segment, &["out-file"]));
        } else if contains_command_token(&lower, "new-item")
            && has_param_value(&lower, "-itemtype", "file")
        {
            paths.extend(extract_param_values(segment, &["-literalpath", "-path"]));
            paths.extend(extract_positional_write_path_values(segment, &["new-item"]));
        }
    }
}

fn contains_command_token(text: &str, token: &str) -> bool {
    find_command_token(text, token).is_some()
}

fn find_command_token(text: &str, token: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(token) {
        let index = offset + relative;
        let before = text[..index].chars().next_back();
        let after = text[index + token.len()..].chars().next();
        let before_ok = before.map_or(true, |ch| !is_command_word_char(ch));
        let after_ok = after.map_or(true, |ch| !is_command_word_char(ch));
        if before_ok && after_ok {
            return Some(index);
        }
        offset = index + token.len();
    }
    None
}

fn is_command_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')
}

fn extract_positional_write_path_values(segment: &str, commands: &[&str]) -> Vec<String> {
    let lower = segment.to_ascii_lowercase();
    let mut values = Vec::new();
    for command in commands {
        let Some(index) = find_command_token(&lower, command) else {
            continue;
        };
        let args = &segment[index + command.len()..];
        let mut skip_next_value = false;
        for token in tokenize_command_args(args) {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if skip_next_value {
                skip_next_value = false;
                continue;
            }
            if token.starts_with('-') {
                if powershell_param_takes_value(token) {
                    skip_next_value = true;
                }
                continue;
            }
            if looks_like_standalone_path(token) {
                values.push(token.to_string());
            }
            break;
        }
    }
    values
}

fn powershell_param_takes_value(param: &str) -> bool {
    matches!(
        param.to_ascii_lowercase().as_str(),
        "-path"
            | "-literalpath"
            | "-filepath"
            | "-value"
            | "-encoding"
            | "-itemtype"
            | "-name"
            | "-destination"
            | "-destinationpath"
    )
}

fn tokenize_command_args(args: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = args.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '`' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '|' | ';' | ')') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if matches!(ch, '|' | ';') {
                break;
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn has_param_value(segment_lower: &str, param: &str, expected: &str) -> bool {
    extract_param_values(segment_lower, &[param])
        .iter()
        .any(|value| value.eq_ignore_ascii_case(expected))
}

fn extract_param_values(segment: &str, params: &[&str]) -> Vec<String> {
    let lower = segment.to_ascii_lowercase();
    let mut values = Vec::new();
    for param in params {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(param) {
            let index = offset + relative;
            let before = lower[..index].chars().next_back();
            let after = lower[index + param.len()..].chars().next();
            let before_ok = before.map_or(true, |ch| ch.is_whitespace() || ch == '|');
            let after_ok = after.map_or(true, |ch| ch.is_whitespace() || ch == ':');
            if before_ok
                && after_ok
                && let Some(value) = parse_command_value_at(segment, index + param.len())
            {
                values.push(value);
            }
            offset = index + param.len();
        }
    }
    values
}

fn parse_command_value_at(text: &str, start: usize) -> Option<String> {
    let rest = &text[start..];
    let mut offset = 0;
    for (idx, ch) in rest.char_indices() {
        if ch.is_whitespace() || ch == ':' {
            offset = idx + ch.len_utf8();
            continue;
        }
        offset = idx;
        break;
    }

    let value = &rest[offset..];
    let first = value.chars().next()?;
    if first == '"' || first == '\'' {
        let quote = first;
        let body = &value[first.len_utf8()..];
        for (idx, ch) in body.char_indices() {
            if ch == quote {
                return Some(body[..idx].to_string());
            }
        }
        return Some(body.to_string());
    }

    let end = value
        .char_indices()
        .find_map(|(idx, ch)| {
            if ch.is_whitespace() || matches!(ch, ';' | '|' | ')') {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(value.len());
    Some(value[..end].to_string())
}

fn collect_shell_redirection_paths(command: &str, paths: &mut Vec<String>) {
    let mut previous = '\0';
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch != '>' || previous.is_ascii_digit() {
            previous = ch;
            continue;
        }
        if matches!(chars.peek(), Some((_, '>'))) {
            chars.next();
        }
        if let Some(value) = parse_command_value_at(command, index + ch.len_utf8())
            && looks_like_standalone_path(&value)
        {
            paths.push(value);
        }
        previous = ch;
    }
}

fn is_usable_write_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty() || path.contains('\n') || path.contains('\r') {
        return false;
    }
    if path.starts_with('$') || path.starts_with('(') || path.starts_with('{') {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    !matches!(lower.as_str(), "$null" | "null" | "nul" | "/dev/null")
}

fn looks_like_standalone_path(path: &str) -> bool {
    let path = path.trim();
    if !is_usable_write_path(path) || path.len() > 512 {
        return false;
    }
    path.contains('/')
        || path.contains('\\')
        || path.starts_with('.')
        || path
            .rsplit_once('.')
            .map(|(_, extension)| {
                !extension.is_empty()
                    && extension
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
            .unwrap_or(false)
}

pub(super) fn tool_diff_hunks_for_tracker_change(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    // Filesystem tracking captures the real on-disk baseline when a tool starts.
    // Prefer that baseline for the ToolCard diff. Using the cumulative session
    // new_text here makes the first tracker-confirmed edit diff against itself,
    // which produces no +/- stats for goose ACP edits.
    if previous_session_new_text.is_none() && tool_old_text.is_none() {
        return Vec::new();
    }
    tool_diff_hunks(None, tool_old_text, tool_new_text).or_else_non_empty(|| {
        tool_diff_hunks(previous_session_new_text, tool_old_text, tool_new_text)
    })
}

pub(super) fn tool_hunks_for_tracker_update(
    skipped_diff: bool,
    exact_edit_hunks: Option<Vec<DiffHunk>>,
    existing_tool_hunks: Option<Vec<DiffHunk>>,
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
    tracker_hunks: &[DiffHunk],
) -> Vec<DiffHunk> {
    if skipped_diff {
        Vec::new()
    } else if let Some(hunks) = exact_edit_hunks {
        hunks
    } else if let Some(hunks) = existing_tool_hunks
        && !looks_like_whole_file_addition_hunks(&hunks)
    {
        hunks
    } else if previous_session_new_text.is_none() && !tracker_hunks.is_empty() {
        tracker_hunks.to_vec()
    } else {
        tool_diff_hunks_for_tracker_change(previous_session_new_text, tool_old_text, tool_new_text)
    }
}

pub(super) fn looks_like_fragment_to_full_file_text(old_text: &str, new_text: &str) -> bool {
    let old_lines = old_text.lines().count();
    let new_lines = new_text.lines().count();
    old_lines > 0 && new_lines >= 100 && old_lines * 4 < new_lines
}

pub(super) fn looks_like_whole_file_addition_hunks(hunks: &[DiffHunk]) -> bool {
    let mut added = 0;
    let mut removed = 0;
    for line in hunks.iter().flat_map(|hunk| &hunk.lines) {
        match line.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    added >= 100 && (removed == 0 || added > removed * 4)
}

pub(super) fn tool_diff_hunks_for_detected_write(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    tool_diff_hunks(previous_session_new_text, tool_old_text, tool_new_text)
        .or_else_non_empty(|| tool_diff_hunks(None, tool_old_text, tool_new_text))
}

pub(super) fn expand_tool_diff_fragment_from_disk(
    abs_path: &std::path::Path,
    old_text: Option<&str>,
    new_text: &str,
) -> Option<(String, String)> {
    let old_fragment = old_text
        .map(normalize_diff_text_for_session_change)
        .filter(|text| !text.is_empty())?;
    let new_fragment = normalize_diff_text_for_session_change(new_text);
    if new_fragment.is_empty() {
        return None;
    }

    let target_text = std::fs::read_to_string(abs_path)
        .ok()
        .map(|text| normalize_diff_text_for_session_change(&text))?;
    if target_text == new_fragment || !target_text.contains(&new_fragment) {
        return None;
    }

    let base_text = target_text.replacen(&new_fragment, &old_fragment, 1);
    (base_text != target_text).then_some((base_text, target_text))
}

pub(super) fn is_file_write_tool_identity(kind: &str, name: &str) -> bool {
    kind_and_name_tokens(kind, name).any(|token| {
        matches!(
            token.as_str(),
            "edit" | "write" | "patch" | "applypatch" | "apply_patch" | "apply-patch"
        )
    })
}

fn kind_and_name_tokens<'a>(kind: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    kind.split(|ch: char| !ch.is_ascii_alphanumeric())
        .chain(name.split(|ch: char| !ch.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
}

fn canonical_unavailable_diff(
    quality: DiffQuality,
    old_text: Option<String>,
    new_text: Option<String>,
) -> CanonicalTextDiff {
    CanonicalTextDiff {
        old_text,
        new_text,
        hunks: Vec::new(),
        added_lines: 0,
        removed_lines: 0,
        quality,
    }
}

fn count_changed_lines(hunks: &[DiffHunk]) -> (usize, usize) {
    let added = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    let removed = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count();
    (added, removed)
}

trait NonEmptyFallback {
    fn or_else_non_empty<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self;
}

impl<T> NonEmptyFallback for Vec<T> {
    fn or_else_non_empty<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self,
    {
        if self.is_empty() { fallback() } else { self }
    }
}
