use super::{normalize_path_for_storage, normalize_tracked_path};
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
        .or_else(|| input.get("new_text"))
        .or_else(|| input.get("newText"))
        .and_then(|value| value.as_str())
}

pub(super) fn write_input_content_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("content")
        .or_else(|| input.get("new_text"))
        .or_else(|| input.get("newText"))
        .and_then(|value| value.as_str())
}

pub(super) fn raw_input_has_write_payload(raw_input: Option<&str>) -> bool {
    let Some(raw_input) = raw_input else {
        return false;
    };
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        return write_input_content_text(&value)
            .map(|text| !text.is_empty())
            .unwrap_or(false)
            || json_has_change_map(&value);
    }
    !extract_apply_patch_paths(raw_input).is_empty()
}

pub(super) fn edit_input_unified_diff_for_path<'a>(
    input: &'a serde_json::Value,
    normalized_path: &str,
    workspace_root: &std::path::Path,
) -> Option<&'a str> {
    let changes = input.get("changes")?.as_object()?;
    for (path, change) in changes {
        let display_path = change
            .get("move_path")
            .or_else(|| change.get("movePath"))
            .and_then(|value| value.as_str())
            .filter(|path| !path.trim().is_empty())
            .unwrap_or(path);
        if normalize_path_for_storage(display_path, workspace_root) != normalized_path
            && normalize_tracked_path(display_path) != normalized_path
        {
            continue;
        }
        if let Some(diff) = change
            .get("unified_diff")
            .or_else(|| change.get("unifiedDiff"))
            .and_then(|value| value.as_str())
        {
            return Some(diff);
        }
    }
    None
}

pub(super) fn reverse_apply_unified_diff(new_text: &str, unified_diff: &str) -> Option<String> {
    reverse_apply_parsed_unified_hunks(new_text, parse_unified_diff_hunks(unified_diff)?)
}

pub(crate) fn reverse_apply_diff_hunks(new_text: &str, hunks: &[DiffHunk]) -> Option<String> {
    let parsed = hunks
        .iter()
        .map(|hunk| {
            let mut old_side = Vec::new();
            let mut new_side = Vec::new();
            for line in &hunk.lines {
                match line.kind {
                    DiffLineKind::Context => {
                        old_side.push(line.content.clone());
                        new_side.push(line.content.clone());
                    }
                    DiffLineKind::Removed => old_side.push(line.content.clone()),
                    DiffLineKind::Added => new_side.push(line.content.clone()),
                }
            }
            Some(ParsedUnifiedHunk {
                new_start: parse_unified_new_start(&hunk.heading).unwrap_or(1),
                old_side,
                new_side,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    reverse_apply_parsed_unified_hunks(new_text, parsed)
}

fn reverse_apply_parsed_unified_hunks(
    new_text: &str,
    mut hunks: Vec<ParsedUnifiedHunk>,
) -> Option<String> {
    let mut lines = split_text_lines(new_text);
    let trailing_newline = new_text.ends_with('\n');
    hunks.sort_by_key(|hunk| hunk.new_start);
    for hunk in hunks.into_iter().rev() {
        let index = hunk
            .new_start
            .checked_sub(1)
            .filter(|index| *index <= lines.len())?;
        let replacement_index = if lines[index..].starts_with(&hunk.new_side) {
            index
        } else {
            find_unique_slice(&lines, &hunk.new_side)?
        };
        lines.splice(
            replacement_index..replacement_index + hunk.new_side.len(),
            hunk.old_side,
        );
    }

    Some(join_text_lines(&lines, trailing_newline))
}

struct ParsedUnifiedHunk {
    new_start: usize,
    old_side: Vec<String>,
    new_side: Vec<String>,
}

fn parse_unified_diff_hunks(unified_diff: &str) -> Option<Vec<ParsedUnifiedHunk>> {
    let mut hunks = Vec::<ParsedUnifiedHunk>::new();
    let mut current: Option<ParsedUnifiedHunk> = None;

    for line in unified_diff.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(ParsedUnifiedHunk {
                new_start: parse_unified_new_start(line)?,
                old_side: Vec::new(),
                new_side: Vec::new(),
            });
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            continue;
        };
        if line.starts_with("\\ No newline") {
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let (kind, content) = line.split_at(1);
        match kind {
            " " => {
                hunk.old_side.push(content.to_string());
                hunk.new_side.push(content.to_string());
            }
            "-" => hunk.old_side.push(content.to_string()),
            "+" => hunk.new_side.push(content.to_string()),
            _ => {}
        }
    }

    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    (!hunks.is_empty()).then_some(hunks)
}

fn parse_unified_new_start(header: &str) -> Option<usize> {
    let plus = header.find('+')?;
    let rest = &header[plus + 1..];
    let end = rest
        .find(|ch: char| ch == ',' || ch.is_whitespace() || ch == '@')
        .unwrap_or(rest.len());
    rest[..end].parse::<usize>().ok()
}

fn split_text_lines(text: &str) -> Vec<String> {
    let normalized = normalize_diff_text_for_session_change(text);
    let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
    if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(str::to_string).collect()
    }
}

fn join_text_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut text = lines.join("\n");
    if trailing_newline {
        text.push('\n');
    }
    text
}

fn find_unique_slice(lines: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > lines.len() {
        return None;
    }
    let mut found = None;
    for index in 0..=lines.len() - needle.len() {
        if &lines[index..index + needle.len()] == needle {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
    }
    found
}

pub(super) fn tool_event_hint_paths(raw_input: Option<&str>) -> Vec<String> {
    let Some(raw_input) = raw_input else {
        return Vec::new();
    };

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        let mut paths = Vec::new();
        collect_path_like_values(&value, &mut paths);
        collect_change_map_paths(&value, &mut paths);
        collect_command_write_hint_paths(&value, &mut paths);
        paths.retain(|path| is_usable_write_path(path));
        paths.sort();
        paths.dedup();
        return paths;
    }

    let mut paths = extract_apply_patch_paths(raw_input);
    paths.extend(extract_write_paths_from_command_text(raw_input));
    if paths.is_empty() && looks_like_standalone_path(raw_input) {
        paths.push(raw_input.to_string());
    }
    paths.retain(|path| is_usable_write_path(path));
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
                if key.contains("path") || key == "file" || key == "cwd" || key.ends_with("file")
                {
                    collect_path_field_value(value, paths);
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

fn collect_path_field_value(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::String(path) => paths.push(path.to_string()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_field_value(item, paths);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values() {
                collect_path_field_value(value, paths);
            }
        }
        _ => {}
    }
}

fn collect_change_map_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(changes) = object.get("changes").and_then(serde_json::Value::as_object) {
                for (path, change) in changes {
                    if looks_like_file_change_entry(change) {
                        paths.push(path.to_string());
                        if let Some(move_path) = change
                            .get("move_path")
                            .or_else(|| change.get("movePath"))
                            .and_then(serde_json::Value::as_str)
                            .filter(|path| !path.trim().is_empty())
                        {
                            paths.push(move_path.to_string());
                        }
                    }
                }
            }
            for value in object.values() {
                collect_change_map_paths(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_change_map_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn json_has_change_map(value: &serde_json::Value) -> bool {
    let mut paths = Vec::new();
    collect_change_map_paths(value, &mut paths);
    paths.iter().any(|path| is_usable_write_path(path))
}

fn looks_like_file_change_entry(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| matches!(kind, "add" | "create" | "update" | "modify" | "delete"))
        || object.contains_key("unified_diff")
        || object.contains_key("unifiedDiff")
        || object.contains_key("content")
        || object.contains_key("move_path")
        || object.contains_key("movePath")
}

fn extract_apply_patch_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        for prefix in [
            "*** Add File:",
            "*** Update File:",
            "*** Delete File:",
            "*** Move to:",
        ] {
            if let Some(path) = line.strip_prefix(prefix) {
                let path = path.trim();
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths
}

fn collect_command_write_hint_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                let key = key.trim_matches('"');
                if matches!(
                    key,
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
    collect_python_pathlib_write_paths(&command, &mut paths);
    collect_python_open_write_paths(&command, &mut paths);
    collect_common_mutation_command_paths(&command, &mut paths);
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

fn collect_python_pathlib_write_paths(command: &str, paths: &mut Vec<String>) {
    if !command.contains("write_text(") && !command.contains("write_bytes(") {
        return;
    }

    let mut offset = 0;
    while let Some(index) = find_next_python_path_call(command, offset) {
        if let Some((path, end)) = parse_python_path_call_at(command, index) {
            let after = command[end..].trim_start();
            if after.starts_with(".write_text(") || after.starts_with(".write_bytes(") {
                paths.push(path);
            }
            offset = end;
        } else {
            offset = index + 1;
        }
    }

    for (name, path) in python_pathlib_assignments(command) {
        if contains_python_method_call(command, &name, "write_text")
            || contains_python_method_call(command, &name, "write_bytes")
        {
            paths.push(path);
        }
    }
}

fn collect_python_open_write_paths(command: &str, paths: &mut Vec<String>) {
    let mut offset = 0;
    while let Some(index) = find_next_python_open_call(command, offset) {
        if let Some((path, end)) = parse_python_open_write_call_at(command, index) {
            paths.push(path);
            offset = end;
        } else {
            offset = index + 1;
        }
    }
}

fn find_next_python_open_call(command: &str, start: usize) -> Option<usize> {
    let open = command[start..].find("open(").map(|index| start + index);
    let io_open = command[start..].find("io.open(").map(|index| start + index);
    [open, io_open]
        .into_iter()
        .flatten()
        .filter(|index| {
            let rest = &command[*index..];
            if rest.starts_with("io.open(") {
                return true;
            }
            command[..*index]
                .chars()
                .next_back()
                .map_or(true, |ch| !is_python_identifier_char(ch) && ch != '.')
        })
        .min()
}

fn parse_python_open_write_call_at(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start..];
    let arg_start = if rest.starts_with("io.open(") {
        start + "io.open(".len()
    } else if rest.starts_with("open(") {
        start + "open(".len()
    } else {
        return None;
    };
    let (path, path_end) = parse_python_string_literal_at(text, arg_start)?;
    let comma = skip_ascii_whitespace(text, path_end);
    if !text[comma..].starts_with(',') {
        return None;
    }
    let (mode, mode_end) = parse_python_string_literal_at(text, comma + 1)?;
    if python_file_mode_can_write(&mode) {
        Some((path, mode_end))
    } else {
        None
    }
}

fn python_file_mode_can_write(mode: &str) -> bool {
    mode.chars().any(|ch| matches!(ch, 'w' | 'a' | 'x' | '+'))
}

fn collect_common_mutation_command_paths(command: &str, paths: &mut Vec<String>) {
    for segment in command.split([';', '\n', '|']) {
        let tokens = tokenize_command_args(segment);
        let Some(command) = tokens.first().map(|token| command_basename(token)) else {
            continue;
        };
        let args = tokens
            .iter()
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>();
        match command.as_str() {
            "mkdir" | "touch" | "rm" | "rmdir" | "del" | "erase" | "remove-item" | "mv"
            | "move" | "move-item" | "cp" | "copy" | "copy-item" => {
                paths.extend(command_path_args(&args));
            }
            "git" => {
                if let Some(subcommand) = args
                    .iter()
                    .find(|arg| !arg.starts_with('-'))
                    .map(|arg| arg.to_ascii_lowercase())
                    && matches!(
                        subcommand.as_str(),
                        "add" | "checkout" | "restore" | "reset" | "apply" | "commit"
                    )
                {
                    paths.extend(command_path_args(&args));
                }
            }
            _ => {}
        }
    }
}

fn command_basename(command: &str) -> String {
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

fn command_path_args(args: &[&str]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        let arg = arg.trim();
        if arg.is_empty() {
            continue;
        }
        if arg == "--" {
            continue;
        }
        if arg.starts_with('-') {
            if powershell_param_takes_value(arg) {
                skip_next = true;
            }
            continue;
        }
        if looks_like_standalone_path(arg) {
            paths.push(arg.to_string());
        }
    }
    paths
}

fn python_pathlib_assignments(command: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();
    for line in command.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some(eq_index) = line.find('=') else {
            continue;
        };
        let name = line[..eq_index].trim();
        if !is_python_identifier(name) {
            continue;
        }
        let right = line[eq_index + 1..].trim_start();
        if let Some((path, _)) = parse_python_path_call_at(right, 0) {
            assignments.push((name.to_string(), path));
        }
    }
    assignments
}

fn contains_python_method_call(command: &str, name: &str, method: &str) -> bool {
    let pattern = format!("{name}.{method}(");
    let mut offset = 0;
    while let Some(relative) = command[offset..].find(&pattern) {
        let index = offset + relative;
        let before = command[..index].chars().next_back();
        if before.map_or(true, |ch| !is_python_identifier_char(ch)) {
            return true;
        }
        offset = index + pattern.len();
    }
    false
}

fn find_next_python_path_call(command: &str, start: usize) -> Option<usize> {
    let path = command[start..].find("Path(").map(|index| start + index);
    let pathlib = command[start..]
        .find("pathlib.Path(")
        .map(|index| start + index);
    match (path, pathlib) {
        (Some(path), Some(pathlib)) => Some(path.min(pathlib)),
        (Some(path), None) => Some(path),
        (None, Some(pathlib)) => Some(pathlib),
        (None, None) => None,
    }
}

fn parse_python_path_call_at(text: &str, start: usize) -> Option<(String, usize)> {
    let rest = &text[start..];
    let arg_start = if rest.starts_with("pathlib.Path(") {
        start + "pathlib.Path(".len()
    } else if rest.starts_with("Path(") {
        start + "Path(".len()
    } else {
        return None;
    };
    let (path, value_end) = parse_python_string_literal_at(text, arg_start)?;
    let close_paren = skip_ascii_whitespace(text, value_end);
    if text[close_paren..].starts_with(')') {
        Some((path, close_paren + 1))
    } else {
        None
    }
}

fn parse_python_string_literal_at(text: &str, start: usize) -> Option<(String, usize)> {
    let mut index = skip_ascii_whitespace(text, start);
    while let Some(ch) = text[index..].chars().next() {
        if matches!(ch, 'r' | 'R' | 'u' | 'U' | 'b' | 'B' | 'f' | 'F') {
            index += ch.len_utf8();
            continue;
        }
        break;
    }
    let quote = text[index..].chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body_start = index + quote.len_utf8();
    let mut escaped = false;
    for (relative, ch) in text[body_start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some((
                text[body_start..body_start + relative].to_string(),
                body_start + relative + quote.len_utf8(),
            ));
        }
    }
    None
}

fn skip_ascii_whitespace(text: &str, start: usize) -> usize {
    let mut index = start;
    while let Some(ch) = text[index..].chars().next() {
        if !ch.is_ascii_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn is_python_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(is_python_identifier_char)
}

fn is_python_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
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
    } else if let (Some(existing), true) = (
        existing_tool_hunks.as_ref(),
        !tracker_hunks.is_empty() && !looks_like_whole_file_addition_hunks(tracker_hunks),
    ) && !looks_like_whole_file_addition_hunks(existing)
        && changed_line_count(tracker_hunks) <= changed_line_count(existing)
    {
        tracker_hunks.to_vec()
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

fn changed_line_count(hunks: &[DiffHunk]) -> usize {
    hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
        .count()
}

pub(super) fn looks_like_fragment_to_full_file_text(old_text: &str, new_text: &str) -> bool {
    let old_lines = old_text.lines().count();
    let new_lines = new_text.lines().count();
    old_lines > 0 && new_lines >= 100 && old_lines * 4 < new_lines
}

pub(super) fn looks_like_whole_file_addition_hunks(hunks: &[DiffHunk]) -> bool {
    let mut added = 0;
    let mut removed = 0;
    for hunk in hunks {
        if let (Some(old_count), Some(new_count)) = (
            parse_unified_range_count(&hunk.heading, '-'),
            parse_unified_range_count(&hunk.heading, '+'),
        ) && new_count >= 100
            && (old_count == 0 || new_count > old_count.saturating_mul(4))
        {
            return true;
        }
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Added => added += 1,
                DiffLineKind::Removed => removed += 1,
                DiffLineKind::Context => {}
            }
        }
    }
    added >= 100 && (removed == 0 || added > removed * 4)
}

fn parse_unified_range_count(header: &str, marker: char) -> Option<usize> {
    let marker_index = header.find(marker)?;
    let rest = &header[marker_index + marker.len_utf8()..];
    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '@')
        .unwrap_or(rest.len());
    let range = &rest[..end];
    if range.is_empty() {
        return None;
    }
    let count = range.split_once(',').map(|(_, count)| count).unwrap_or("1");
    count.parse::<usize>().ok()
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
            "edit"
                | "write"
                | "patch"
                | "multiedit"
                | "multi_edit"
                | "multi-edit"
                | "applypatch"
                | "apply_patch"
                | "apply-patch"
                | "fswrite"
                | "fs_write"
                | "fs-write"
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
