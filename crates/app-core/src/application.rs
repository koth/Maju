use crate::bootstrap::build_initial_ui;
use crate::file_tracker::FileChangeTracker;
use crate::paths::AppPaths;
use crate::reducer::apply_event;
use acp_core::{ClientEvent, PromptTask, SessionConfig, SessionHandle, diff_to_hunks};
use git_service::GitService;
use session_store::SessionStore;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use workspace_model::{
    AgentCliId, ChangeSection, ChangeSetFilesResponse, ChangeSetSource, ChangeSetStatus,
    ChangeSetSummary, ChatMessage, ChatMessageDelta, DiffHunk, DiffLineKind, DiffQuality,
    FileChangeRecord, FileChangeSummary, FileChangeType, GetChangeSetFileDiffRequest,
    ListChangeSetFilesRequest, ListChangeSetsRequest, MessageRole, SessionConfigSource,
    SessionFileChange, SessionListItem, SessionStatus, TimelineItem, ToolDiffPreview,
    ToolInvocation, ToolLogEntry, ToolStatus, TurnFileChanges, UiSnapshotPatch, UserPromptContent,
};

const AGENT_DEFAULT_MODEL_LABEL: &str = "Agent default";
const RESTORED_INCOMPLETE_TOOL_REASON: &str = "上次会话结束前未完成";
const SNAPSHOT_TOOL_DETAIL_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_RAW_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_OUTPUT_CHARS: usize = 8 * 1024;
const SNAPSHOT_TOOL_LOG_CHARS: usize = 1024;
const SNAPSHOT_TOOL_LOG_ENTRIES: usize = 6;

struct ChangeSectionEntry {
    section: ChangeSection,
    change_set_id: &'static str,
    label: &'static str,
}

impl ChangeSectionEntry {
    fn staged() -> Self {
        Self {
            section: ChangeSection::Staged,
            change_set_id: "git-worktree:staged",
            label: "Git 已暂存",
        }
    }

    fn unstaged() -> Self {
        Self {
            section: ChangeSection::Unstaged,
            change_set_id: "git-worktree:unstaged",
            label: "Git 未暂存",
        }
    }

    fn untracked() -> Self {
        Self {
            section: ChangeSection::Untracked,
            change_set_id: "git-worktree:untracked",
            label: "Git 未跟踪",
        }
    }
}

fn make_log_id() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ts}")
}

struct InFlightPrompt {
    task: PromptTask,
}

#[derive(Debug, Default)]
struct InlineThinkFilter {
    in_think: bool,
    pending: String,
}

impl InlineThinkFilter {
    fn reset(&mut self) {
        self.in_think = false;
        self.pending.clear();
    }

    fn filter_chunk(&mut self, chunk: &str) -> Option<String> {
        if chunk.is_empty() && self.pending.is_empty() {
            return None;
        }

        let mut text = String::new();
        if !self.pending.is_empty() {
            text.push_str(&self.pending);
            self.pending.clear();
        }
        text.push_str(chunk);

        let mut visible = String::new();
        let mut cursor = 0;

        while cursor < text.len() {
            if self.in_think {
                if let Some(close_at) = find_ascii_case_insensitive(&text[cursor..], "</think>") {
                    cursor += close_at + "</think>".len();
                    self.in_think = false;
                } else {
                    let suffix_len = trailing_tag_prefix_len(&text[cursor..], "</think>");
                    if suffix_len > 0 {
                        self.pending = text[text.len() - suffix_len..].to_string();
                    }
                    break;
                }
            } else if let Some(open_at) = find_ascii_case_insensitive(&text[cursor..], "<think>") {
                let open_start = cursor + open_at;
                visible.push_str(&text[cursor..open_start]);
                cursor = open_start + "<think>".len();
                self.in_think = true;
            } else {
                let suffix_len = trailing_tag_prefix_len(&text[cursor..], "<think>");
                let emit_end = text.len() - suffix_len;
                visible.push_str(&text[cursor..emit_end]);
                if suffix_len > 0 {
                    self.pending = text[emit_end..].to_string();
                }
                break;
            }
        }

        (!visible.is_empty()).then_some(visible)
    }

    fn flush(&mut self) -> Option<String> {
        let visible = if self.in_think {
            None
        } else if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        };
        self.reset();
        visible
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn trailing_tag_prefix_len(text: &str, tag: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let tag = tag.to_ascii_lowercase();
    (1..tag.len())
        .rev()
        .find(|len| lower.ends_with(&tag[..*len]))
        .unwrap_or(0)
}

#[derive(Clone)]
struct ExactEditText {
    old_text: String,
    new_text: String,
    hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalTextDiff {
    old_text: Option<String>,
    new_text: Option<String>,
    hunks: Vec<DiffHunk>,
    added_lines: usize,
    removed_lines: usize,
    quality: DiffQuality,
}

pub struct Application {
    pub ui: workspace_model::UiSnapshot,
    session: SessionHandle,
    store: SessionStore,
    app_paths: AppPaths,
    pub agent_command: String,
    acp_port: u16,
    in_flight_prompt: Option<InFlightPrompt>,
    /// Tracks the current timeline sequence counter for SQLite persistence
    seq_counter: i64,
    /// Whether we're waiting to generate a title after the first turn
    needs_title: bool,
    /// Whether the agent has pushed a title via SessionTitleUpdated
    agent_title_received: bool,
    /// Prompt-derived first title; agent title syncs echoing this value are stale.
    provisional_prompt_title: Option<String>,
    /// When true, discard replay events from session/load until user sends first prompt
    skip_replay: bool,
    pending_model_restore: Option<String>,
    file_tracker: FileChangeTracker,
    dirty_tool_call_ids: HashSet<String>,
    review_changes_started: bool,
    current_turn_user_message_id: Option<uuid::Uuid>,
    inline_think_filter: InlineThinkFilter,
}

#[derive(Debug, Default)]
pub struct UiPatchCursor {
    revision: u64,
    workspace_id: Option<uuid::Uuid>,
    session_id: Option<uuid::Uuid>,
    timeline_len: usize,
    message_bodies: HashMap<uuid::Uuid, String>,
    known_tool_ids: HashSet<uuid::Uuid>,
}

pub enum UiSnapshotUpdate {
    Full(workspace_model::UiSnapshot),
    Patch(UiSnapshotPatch),
}

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

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn file_summary_from_record(record: &FileChangeRecord) -> FileChangeSummary {
    FileChangeSummary {
        change_set_id: record.change_set_id.clone(),
        path: normalize_tracked_path(&record.path),
        change_type: record.change_type.clone(),
        added_lines: record.added_lines,
        removed_lines: record.removed_lines,
        quality: record.quality.clone(),
        updated_at: record.updated_at.clone(),
    }
}

fn git_section_from_change_set_id(change_set_id: &str) -> Option<ChangeSection> {
    match change_set_id {
        "git-worktree:staged" => Some(ChangeSection::Staged),
        "git-worktree:unstaged" => Some(ChangeSection::Unstaged),
        "git-worktree:untracked" => Some(ChangeSection::Untracked),
        _ => None,
    }
}

fn turn_finished_notice(stop_reason: &str, agent_cli: Option<&str>) -> Option<String> {
    let agent = agent_cli
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .unwrap_or("智能体");

    match stop_reason {
        "end_turn" => None,
        "cancelled" => Some("本轮已取消。".into()),
        "refusal" => Some(format!(
            "本轮异常结束：{agent} 返回 `refusal`，没有完成正常收尾。常见原因是上游请求失败、被拒绝或限流（例如 429）；请查看对应智能体日志获取更具体的错误。"
        )),
        "max_tokens" => Some(format!(
            "本轮异常结束：{agent} 达到最大上下文或输出 token 限制，未完成正常收尾。"
        )),
        "max_turn_requests" => Some(format!(
            "本轮异常结束：{agent} 达到本轮最大请求次数限制，未完成正常收尾。"
        )),
        other => Some(format!("本轮异常结束：{agent} 返回 `{other}`。")),
    }
}

fn normalize_diff_text_for_session_change(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn canonical_text_diff(
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

fn sanitize_session_file_changes(changes: &mut Vec<SessionFileChange>) -> bool {
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

fn is_trustworthy_review_change_text(
    change_type: &FileChangeType,
    old_text: Option<&str>,
    new_text: &str,
) -> bool {
    matches!(
        canonical_text_diff(change_type, old_text, Some(new_text), None).quality,
        DiffQuality::Exact
    )
}

fn tool_diff_hunks(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    diff_to_hunks(previous_session_new_text.or(tool_old_text), tool_new_text)
}

fn edit_input_before_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("before")
        .or_else(|| input.get("old_string"))
        .or_else(|| input.get("oldString"))
        .and_then(|value| value.as_str())
}

fn edit_input_after_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("after")
        .or_else(|| input.get("new_string"))
        .or_else(|| input.get("newString"))
        .and_then(|value| value.as_str())
}

fn tool_event_hint_paths(raw_input: Option<&str>) -> Vec<String> {
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
            if before_ok && after_ok {
                if let Some(value) = parse_command_value_at(segment, index + param.len()) {
                    values.push(value);
                }
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

fn interrupt_incomplete_tools(tools: &mut [ToolInvocation]) -> Vec<String> {
    let mut updated_ids = Vec::new();

    for tool in tools
        .iter_mut()
        .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        tool.status = ToolStatus::Interrupted;
        if tool.summary.trim().is_empty()
            || tool.summary == "等待活动"
            || tool.summary.starts_with("等待权限")
        {
            tool.summary = RESTORED_INCOMPLETE_TOOL_REASON.into();
        }
        if tool.kind == "permission" && tool.permission_decision.is_none() {
            tool.permission_decision = Some("已中断".into());
        }
        if tool.error.is_none() {
            tool.error = Some(RESTORED_INCOMPLETE_TOOL_REASON.into());
        }
        if tool.logs.last().map(|entry| entry.body.as_str())
            != Some(RESTORED_INCOMPLETE_TOOL_REASON)
        {
            tool.logs.push(ToolLogEntry {
                title: "已中断".into(),
                body: RESTORED_INCOMPLETE_TOOL_REASON.into(),
            });
            if tool.logs.len() > 12 {
                let keep_from = tool.logs.len() - 12;
                tool.logs.drain(0..keep_from);
            }
        }
        updated_ids.push(tool.id.to_string());
    }

    updated_ids
}

fn lightweight_tool_invocation(tool: &ToolInvocation) -> ToolInvocation {
    let mut next = tool.clone();
    cap_string_in_place(&mut next.detail_text, SNAPSHOT_TOOL_DETAIL_CHARS);
    next.raw_input = next
        .raw_input
        .as_deref()
        .map(|value| capped_snapshot_string(value, SNAPSHOT_TOOL_RAW_CHARS));
    next.raw_output = next
        .raw_output
        .as_deref()
        .map(|value| capped_snapshot_string(value, SNAPSHOT_TOOL_OUTPUT_CHARS));
    if let Some(output) = &mut next.terminal_output {
        cap_string_in_place(&mut output.output, SNAPSHOT_TOOL_OUTPUT_CHARS);
    }
    if next.logs.len() > SNAPSHOT_TOOL_LOG_ENTRIES {
        let keep_from = next.logs.len() - SNAPSHOT_TOOL_LOG_ENTRIES;
        next.logs.drain(0..keep_from);
    }
    for entry in &mut next.logs {
        cap_string_in_place(&mut entry.body, SNAPSHOT_TOOL_LOG_CHARS);
    }
    next.diff_previews
        .retain(|preview| !looks_like_bogus_whole_file_preview(preview));
    next
}

impl UiPatchCursor {
    fn reset_from_snapshot(&mut self, snapshot: &workspace_model::UiSnapshot) {
        self.revision = snapshot.revision;
        self.workspace_id = Some(snapshot.workspace.id);
        self.session_id = Some(snapshot.session.id);
        self.timeline_len = snapshot.timeline.len();
        self.message_bodies = snapshot
            .messages
            .iter()
            .map(|message| (message.id, message.body.clone()))
            .collect();
        self.known_tool_ids = snapshot.tools.iter().map(|tool| tool.id).collect();
    }
}

fn capped_snapshot_string(value: &str, max_chars: usize) -> String {
    let mut output = value.to_string();
    cap_string_in_place(&mut output, max_chars);
    output
}

fn cap_string_in_place(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }
    let mut capped: String = value.chars().take(max_chars).collect();
    capped.push_str("\n...");
    *value = capped;
}

fn looks_like_bogus_whole_file_preview(preview: &ToolDiffPreview) -> bool {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in preview.hunks.iter().flat_map(|hunk| &hunk.lines) {
        match line.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    added >= 100 && (removed == 0 || added > removed * 4)
}

fn prompt_text(prompt: &[UserPromptContent]) -> Option<String> {
    let text = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => Some(text.trim()),
            UserPromptContent::Image { .. } | UserPromptContent::File { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if text.is_empty() { None } else { Some(text) }
}

fn prompt_has_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

fn prompt_has_file(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

fn is_codex_agent_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    normalized == "codex" || normalized == "codex-acp"
}

fn display_codex_provider(provider: &str) -> &str {
    match provider {
        "default" => "默认",
        "venus" => "Venus",
        "deepseek" => "DeepSeek",
        other => other,
    }
}

fn markdown_image_alt(name: Option<&str>) -> String {
    name.unwrap_or("attached image")
        .replace(['\n', '\r', '[', ']'], " ")
        .trim()
        .to_string()
}

fn prompt_display_body(prompt: &[UserPromptContent]) -> String {
    let mut parts = Vec::new();
    if let Some(text) = prompt_text(prompt) {
        parts.push(text);
    }
    parts.extend(prompt.iter().filter_map(|content| match content {
        UserPromptContent::Image {
            name,
            thumbnail_data,
            thumbnail_mime_type,
            ..
        } => {
            let alt = markdown_image_alt(name.as_deref());
            thumbnail_data.as_ref().map_or_else(
                || Some(format!("[Image: {alt}]")),
                |data| {
                    let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                    Some(format!("![Image: {alt}](data:{mime_type};base64,{data})"))
                },
            )
        }
        UserPromptContent::File { name, .. } => Some(format!("[File: {name}]")),
        UserPromptContent::Text { .. } => None,
    }));
    parts.join("\n\n")
}

impl Application {
    fn bump_revision(&mut self) {
        self.ui.revision = self.ui.revision.saturating_add(1);
    }

    pub fn lightweight_ui_snapshot(&self) -> workspace_model::UiSnapshot {
        workspace_model::UiSnapshot {
            revision: self.ui.revision,
            workspace: self.ui.workspace.clone(),
            session: self.ui.session.clone(),
            session_config: self.ui.session_config.clone(),
            prompt_capabilities: self.ui.prompt_capabilities.clone(),
            available_commands: self.ui.available_commands.clone(),
            agent_plan: self.ui.agent_plan.clone(),
            messages: self.ui.messages.clone(),
            timeline: self.ui.timeline.clone(),
            tools: self
                .ui
                .tools
                .iter()
                .map(lightweight_tool_invocation)
                .collect(),
            repository: self.ui.repository.clone(),
            inspector_tab: self.ui.inspector_tab.clone(),
            inspector_sections: self.ui.inspector_sections.clone(),
            session_changes: self
                .ui
                .session_changes
                .iter()
                .map(|change| SessionFileChange {
                    path: change.path.clone(),
                    change_type: change.change_type.clone(),
                    old_text: None,
                    new_text: String::new(),
                    added_lines: change.added_lines,
                    removed_lines: change.removed_lines,
                    timestamp: change.timestamp.clone(),
                })
                .collect(),
            review_changes: self.ui.review_changes.clone(),
            turn_changes: self.ui.turn_changes.clone(),
            thinking_status: self.ui.thinking_status.clone(),
        }
    }

    pub fn lightweight_ui_update(
        &mut self,
        cursor: &mut UiPatchCursor,
    ) -> Option<UiSnapshotUpdate> {
        let same_target = cursor.workspace_id == Some(self.ui.workspace.id)
            && cursor.session_id == Some(self.ui.session.id);

        if same_target && self.ui.revision == cursor.revision {
            return None;
        }

        if cursor.revision == 0 || !same_target {
            let snapshot = self.lightweight_ui_snapshot();
            cursor.reset_from_snapshot(&snapshot);
            self.dirty_tool_call_ids.clear();
            return Some(UiSnapshotUpdate::Full(snapshot));
        }

        let mut messages = Vec::new();
        let mut message_deltas = Vec::new();
        let mut current_message_ids = HashSet::new();
        for message in &self.ui.messages {
            current_message_ids.insert(message.id);
            match cursor.message_bodies.get(&message.id) {
                Some(previous_body) if previous_body == &message.body => {}
                Some(previous_body)
                    if message.body.starts_with(previous_body)
                        && message.body.is_char_boundary(previous_body.len()) =>
                {
                    message_deltas.push(ChatMessageDelta {
                        id: message.id,
                        append: message.body[previous_body.len()..].to_string(),
                    });
                    cursor
                        .message_bodies
                        .insert(message.id, message.body.clone());
                }
                _ => {
                    messages.push(message.clone());
                    cursor
                        .message_bodies
                        .insert(message.id, message.body.clone());
                }
            }
        }
        cursor
            .message_bodies
            .retain(|message_id, _| current_message_ids.contains(message_id));

        let timeline_start = cursor.timeline_len.min(self.ui.timeline.len());
        let timeline = self.ui.timeline[timeline_start..].to_vec();
        cursor.timeline_len = self.ui.timeline.len();

        let mut tools = Vec::new();
        let dirty_tool_call_ids = std::mem::take(&mut self.dirty_tool_call_ids);
        let mut emitted_tool_ids = HashSet::new();
        for call_id in dirty_tool_call_ids {
            if let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == call_id) {
                cursor.known_tool_ids.insert(tool.id);
                emitted_tool_ids.insert(tool.id);
                tools.push(lightweight_tool_invocation(tool));
            }
        }
        for tool in &self.ui.tools {
            if cursor.known_tool_ids.insert(tool.id) && emitted_tool_ids.insert(tool.id) {
                tools.push(lightweight_tool_invocation(tool));
            }
        }
        let current_tool_ids = self
            .ui
            .tools
            .iter()
            .map(|tool| tool.id)
            .collect::<HashSet<_>>();
        cursor
            .known_tool_ids
            .retain(|tool_id| current_tool_ids.contains(tool_id));

        cursor.revision = self.ui.revision;
        cursor.workspace_id = Some(self.ui.workspace.id);
        cursor.session_id = Some(self.ui.session.id);

        Some(UiSnapshotUpdate::Patch(UiSnapshotPatch {
            revision: self.ui.revision,
            session: self.ui.session.clone(),
            session_config: self.ui.session_config.clone(),
            prompt_capabilities: self.ui.prompt_capabilities.clone(),
            available_commands: self.ui.available_commands.clone(),
            agent_plan: self.ui.agent_plan.clone(),
            messages,
            message_deltas,
            timeline_start,
            timeline,
            tools,
            inspector_tab: self.ui.inspector_tab.clone(),
            inspector_sections: self.ui.inspector_sections.clone(),
            session_changes: self
                .ui
                .session_changes
                .iter()
                .map(|change| SessionFileChange {
                    path: change.path.clone(),
                    change_type: change.change_type.clone(),
                    old_text: None,
                    new_text: String::new(),
                    added_lines: change.added_lines,
                    removed_lines: change.removed_lines,
                    timestamp: change.timestamp.clone(),
                })
                .collect(),
            review_changes: self.ui.review_changes.clone(),
            turn_changes: self.ui.turn_changes.clone(),
            thinking_status: self.ui.thinking_status.clone(),
        }))
    }

    pub fn bootstrap(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::bootstrap_with_app_paths(workspace_root, agent_command, AppPaths::resolve()?)
    }

    pub fn bootstrap_with_app_paths(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let workspace_root = workspace_root.as_ref();
        let agent_command = agent_command.into();
        crate::startup_perf::mark(
            "app/bootstrap/start",
            format!(
                "workspace={} agent_command_len={}",
                workspace_root.display(),
                agent_command.len()
            ),
        );
        crate::startup_perf::measure("app/bootstrap/ensure_dirs", "", || {
            app_paths.ensure_standard_dirs()
        })?;
        let mut ui = crate::startup_perf::measure(
            "app/bootstrap/build_initial_ui",
            workspace_root.display().to_string(),
            || build_initial_ui(workspace_root),
        )?;

        let store = crate::startup_perf::measure(
            "app/bootstrap/session_store_open",
            workspace_root.display().to_string(),
            || SessionStore::open(app_paths.root(), workspace_root),
        )?;

        // Read ACP port from settings.
        let settings = crate::startup_perf::measure("app/bootstrap/load_settings", "", || {
            crate::settings::load_app_settings(&app_paths)
        });
        let acp_port = settings.acp_port;

        let existing_sessions =
            crate::startup_perf::measure("app/bootstrap/list_sessions", "", || {
                store.list_sessions().unwrap_or_default()
            });
        crate::startup_perf::mark(
            "app/bootstrap/list_sessions_count",
            existing_sessions.len().to_string(),
        );
        let most_recent_session = existing_sessions.first();
        let requested_agent_label =
            crate::startup_perf::measure("app/bootstrap/agent_label_for_command", "", || {
                crate::settings::agent_label_for_command(&agent_command)
            });
        let persisted_agent_command = most_recent_session
            .and_then(|session| session.agent_cli.as_deref())
            .filter(|label| *label != requested_agent_label)
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &app_paths)
            });
        let agent_command = persisted_agent_command.unwrap_or(agent_command);

        // Check for existing session and its ACP session ID for --resume
        let resume_session_id = most_recent_session.and_then(|s| s.acp_session_id.clone());

        // If resuming an existing session, skip replay events from session/load
        let skip_replay = resume_session_id.is_some();

        let session = crate::startup_perf::measure(
            "app/bootstrap/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: ui.workspace.root.display().to_string(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: ui.session.model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: crate::settings::agent_env_for_command(&agent_command, &app_paths),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port,
                })
            },
        )?;

        // Try to restore the most recent session, otherwise create a new one
        let (needs_title, seq_counter, pending_model_restore) = match existing_sessions.as_slice() {
            [recent, ..] => {
                // list_sessions orders by updated_at DESC
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) =
                    crate::startup_perf::measure("app/bootstrap/load_session", session_id, || {
                        store.load_session(session_id)
                    })
                {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    let mut tools = tools;
                    let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
                    for tool_id in &interrupted_tool_ids {
                        if let Some(tool) =
                            tools.iter().find(|tool| tool.id.to_string() == *tool_id)
                        {
                            let _ = store.update_tool(
                                tool_id,
                                "Interrupted",
                                tool.raw_output.as_deref(),
                                tool.error.as_deref(),
                            );
                        }
                    }
                    let mut pending_model_restore = None;
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        pending_model_restore = Some(model.clone());
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    // Historical diffs are now loaded through scoped change-set APIs.
                    // Keep legacy arrays empty on session restore so they cannot act as
                    // the primary source for review or timeline diff hydration.
                    ui.session_changes.clear();
                    ui.review_changes.clear();
                    ui.turn_changes.clear();
                    let seq =
                        crate::startup_perf::measure("app/bootstrap/next_seq", session_id, || {
                            store.next_seq(session_id).unwrap_or(1)
                        });
                    let needs_title = recent.title == "新会话";
                    (needs_title, seq, pending_model_restore)
                } else {
                    // Failed to load — create new session
                    let session_id = ui.session.id.to_string();
                    crate::startup_perf::measure(
                        "app/bootstrap/create_session_after_load_failed",
                        &session_id,
                        || store.create_session(&session_id, &ui.session.model),
                    )?;
                    (true, 1, None)
                }
            }
            _ => {
                // No sessions exist — create a new one
                let session_id = ui.session.id.to_string();
                crate::startup_perf::measure(
                    "app/bootstrap/create_session_empty",
                    &session_id,
                    || store.create_session(&session_id, &ui.session.model),
                )?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        // Determine which agent CLI this session is using. Preserve the per-session
        // persisted value when reopening, instead of overwriting it with the global
        // settings default.
        let agent_cli_label =
            crate::startup_perf::measure("app/bootstrap/resolve_session_agent_label", "", || {
                store
                    .get_session_agent_cli(&ui.session.id.to_string())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| crate::settings::agent_label_for_command(&agent_command))
            });
        ui.session.agent_cli = Some(agent_cli_label.clone());
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_agent_cli", "", || {
            store.update_session_agent_cli(&ui.session.id.to_string(), &agent_cli_label)
        });
        if is_codex_agent_label(&agent_cli_label) {
            let session_id = ui.session.id.to_string();
            if store
                .get_session_codex_provider(&session_id)
                .ok()
                .flatten()
                .is_none()
            {
                let provider = crate::settings::codex_current_provider(&app_paths);
                let _ = crate::startup_perf::measure(
                    "app/bootstrap/update_session_codex_provider",
                    "",
                    || store.update_session_codex_provider(&session_id, &provider),
                );
            }
        }
        let _ = crate::startup_perf::measure("app/bootstrap/set_permission_mode", "", || {
            session.set_permission_mode(ui.session.mode.as_deref().unwrap_or("Build"))
        });
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_model_mode", "", || {
            store.update_session_model_mode(
                &ui.session.id.to_string(),
                &ui.session.model,
                ui.session.mode.as_deref(),
            )
        });

        let file_tracker = crate::startup_perf::measure(
            "app/bootstrap/file_tracker_new",
            workspace_root.display().to_string(),
            || FileChangeTracker::new(workspace_root),
        );
        crate::startup_perf::mark("app/bootstrap/end", "");

        Ok(Self {
            ui,
            session,
            store,
            app_paths,
            agent_command,
            acp_port,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay,
            pending_model_restore,
            file_tracker,
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            inline_think_filter: InlineThinkFilter::default(),
        })
    }

    pub fn send_prompt(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.inline_think_filter.reset();
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;
        let events = self.session.send_prompt(prompt)?;
        let turn_stop_reason = events.iter().rev().find_map(|event| match event {
            ClientEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        });
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        if let Some(stop_reason) = turn_stop_reason.as_deref()
            && let Some(notice) =
                turn_finished_notice(stop_reason, self.ui.session.agent_cli.as_deref())
        {
            self.push_system_message(notice);
        }
        self.ui.session.status = SessionStatus::Idle;
        Ok(())
    }

    pub fn send_prompt_background(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.send_prompt_content_background(vec![UserPromptContent::text(prompt.into())])
    }

    pub fn send_prompt_content_background(
        &mut self,
        prompt: Vec<UserPromptContent>,
    ) -> anyhow::Result<()> {
        if self.in_flight_prompt.is_some() {
            let error = anyhow::anyhow!("提示请求已在运行中");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "图片提示".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("提示内容不能为空");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_image(&prompt) && !self.ui.prompt_capabilities.image {
            let error = anyhow::anyhow!("当前智能体不支持图片提示");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_file(&prompt) && !self.ui.prompt_capabilities.embedded_context {
            let error = anyhow::anyhow!("当前智能体不支持文件附件");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        if !self.session.is_alive() {
            if self.session.last_error().is_none() && self.should_auto_reconnect_after_clean_exit()
            {
                self.reconnect_session().map_err(anyhow::Error::msg)?;
            } else {
                let reason = self
                    .session
                    .last_error()
                    .unwrap_or_else(|| "ACP 子进程意外退出".to_string());
                let error = anyhow::anyhow!(reason);
                self.push_system_message(format!("会话已断开：{error}"));
                return Err(error);
            }
        }

        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::User,
            body: display_body,
            created_at: current_timestamp(),
        };
        let message_id = message.id;

        // Persist user message to SQLite
        let seq = self.next_seq();
        let _ = self.store.insert_message(
            &self.ui.session.id.to_string(),
            &message.id.to_string(),
            "User",
            &message.body,
            seq,
        );

        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;
        self.review_changes_started = false;
        self.current_turn_user_message_id = Some(message_id);
        self.inline_think_filter.reset();

        // Step 1: Immediately set a truncated title from user prompt (no delay)
        if self.needs_title && self.ui.session.title == "新会话" {
            let title = extract_title_from_prompt(&title_source);
            self.ui.session.title = title.clone();
            self.provisional_prompt_title = Some(title.clone());
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }

        // User is sending a new prompt — drain any buffered replay events
        // from session/load before sending, so they don't mix with real responses.
        if self.skip_replay {
            self.session.drain_events();
            self.skip_replay = false;
        }

        let task = self.session.send_prompt_content_async(prompt)?;
        self.in_flight_prompt = Some(InFlightPrompt { task });
        self.bump_revision();
        Ok(())
    }

    pub fn poll_prompt_progress(&mut self) {
        // Detect subprocess crash even when no prompt is in flight
        if self.in_flight_prompt.is_none()
            && !self.session.is_alive()
            && self.ui.session.status != SessionStatus::Interrupted
        {
            let last_error = self.session.last_error();
            if last_error.is_none() && self.should_auto_reconnect_after_clean_exit() {
                if let Err(error) = self.reconnect_session() {
                    let reason = format!("ACP 子进程退出且重连失败：{error}");
                    self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                        reason: reason.clone(),
                    });
                    self.push_system_message(format!("会话已断开：{}", reason));
                    self.bump_revision();
                }
                return;
            }

            let reason = last_error.unwrap_or_else(|| "ACP 子进程意外退出".to_string());
            self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                reason: reason.clone(),
            });
            self.push_system_message(format!("会话已断开：{}", reason));
            self.bump_revision();
            return;
        }

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            self.session.update_session_id(&events);
            let has_events = !events.is_empty();
            for event in events {
                self.apply_event_and_restore_model(event);
            }
            if has_events {
                self.bump_revision();
            }
            return;
        };

        let events = match in_flight.task.collect_ready_events(&mut self.session) {
            Ok(events) => events,
            Err(error) => {
                self.ui.session.status = SessionStatus::Interrupted;
                self.ui.agent_plan.clear();
                self.push_system_message(format!(
                    "从 `{}` 读取 ACP 事件失败：{}",
                    self.agent_command, error
                ));
                self.in_flight_prompt = None;
                self.current_turn_user_message_id = None;
                self.bump_revision();
                return;
            }
        };

        let is_finished = in_flight.task.is_finished();

        // If skip_replay is active, discard all events except SessionStarted and TurnFinished.
        // These are replay events from session/load that we already have in SQLite.
        if self.skip_replay {
            // Only keep SessionStarted (to update the ACP session ID) and check for TurnFinished
            for event in &events {
                if let ClientEvent::SessionStarted { .. } = event {
                    self.session.update_session_id(&[event.clone()]);
                    self.persist_event(event);
                    self.bump_revision();
                }
            }
            if is_finished {
                self.skip_replay = false;
                self.in_flight_prompt = None;
                self.current_turn_user_message_id = None;
                self.ui.session.status = SessionStatus::Idle;
                self.bump_revision();
            }
            return;
        }

        // Preprocess ToolDiff events: fill in old_text from the correct baseline.
        // For the tool card diff, old_text should be "what was on disk when the tool started"
        // so the card shows what THIS tool changed.
        // For session-level changes, the reducer's upsert_session_change preserves the
        // first-ever baseline separately.
        let workspace_root = self.ui.workspace.root.clone();
        let mut events = events;
        let mut had_file_changes = false;
        let mut batch_file_versions = HashMap::<String, String>::new();
        let turn_stop_reason = events.iter().rev().find_map(|event| match event {
            ClientEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        });

        // Events are collected in batches. Some agents emit ToolStarted and ToolDiff in
        // the same batch after the file has already been written. Start recording before
        // the ToolDiff preprocessing pass so `get_any_baseline_text` can still supply
        // a baseline instead of letting the card diff against an empty file.
        for event in &events {
            if let ClientEvent::ToolStarted { id, raw_input, .. } = event {
                self.file_tracker
                    .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
            }
        }

        for event in events.iter_mut() {
            if let ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
                ..
            } = event
            {
                had_file_changes = true;
                // Normalize path to workspace-relative with forward slashes
                let normalized = normalize_path_for_storage(path, &workspace_root);
                self.file_tracker.add_candidate(id, normalized.clone());
                let abs_path = workspace_root.join(&normalized);
                if let Some((expanded_old, expanded_new)) =
                    expand_tool_diff_fragment_from_disk(&abs_path, old_text.as_deref(), new_text)
                {
                    *old_text = Some(expanded_old);
                    *new_text = expanded_new;
                }
                let old_text_is_untrusted = old_text.as_deref().map_or(true, |text| {
                    text.is_empty() || looks_like_fragment_to_full_file_text(text, new_text)
                });
                if old_text_is_untrusted {
                    // 1. For multiple ToolDiffs for the same file in one poll batch,
                    // use the previous diff's new_text. This keeps each ToolCard scoped
                    // to this tool's own edit instead of every card comparing against an
                    // empty/missing base and showing the whole file as added.
                    if let Some(baseline) = self.tool_diff_baseline_text(
                        id,
                        &normalized,
                        new_text,
                        &batch_file_versions,
                    ) {
                        *old_text = Some(baseline);
                    } else if old_text.as_deref().is_some_and(str::is_empty) {
                        *old_text = None;
                    }
                } else if old_text.as_deref().is_some_and(|text| {
                    normalize_diff_text_for_session_change(text)
                        == normalize_diff_text_for_session_change(new_text)
                }) {
                    *old_text = None;
                }

                // Last resort requested by user: read the file directly only when
                // the file on disk is different from the preview target. If it is
                // already equal, treating an unknown baseline as "created" would
                // make the UI show the whole file as added.
                if old_text.is_none()
                    && let Ok(content) = std::fs::read_to_string(&abs_path)
                    && normalize_diff_text_for_session_change(&content)
                        != normalize_diff_text_for_session_change(new_text)
                {
                    *old_text = Some(normalize_diff_text_for_session_change(&content));
                }
                batch_file_versions.insert(normalized.clone(), new_text.clone());
                *path = normalized;
            }
        }

        // Process events and track tool lifecycle for file change detection
        let mut ui_changed = !events.is_empty();
        let mut completed_tool_ids = Vec::new();
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, raw_input, .. } => {
                    self.file_tracker
                        .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
                }
                ClientEvent::ToolUpdated { id, raw_input, .. } => {
                    for path in tool_event_hint_paths(raw_input.as_deref()) {
                        self.file_tracker.add_candidate(id, path);
                    }
                }
                ClientEvent::ToolCompleted { id, .. } | ClientEvent::ToolFailed { id, .. } => {
                    completed_tool_ids.push(id.clone());
                    let changes = self.file_tracker.finish_recording(id);
                    had_file_changes |= self.apply_tracker_changes(id, changes);
                }
                _ => {}
            }
            self.apply_event_with_dirty_tracking(event);
            if let ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
                ..
            } = event
            {
                let change_type = self.tool_diff_change_type(id, path, old_text.as_deref());
                self.upsert_review_file_change(
                    path,
                    change_type,
                    old_text.clone(),
                    new_text.clone(),
                );
                had_file_changes = true;
            }
        }
        self.session.update_session_id(&events);

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        if !completed_tool_ids.is_empty() {
            had_file_changes |= self.detect_file_writes_from_tools(&completed_tool_ids);
        }

        // Persist session_changes to SQLite after all file-change sources have run.
        if had_file_changes {
            self.persist_file_changes();
            self.persist_review_file_changes();
        }

        if is_finished {
            if self.ui.session.status == SessionStatus::Streaming {
                self.ui.session.status = SessionStatus::Idle;
                ui_changed = true;
            }

            // Step 2: After first turn, try to refine title from assistant's response
            if self.needs_title && !self.agent_title_received {
                self.needs_title = false;
                self.refine_session_title();
                ui_changed = true;
            }

            ui_changed |= self.persist_current_turn_file_changes();
            if let Some(stop_reason) = turn_stop_reason.as_deref()
                && let Some(notice) =
                    turn_finished_notice(stop_reason, self.ui.session.agent_cli.as_deref())
            {
                self.push_system_message(notice);
                ui_changed = true;
            }
            self.current_turn_user_message_id = None;
            self.in_flight_prompt = None;
        }

        if ui_changed || had_file_changes {
            self.bump_revision();
        }
    }

    pub fn has_in_flight_prompt(&self) -> bool {
        self.in_flight_prompt.is_some()
    }

    fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        false
    }

    pub fn has_running_codex_acp_session(&self) -> bool {
        self.is_codex_acp_session() && self.session.is_alive()
    }

    fn is_codex_acp_session(&self) -> bool {
        self.ui
            .session
            .agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or_else(|| {
                self.agent_command
                    .to_ascii_lowercase()
                    .contains("codex-acp")
            })
    }

    fn persist_current_codex_provider_if_needed(&self) {
        if !self.is_codex_acp_session() {
            return;
        }
        let provider = crate::settings::codex_current_provider(&self.app_paths);
        let _ = self
            .store
            .update_session_codex_provider(&self.ui.session.id.to_string(), &provider);
    }

    fn ensure_codex_provider_matches_for_resume(&self, session_id: &str) -> Result<(), String> {
        let agent_cli = self.store.get_session_agent_cli(session_id).unwrap_or(None);
        if !agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or(false)
        {
            return Ok(());
        }

        let Some(stored_provider) = self
            .store
            .get_session_codex_provider(session_id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let current_provider = crate::settings::codex_current_provider(&self.app_paths);
        if stored_provider == current_provider {
            return Ok(());
        }

        Err(format!(
            "配置不一致，请新开会话，或者去切换配置。当前配置：{}，会话配置：{}",
            display_codex_provider(&current_provider),
            display_codex_provider(&stored_provider)
        ))
    }

    pub fn cancel_prompt(&mut self) -> Result<(), String> {
        if self.in_flight_prompt.is_none() {
            return Ok(());
        }
        self.session
            .cancel_prompt()
            .map_err(|error| error.to_string())?;
        self.mark_current_turn_cancelled();
        self.bump_revision();
        Ok(())
    }

    fn mark_current_turn_cancelled(&mut self) {
        let session_id = self.ui.session.id.to_string();
        let mut cancelled_tools = Vec::new();
        let mut dirty_tool_call_ids = Vec::new();

        for tool in self
            .ui
            .tools
            .iter_mut()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
        {
            dirty_tool_call_ids.push(tool.call_id.clone());
            tool.status = ToolStatus::Interrupted;
            if tool.summary.trim().is_empty()
                || tool.summary == "等待活动"
                || tool.summary.starts_with("等待权限")
            {
                tool.summary = "已取消".into();
            }
            if tool.kind == "permission" && tool.permission_decision.is_none() {
                tool.permission_decision = Some("已取消".into());
            }
            tool.logs.push(ToolLogEntry {
                title: "已取消".into(),
                body: "客户端发送了 session/cancel 取消当前轮次".into(),
            });
            cancelled_tools.push(tool.clone());
        }
        self.dirty_tool_call_ids.extend(dirty_tool_call_ids);

        for tool in cancelled_tools {
            let seq = self.next_seq();
            let _ = self.store.insert_tool(&session_id, &tool, seq);
        }
    }

    pub fn set_session_config_control(
        &mut self,
        control_id: &str,
        value_id: &str,
    ) -> Result<workspace_model::SessionConfigState, String> {
        if self.in_flight_prompt.is_some() || self.ui.session.status != SessionStatus::Idle {
            return Err("会话控件只能在会话空闲时更改".into());
        }

        let control = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.id == control_id)
            .cloned()
            .ok_or_else(|| format!("未知的会话控件：{control_id}"))?;

        if !control.enabled {
            return Err(format!("会话控件不可用：{}", control.label));
        }
        if !control.choices.iter().any(|choice| choice.id == value_id) {
            return Err(format!("{} 的值未知：{value_id}", control.label));
        }

        let events = match control.source {
            SessionConfigSource::ConfigOption => self
                .session
                .set_config_option(control.id, value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LegacyMode => self
                .session
                .set_mode(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LocalMode => {
                self.session
                    .set_permission_mode(value_id)
                    .map_err(|error| error.to_string())?;
                vec![ClientEvent::SessionConfigValueChanged {
                    control_id: control.id,
                    value_id: value_id.to_string(),
                    value_label: control
                        .choices
                        .iter()
                        .find(|choice| choice.id == value_id)
                        .map(|choice| choice.label.clone()),
                }]
            }
        };

        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.persist_session_model_mode();
        self.bump_revision();

        Ok(self.ui.session_config.clone())
    }

    pub fn resolve_tool_permission(
        &mut self,
        request_id: &str,
        option_id: Option<String>,
    ) -> Result<(), String> {
        let delivered_to_acp_request = self
            .session
            .resolve_permission(request_id, option_id.clone())
            .map_err(|error| error.to_string())?;

        if !delivered_to_acp_request {
            let decision = option_id.unwrap_or_else(|| "deny".into());
            self.session
                .resolve_codebuddy_interruption(request_id, &decision)
                .map_err(|error| error.to_string())?;
            self.mark_tool_permission_selected(request_id, &decision);
        } else {
            let decision = option_id.as_deref().unwrap_or("cancelled");
            self.mark_tool_permission_selected(request_id, decision);
        }

        Ok(())
    }

    fn mark_tool_permission_selected(&mut self, request_id: &str, decision: &str) {
        if let Some(tool) = self
            .ui
            .tools
            .iter_mut()
            .find(|tool| tool.call_id == request_id)
        {
            let outcome = format!("Permission selected: {decision}");
            tool.summary = outcome.clone();
            tool.permission_options.clear();
            tool.permission_decision = Some(outcome);
            self.mark_tool_call_dirty(request_id);
            self.bump_revision();
        }
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        self.store.list_sessions().map_err(|e| e.to_string())
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(id)?;

        // Load session data from SQLite
        let (messages, mut tools, timeline) =
            self.store.load_session(id).map_err(|e| e.to_string())?;
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = tools.iter().find(|tool| tool.id.to_string() == *tool_id) {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        let (model, mode) = self
            .store
            .get_session_model_mode(id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| (self.ui.session.model.clone(), self.ui.session.mode.clone()));
        let mode = mode.or_else(|| Some("Build".into()));
        let stored_agent_cli = self.store.get_session_agent_cli(id).unwrap_or(None);
        let session_agent_command = stored_agent_cli
            .as_deref()
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
            })
            .unwrap_or_else(|| self.agent_command.clone());

        // Get the stored ACP session ID for resume
        let resume_acp_id = self.store.get_acp_session_id(id).unwrap_or(None);

        // Start a new ACP session handle
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: session_agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(
                &session_agent_command,
                &self.app_paths,
            ),
            resume_session_id: resume_acp_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));

        // Update UI snapshot
        self.ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        self.ui.session.model = model;
        self.ui.session.mode = mode;
        self.agent_command = session_agent_command;
        self.ui.session.agent_cli = stored_agent_cli.or_else(|| {
            Some(crate::settings::agent_label_for_command(
                &self.agent_command,
            ))
        });
        self.persist_current_codex_provider_if_needed();
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages = messages;
        self.ui.tools = tools;
        self.ui.timeline = timeline;
        self.ui.session.status = SessionStatus::Idle;
        self.session = session;
        self.in_flight_prompt = None;
        // Historical diffs are loaded through scoped change-set APIs. The old
        // arrays remain runtime staging buffers for compatibility, but they are
        // not restored as primary session/review/timeline state on switch.
        self.ui.session_changes.clear();
        self.ui.review_changes.clear();
        self.ui.turn_changes.clear();
        self.review_changes_started = false;
        self.current_turn_user_message_id = None;

        // Compute seq counter from loaded data
        self.seq_counter = self.store.next_seq(id).unwrap_or(1);

        // Load session title
        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            self.ui.session.title = s.title.clone();
        }

        self.needs_title = self.ui.session.title == "新会话";
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    pub fn session_create(&mut self, agent: Option<AgentCliId>) -> Result<(), String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let current_agent_command = match agent {
            Some(agent) => crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        };
        self.agent_command = current_agent_command;

        // Start a new ACP session handle (no resume for new session)
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode("Build");

        self.ui.session.id = new_id;
        self.ui.session.title = "新会话".to_string();
        self.ui.session.model = initial_model;
        self.ui.session.mode = Some("Build".into());
        let agent_cli_label = crate::settings::agent_label_for_command(&self.agent_command);
        self.ui.session.agent_cli = Some(agent_cli_label);
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.session.status = SessionStatus::Idle;
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages.clear();
        self.ui.tools.clear();
        self.ui.timeline.clear();
        self.ui.session_changes.clear();
        self.ui.review_changes.clear();
        self.ui.turn_changes.clear();
        self.session = session;
        self.in_flight_prompt = None;
        self.review_changes_started = false;
        self.current_turn_user_message_id = None;
        self.seq_counter = 1;
        self.needs_title = true;
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.pending_model_restore = None;
        self.persist_session_model_mode();
        let _ = self.store.update_session_agent_cli(
            &self.ui.session.id.to_string(),
            self.ui.session.agent_cli.as_deref().unwrap_or("CodeBuddy"),
        );
        self.persist_current_codex_provider_if_needed();

        self.bump_revision();
        Ok(())
    }

    pub fn session_delete(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            let replacement_id = self
                .store
                .list_sessions()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|session| session.id != id)
                .map(|session| session.id);

            if let Some(replacement_id) = replacement_id {
                self.session_switch(&replacement_id)?;
            } else {
                self.session_create(None)?;
            }
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(&self.ui.session.id.to_string())?;

        // Try to resume the current ACP session if we have its ID
        let resume_id = if !self.session.id.is_empty() {
            Some(self.session.id.clone())
        } else {
            self.store
                .get_acp_session_id(&self.ui.session.id.to_string())
                .unwrap_or(None)
        };

        let resume_id_for_handle = resume_id.clone();
        let has_resume_id = resume_id_for_handle.is_some();
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
            resume_session_id: resume_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        if let Some(acp_id) = resume_id_for_handle {
            session.id = acp_id;
        }

        self.session = session;
        self.ui.session.status = SessionStatus::Idle;
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut self.ui.tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = self
                .ui
                .tools
                .iter()
                .find(|tool| tool.id.to_string() == *tool_id)
            {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        self.in_flight_prompt = None;
        self.current_turn_user_message_id = None;
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.skip_replay = has_resume_id;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    // ── Title refinement ──

    /// After the first turn completes, try to extract a better title from the
    /// assistant's response. The truncated user prompt is already set as Step 1.
    /// This Step 2 tries to improve it by looking at what the assistant actually did.
    fn refine_session_title(&mut self) {
        // Find first assistant message
        let assistant_body = match self
            .ui
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
        {
            Some(m) => m.body.clone(),
            None => return, // No assistant response yet, keep truncated title
        };

        // Try to extract a meaningful title from the assistant's first sentence.
        // Common patterns: "I'll help you X", "Let me X", "Here's how to X", etc.
        let refined = extract_title_from_response(&assistant_body);
        if let Some(title) = refined {
            self.ui.session.title = title.clone();
            self.provisional_prompt_title = None;
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }
        // If extraction fails, keep the truncated user prompt title from Step 1
    }

    // ── Internal helpers ──

    fn push_system_message(&mut self, body: impl Into<String>) {
        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::System,
            body: body.into(),
            created_at: current_timestamp(),
        };
        let seq = self.next_seq();
        let _ = self.store.insert_message(
            &self.ui.session.id.to_string(),
            &message.id.to_string(),
            "System",
            &message.body,
            seq,
        );
        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
    }

    pub fn refresh_repository(&mut self) {
        match GitService::open(&self.ui.workspace.root) {
            Ok(snapshot) if snapshot != self.ui.repository => {
                self.ui.repository = snapshot;
                self.bump_revision();
            }
            Ok(_) => {}
            Err(_) if !self.ui.repository.changed_files.is_empty() => {
                self.ui.repository.changed_files.clear();
                self.bump_revision();
            }
            Err(_) => {}
        }
    }

    pub fn stage_files(&mut self, paths: &[String]) -> Result<(), String> {
        GitService::stage(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    pub fn list_change_sets(&self, request: ListChangeSetsRequest) -> Vec<ChangeSetSummary> {
        let requested_workspace = request
            .workspace_root
            .as_deref()
            .map(normalize_tracked_path)
            .unwrap_or_else(|| {
                normalize_tracked_path(&self.ui.workspace.root.display().to_string())
            });
        if requested_workspace
            != normalize_tracked_path(&self.ui.workspace.root.display().to_string())
        {
            return Vec::new();
        }

        let session_id = request.session_id.unwrap_or(self.ui.session.id).to_string();
        let mut summaries = if request.source == Some(ChangeSetSource::GitWorktree) {
            Vec::new()
        } else {
            self.store
                .list_change_sets_with_legacy(&session_id, request.source.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|summary| summary.source != ChangeSetSource::GitWorktree)
                .collect()
        };

        if request
            .source
            .as_ref()
            .is_none_or(|source| source == &ChangeSetSource::GitWorktree)
        {
            summaries.extend(self.git_worktree_change_set_summaries());
        }

        summaries
    }

    pub fn list_change_set_files(
        &self,
        request: ListChangeSetFilesRequest,
    ) -> ChangeSetFilesResponse {
        let files = if let Some(section) = git_section_from_change_set_id(&request.change_set_id) {
            self.git_worktree_file_summaries(&request.change_set_id, section)
        } else {
            self.store
                .list_change_set_files_with_legacy(&request.change_set_id)
                .unwrap_or_default()
        };

        ChangeSetFilesResponse {
            change_set_id: request.change_set_id,
            files,
        }
    }

    pub fn get_change_set_file_diff(
        &self,
        request: GetChangeSetFileDiffRequest,
    ) -> Option<FileChangeRecord> {
        if let Some(section) = git_section_from_change_set_id(&request.change_set_id) {
            return GitService::file_diff(&self.ui.workspace.root, &request.path, section)
                .ok()
                .flatten();
        }

        self.store
            .load_change_set_file_diff_with_legacy(&request.change_set_id, &request.path)
            .ok()
            .flatten()
    }

    pub fn record_manual_editor_save(
        &mut self,
        path: &str,
        before_text: Option<String>,
        after_text: String,
    ) {
        let change_set_id = self.manual_edit_change_set_id();
        let normalized_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let mut records = self.load_change_set_records(&change_set_id);
        let existing_index = records
            .iter()
            .position(|record| normalize_tracked_path(&record.path) == normalized_path);
        let base_text = existing_index
            .and_then(|index| records[index].old_text.clone())
            .or(before_text)
            .map(|text| normalize_diff_text_for_session_change(&text));
        let normalized_after_text = normalize_diff_text_for_session_change(&after_text);
        let change_type = if base_text.is_none() {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        };
        let canonical = canonical_text_diff(
            &change_type,
            base_text.as_deref(),
            Some(&normalized_after_text),
            None,
        );

        if canonical.quality == DiffQuality::Exact
            && canonical.added_lines == 0
            && canonical.removed_lines == 0
        {
            if let Some(index) = existing_index {
                records.remove(index);
                self.replace_manual_edit_change_set(records);
                self.bump_revision();
            }
            return;
        }

        let record = FileChangeRecord {
            change_set_id: change_set_id.clone(),
            path: normalized_path,
            change_type,
            old_text: canonical.old_text,
            new_text: canonical.new_text,
            added_lines: canonical.added_lines,
            removed_lines: canonical.removed_lines,
            quality: canonical.quality,
            updated_at: current_timestamp(),
        };

        if let Some(index) = existing_index {
            records[index] = record;
        } else {
            records.push(record);
        }
        self.replace_manual_edit_change_set(records);
        self.bump_revision();
    }

    /// Persist current session_changes to SQLite.
    fn persist_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        let _ = self
            .store
            .replace_file_changes(&session_id, &self.ui.session_changes);
    }

    fn persist_review_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        let _ = self
            .store
            .replace_review_file_changes(&session_id, &self.ui.review_changes);
    }

    fn current_agent_turn_change_set_id(&self) -> Option<String> {
        let user_message_id = self.current_turn_user_message_id?;
        Some(format!(
            "agent-turn:{}:{user_message_id}",
            self.ui.session.id
        ))
    }

    fn manual_edit_change_set_id(&self) -> String {
        format!("manual-edit:{}", self.ui.session.id)
    }

    fn load_change_set_records(&self, change_set_id: &str) -> Vec<FileChangeRecord> {
        self.store
            .list_change_set_files(change_set_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| {
                self.store
                    .load_change_set_file_diff(change_set_id, &summary.path)
                    .ok()
                    .flatten()
            })
            .collect()
    }

    fn make_change_set_summary(
        &self,
        id: String,
        source: ChangeSetSource,
        message_id: Option<uuid::Uuid>,
        owner_key: Option<String>,
        label: &str,
        status: ChangeSetStatus,
    ) -> ChangeSetSummary {
        ChangeSetSummary {
            id,
            source,
            session_id: Some(self.ui.session.id),
            workspace_root: self.store.workspace_root().to_string(),
            message_id,
            tool_call_id: None,
            owner_key,
            label: label.to_string(),
            added_lines: 0,
            removed_lines: 0,
            file_count: 0,
            updated_at: current_timestamp(),
            status,
        }
    }

    fn git_worktree_change_set_summaries(&self) -> Vec<ChangeSetSummary> {
        [
            ChangeSectionEntry::staged(),
            ChangeSectionEntry::unstaged(),
            ChangeSectionEntry::untracked(),
        ]
        .into_iter()
        .filter_map(|entry| {
            let files = self
                .ui
                .repository
                .changed_files
                .iter()
                .filter(|file| file.section == entry.section)
                .collect::<Vec<_>>();
            if files.is_empty() {
                return None;
            }

            Some(ChangeSetSummary {
                id: entry.change_set_id.to_string(),
                source: ChangeSetSource::GitWorktree,
                session_id: None,
                workspace_root: self.store.workspace_root().to_string(),
                message_id: None,
                tool_call_id: None,
                owner_key: Some("workspace".into()),
                label: entry.label.to_string(),
                added_lines: files.iter().map(|file| file.stats.added).sum(),
                removed_lines: files.iter().map(|file| file.stats.removed).sum(),
                file_count: files.len(),
                updated_at: current_timestamp(),
                status: ChangeSetStatus::Live,
            })
        })
        .collect()
    }

    fn git_worktree_file_summaries(
        &self,
        change_set_id: &str,
        section: ChangeSection,
    ) -> Vec<FileChangeSummary> {
        let mut summaries = self
            .ui
            .repository
            .changed_files
            .iter()
            .filter(|file| file.section == section)
            .filter_map(|file| {
                let path = file.path.display().to_string();
                let record = GitService::file_diff(&self.ui.workspace.root, &path, section.clone())
                    .ok()
                    .flatten();
                Some(match record {
                    Some(record) => file_summary_from_record(&record),
                    None => FileChangeSummary {
                        change_set_id: change_set_id.to_string(),
                        path: normalize_tracked_path(&path),
                        change_type: FileChangeType::Modified,
                        added_lines: file.stats.added,
                        removed_lines: file.stats.removed,
                        quality: DiffQuality::MissingBaseline,
                        updated_at: String::new(),
                    },
                })
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|a, b| a.path.cmp(&b.path));
        summaries
    }

    fn file_record_from_session_change(
        change_set_id: &str,
        change: &SessionFileChange,
    ) -> Option<FileChangeRecord> {
        let target_text = if change.change_type == FileChangeType::Deleted {
            None
        } else {
            Some(change.new_text.as_str())
        };
        let canonical = canonical_text_diff(
            &change.change_type,
            change.old_text.as_deref(),
            target_text,
            None,
        );
        if canonical.quality == DiffQuality::Exact
            && canonical.added_lines == 0
            && canonical.removed_lines == 0
        {
            return None;
        }

        Some(FileChangeRecord {
            change_set_id: change_set_id.to_string(),
            path: normalize_tracked_path(&change.path),
            change_type: change.change_type.clone(),
            old_text: canonical.old_text,
            new_text: canonical.new_text,
            added_lines: canonical.added_lines,
            removed_lines: canonical.removed_lines,
            quality: canonical.quality,
            updated_at: change.timestamp.clone(),
        })
    }

    fn persist_current_agent_turn_change_set(
        &self,
        message_id: Option<uuid::Uuid>,
        status: ChangeSetStatus,
    ) {
        let Some(change_set_id) = self.current_agent_turn_change_set_id() else {
            return;
        };
        let owner_key = self
            .current_turn_user_message_id
            .map(|id| format!("user-message:{id}"));
        let summary = self.make_change_set_summary(
            change_set_id.clone(),
            ChangeSetSource::AgentTurn,
            message_id,
            owner_key,
            "本轮对话",
            status,
        );
        let records = self
            .ui
            .review_changes
            .iter()
            .filter_map(|change| Self::file_record_from_session_change(&change_set_id, change))
            .collect::<Vec<_>>();
        let _ = self.store.replace_change_set(&summary, &records);
    }

    fn remove_current_agent_turn_change_set(&self) {
        let Some(change_set_id) = self.current_agent_turn_change_set_id() else {
            return;
        };
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::AgentTurn,
            None,
            self.current_turn_user_message_id
                .map(|id| format!("user-message:{id}")),
            "本轮对话",
            ChangeSetStatus::Pending,
        );
        let _ = self.store.replace_change_set(&summary, &[]);
    }

    fn replace_manual_edit_change_set(&self, mut records: Vec<FileChangeRecord>) {
        let change_set_id = self.manual_edit_change_set_id();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::ManualEdit,
            None,
            Some(format!("session:{}", self.ui.session.id)),
            "手工修改",
            ChangeSetStatus::Complete,
        );
        let _ = self.store.replace_change_set(&summary, &records);
    }

    fn persist_agent_conversation_change_set_from_turns(&self) {
        let change_set_id = format!("agent-conversation:{}", self.ui.session.id);
        let session_id = self.ui.session.id.to_string();
        let mut turn_summaries = self
            .store
            .list_change_sets_with_legacy(&session_id, Some(ChangeSetSource::AgentTurn))
            .unwrap_or_default();
        turn_summaries.retain(|summary| {
            summary.message_id.is_some()
                && matches!(
                    summary.status,
                    ChangeSetStatus::Complete | ChangeSetStatus::LegacyIncomplete
                )
        });
        let message_order = self
            .ui
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                TimelineItem::Message(message_id) => Some((*message_id, index)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        turn_summaries.sort_by(|a, b| {
            let a_order = a
                .message_id
                .and_then(|message_id| message_order.get(&message_id).copied())
                .unwrap_or(usize::MAX);
            let b_order = b
                .message_id
                .and_then(|message_id| message_order.get(&message_id).copied())
                .unwrap_or(usize::MAX);
            a_order
                .cmp(&b_order)
                .then(a.updated_at.cmp(&b.updated_at))
                .then(a.id.cmp(&b.id))
        });

        let mut aggregate = HashMap::<String, FileChangeRecord>::new();
        for summary in turn_summaries {
            let files = self
                .store
                .list_change_set_files_with_legacy(&summary.id)
                .unwrap_or_default();
            for file in files {
                let Some(record) = self
                    .store
                    .load_change_set_file_diff_with_legacy(&summary.id, &file.path)
                    .ok()
                    .flatten()
                else {
                    continue;
                };
                let path = normalize_tracked_path(&record.path);
                if let Some(existing) = aggregate.get_mut(&path) {
                    existing.new_text = record.new_text.clone();
                    existing.change_type = if existing.old_text.is_none() {
                        FileChangeType::Created
                    } else if record.change_type == FileChangeType::Deleted {
                        FileChangeType::Deleted
                    } else {
                        FileChangeType::Modified
                    };
                    existing.quality = if existing.quality == DiffQuality::Exact
                        && record.quality == DiffQuality::Exact
                    {
                        DiffQuality::Exact
                    } else {
                        DiffQuality::LegacyIncomplete
                    };
                    existing.updated_at = record.updated_at;
                } else {
                    aggregate.insert(
                        path.clone(),
                        FileChangeRecord {
                            change_set_id: change_set_id.clone(),
                            path,
                            ..record
                        },
                    );
                }
            }
        }

        let mut records = aggregate
            .into_values()
            .map(|mut record| {
                let canonical = canonical_text_diff(
                    &record.change_type,
                    record.old_text.as_deref(),
                    record.new_text.as_deref(),
                    Some(record.quality.clone()),
                );
                record.change_set_id = change_set_id.clone();
                record.old_text = canonical.old_text;
                record.new_text = canonical.new_text;
                record.added_lines = canonical.added_lines;
                record.removed_lines = canonical.removed_lines;
                record.quality = canonical.quality;
                record
            })
            .collect::<Vec<_>>();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::AgentConversation,
            None,
            Some(format!("session:{}", self.ui.session.id)),
            "整体对话",
            ChangeSetStatus::Complete,
        );
        let _ = self.store.replace_change_set(&summary, &records);
    }

    fn persist_current_turn_file_changes(&mut self) -> bool {
        let Some(message_id) = self.current_turn_assistant_message_id() else {
            return false;
        };

        if !self.review_changes_started {
            let session_id = self.ui.session.id.to_string();
            let before = self.ui.turn_changes.len();
            self.ui
                .turn_changes
                .retain(|entry| entry.message_id != message_id);
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &message_id, &[]);
            self.remove_current_agent_turn_change_set();
            self.persist_agent_conversation_change_set_from_turns();
            return self.ui.turn_changes.len() != before;
        }

        let mut changes = self.ui.review_changes.clone();
        sanitize_session_file_changes(&mut changes);
        let session_id = self.ui.session.id.to_string();

        if changes.is_empty() {
            let before = self.ui.turn_changes.len();
            self.ui
                .turn_changes
                .retain(|entry| entry.message_id != message_id);
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &message_id, &[]);
            self.remove_current_agent_turn_change_set();
            self.persist_agent_conversation_change_set_from_turns();
            return self.ui.turn_changes.len() != before;
        }

        let mut changed = false;
        if let Some(index) = self
            .ui
            .turn_changes
            .iter()
            .position(|entry| entry.message_id == message_id)
        {
            if self.ui.turn_changes[index].changes != changes {
                self.ui.turn_changes[index].changes = changes.clone();
                changed = true;
            }
        } else {
            self.ui.turn_changes.push(TurnFileChanges {
                message_id,
                changes: changes.clone(),
            });
            changed = true;
        }

        let _ = self
            .store
            .replace_turn_file_changes(&session_id, &message_id, &changes);
        self.persist_current_agent_turn_change_set(Some(message_id), ChangeSetStatus::Complete);
        self.persist_agent_conversation_change_set_from_turns();
        changed
    }

    fn current_turn_assistant_message_id(&self) -> Option<uuid::Uuid> {
        let start_id = self.current_turn_user_message_id?;
        let mut after_start = false;
        let mut assistant_id = None;

        for item in &self.ui.timeline {
            let TimelineItem::Message(message_id) = item else {
                continue;
            };
            if *message_id == start_id {
                after_start = true;
                continue;
            }
            if !after_start {
                continue;
            }
            if self
                .ui
                .messages
                .iter()
                .any(|message| message.id == *message_id && message.role == MessageRole::Assistant)
            {
                assistant_id = Some(*message_id);
            }
        }

        assistant_id
    }

    fn begin_review_changes_if_needed(&mut self) {
        if self.review_changes_started {
            return;
        }
        self.review_changes_started = true;
        self.ui.review_changes.clear();
        self.persist_review_file_changes();
        self.remove_current_agent_turn_change_set();
    }

    fn upsert_review_file_change(
        &mut self,
        path: &str,
        change_type: FileChangeType,
        old_text: Option<String>,
        new_text: String,
    ) {
        self.begin_review_changes_if_needed();
        let normalized_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let normalized_old_text = old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let normalized_new_text = normalize_diff_text_for_session_change(&new_text);
        let existing_index = self
            .ui
            .review_changes
            .iter()
            .position(|change| normalize_tracked_path(&change.path) == normalized_path);
        let base_text = existing_index
            .and_then(|index| self.ui.review_changes[index].old_text.clone())
            .or(normalized_old_text);

        if !is_trustworthy_review_change_text(
            &change_type,
            base_text.as_deref(),
            &normalized_new_text,
        ) {
            return;
        }

        if base_text.as_deref().unwrap_or_default() == normalized_new_text {
            if let Some(index) = existing_index {
                self.ui.review_changes.remove(index);
            }
            self.persist_review_file_changes();
            self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
            return;
        }

        let canonical = canonical_text_diff(
            &change_type,
            base_text.as_deref(),
            Some(&normalized_new_text),
            None,
        );
        let added = canonical.added_lines;
        let removed = canonical.removed_lines;
        if added == 0 && removed == 0 {
            if let Some(index) = existing_index {
                self.ui.review_changes.remove(index);
            }
            self.persist_review_file_changes();
            self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
            return;
        }

        let timestamp = current_timestamp();
        if let Some(index) = existing_index {
            let existing = &mut self.ui.review_changes[index];
            existing.new_text = canonical.new_text.unwrap_or(normalized_new_text);
            existing.change_type = change_type;
            existing.added_lines = added;
            existing.removed_lines = removed;
            existing.timestamp = timestamp;
        } else {
            self.ui.review_changes.push(SessionFileChange {
                path: normalized_path,
                change_type,
                old_text: base_text,
                new_text: canonical.new_text.unwrap_or(normalized_new_text),
                added_lines: added,
                removed_lines: removed,
                timestamp,
            });
        }
        self.persist_review_file_changes();
        self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
    }

    fn upsert_review_file_change_for_tool(
        &mut self,
        path: &str,
        change_type: FileChangeType,
        exact_edit: Option<&ExactEditText>,
        fallback_old_text: Option<String>,
        fallback_new_text: String,
    ) {
        let fallback_new_text = normalize_diff_text_for_session_change(&fallback_new_text);
        let fallback_old_text = fallback_old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);

        if is_trustworthy_review_change_text(
            &change_type,
            fallback_old_text.as_deref(),
            &fallback_new_text,
        ) {
            self.upsert_review_file_change(path, change_type, fallback_old_text, fallback_new_text);
        } else if let Some(exact_edit) = exact_edit {
            self.upsert_review_file_change(
                path,
                change_type,
                Some(exact_edit.old_text.clone()),
                exact_edit.new_text.clone(),
            );
        }
    }

    fn tool_diff_baseline_text(
        &self,
        call_id: &str,
        normalized_path: &str,
        new_text: &str,
        batch_file_versions: &HashMap<String, String>,
    ) -> Option<String> {
        let normalized_new_text = normalize_diff_text_for_session_change(new_text);
        let candidate = |text: Option<String>| -> Option<String> {
            let text = normalize_diff_text_for_session_change(text?.as_str());
            (text != normalized_new_text).then_some(text)
        };

        candidate(batch_file_versions.get(normalized_path).cloned())
            .or_else(|| {
                candidate(
                    self.file_tracker
                        .get_baseline_text(call_id, normalized_path)
                        .map(str::to_string),
                )
            })
            .or_else(|| candidate(self.review_new_text_for_path(normalized_path)))
            .or_else(|| candidate(self.session_new_text_for_path(normalized_path)))
            .or_else(|| candidate(self.git_head_text_for_path(normalized_path)))
    }

    fn tool_diff_change_type(
        &self,
        call_id: &str,
        normalized_path: &str,
        old_text: Option<&str>,
    ) -> FileChangeType {
        if old_text.is_some() {
            return FileChangeType::Modified;
        }
        if call_id.starts_with("fs_write:")
            || self
                .file_tracker
                .was_missing_at_start(call_id, normalized_path)
                .unwrap_or(false)
        {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        }
    }

    fn review_new_text_for_path(&self, normalized_path: &str) -> Option<String> {
        self.ui
            .review_changes
            .iter()
            .find(|change| normalize_tracked_path(&change.path) == normalized_path)
            .map(|change| change.new_text.clone())
    }

    fn session_new_text_for_path(&self, normalized_path: &str) -> Option<String> {
        self.ui
            .session_changes
            .iter()
            .find(|change| normalize_tracked_path(&change.path) == normalized_path)
            .map(|change| change.new_text.clone())
    }

    fn git_head_text_for_path(&self, normalized_path: &str) -> Option<String> {
        GitService::head_text(&self.ui.workspace.root, normalized_path)
            .ok()
            .flatten()
            .map(|text| normalize_diff_text_for_session_change(&text))
    }

    fn next_seq(&mut self) -> i64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    fn persist_event(&mut self, event: &ClientEvent) {
        let session_id = self.ui.session.id.to_string();
        match event {
            ClientEvent::SessionStarted { session_id: acp_id } => {
                // Persist the ACP session ID for --resume on next startup
                let _ = self.store.update_acp_session_id(&session_id, acp_id);
                self.persist_current_codex_provider_if_needed();
            }
            ClientEvent::MessageChunk { .. } => {}
            ClientEvent::TurnFinished { .. } => {
                // Persist the final assistant message if not already persisted
                let msg_data = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::Assistant)
                    .map(|m| (m.id.to_string(), m.body.clone()));

                if let Some((id_str, body)) = msg_data {
                    let seq = self.next_seq();
                    if self
                        .store
                        .insert_message(&session_id, &id_str, "Assistant", &body, seq)
                        .is_err()
                    {
                        let _ = self.store.update_message_body(&id_str, &body);
                    }
                }
                let _ = self.store.update_session_status(&session_id, "Idle");
            }
            ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. } => {
                self.persist_session_model_mode();
            }
            ClientEvent::SessionTitleUpdated { title } => {
                let _ = self.store.update_session_title(&session_id, title);
            }
            ClientEvent::ToolStarted { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. } => {
                // Find the tool in the UI snapshot and persist its latest display state
                let tool_clone = self
                    .ui
                    .tools
                    .iter()
                    .find(|t| t.id.to_string() == *id || t.call_id == *id)
                    .cloned();

                if let Some(tool) = tool_clone {
                    let seq = self.next_seq();
                    let _ = self.store.insert_tool(&session_id, &tool, seq);
                }
            }
            _ => {}
        }
    }

    fn apply_event_with_dirty_tracking(&mut self, event: &ClientEvent) {
        let events = self.filter_inline_think_event(event.clone());
        for event in events {
            self.apply_event_with_dirty_tracking_unfiltered(&event);
        }
    }

    fn apply_event_with_dirty_tracking_unfiltered(&mut self, event: &ClientEvent) {
        if let ClientEvent::SessionTitleUpdated { title } = event
            && !self.should_apply_session_title_update(title)
        {
            return;
        }
        self.mark_event_tools_dirty(event);
        apply_event(&mut self.ui, event.clone());
        if let ClientEvent::SessionTitleUpdated { .. } = event {
            self.agent_title_received = true;
            self.provisional_prompt_title = None;
        }
        self.persist_event(event);
    }

    fn mark_tool_call_dirty(&mut self, call_id: &str) {
        self.dirty_tool_call_ids.insert(call_id.to_string());
    }

    fn mark_running_tools_dirty(&mut self) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    fn mark_running_child_tools_dirty(
        &mut self,
        parent_call_id: &str,
        except_call_id: Option<&str>,
    ) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| {
                tool.parent_call_id.as_deref() == Some(parent_call_id)
                    && except_call_id != Some(tool.call_id.as_str())
                    && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            })
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    fn mark_event_tools_dirty(&mut self, event: &ClientEvent) {
        match event {
            ClientEvent::ToolMessageChunk { id, .. }
            | ClientEvent::ToolPermissionRequest { id, .. }
            | ClientEvent::ToolPermissionResolved { id, .. }
            | ClientEvent::ToolProgress { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. }
            | ClientEvent::ToolDiff { id, .. } => {
                self.mark_tool_call_dirty(id);
            }
            ClientEvent::ToolStarted { id, parent_id, .. }
            | ClientEvent::ToolUpdated { id, parent_id, .. } => {
                self.mark_tool_call_dirty(id);
                if let Some(parent_id) = parent_id.as_deref() {
                    self.mark_running_child_tools_dirty(parent_id, Some(id));
                }
            }
            ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. } => {
                self.mark_running_tools_dirty();
            }
            ClientEvent::SessionStarted { .. }
            | ClientEvent::ThinkingActivity { .. }
            | ClientEvent::MessageChunk { .. }
            | ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::PromptCapabilitiesUpdated { .. }
            | ClientEvent::AvailableCommandsUpdated { .. }
            | ClientEvent::SessionTitleUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. }
            | ClientEvent::PlanUpdated { .. } => {}
        }
    }

    fn persist_session_model_mode(&self) {
        let _ = self.store.update_session_model_mode(
            &self.ui.session.id.to_string(),
            &self.ui.session.model,
            self.ui.session.mode.as_deref(),
        );
    }

    fn apply_event_and_restore_model(&mut self, event: ClientEvent) {
        let events = self.filter_inline_think_event(event);
        for event in events {
            if let ClientEvent::SessionTitleUpdated { title } = &event
                && !self.should_apply_session_title_update(title)
            {
                continue;
            }
            let should_restore_model = matches!(event, ClientEvent::SessionConfigUpdated { .. });
            self.mark_event_tools_dirty(&event);
            apply_event(&mut self.ui, event.clone());
            if let ClientEvent::SessionTitleUpdated { .. } = &event {
                self.agent_title_received = true;
                self.provisional_prompt_title = None;
            }
            if should_restore_model {
                self.restore_pending_model_selection();
            }
            self.persist_event(&event);
        }
    }

    fn should_apply_session_title_update(&self, title: &str) -> bool {
        let trimmed = title.trim();
        if trimmed.is_empty() || is_placeholder_session_title(trimmed) {
            return false;
        }
        if same_session_title(&self.ui.session.title, trimmed) {
            return false;
        }
        if self
            .provisional_prompt_title
            .as_deref()
            .is_some_and(|provisional| same_session_title(provisional, trimmed))
        {
            return false;
        }
        true
    }

    fn filter_inline_think_event(&mut self, event: ClientEvent) -> Vec<ClientEvent> {
        match event {
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content,
            } => self
                .inline_think_filter
                .filter_chunk(&content)
                .map(|content| {
                    vec![ClientEvent::MessageChunk {
                        role: MessageRole::Assistant,
                        content,
                    }]
                })
                .unwrap_or_default(),
            ClientEvent::TurnFinished { stop_reason } => {
                let mut events = Vec::new();
                if let Some(content) = self.inline_think_filter.flush() {
                    events.push(ClientEvent::MessageChunk {
                        role: MessageRole::Assistant,
                        content,
                    });
                }
                events.push(ClientEvent::TurnFinished { stop_reason });
                events
            }
            ClientEvent::Interrupted { reason } => {
                self.inline_think_filter.reset();
                vec![ClientEvent::Interrupted { reason }]
            }
            other => vec![other],
        }
    }

    fn restore_pending_model_selection(&mut self) {
        let Some(saved_model) = self.pending_model_restore.clone() else {
            return;
        };
        let Some(model_control) = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == workspace_model::SessionConfigCategory::Model)
        else {
            return;
        };

        if model_control.current_value_id == saved_model
            || model_control.current_value_label == saved_model
        {
            self.pending_model_restore = None;
            return;
        }

        let Some(choice) = model_control
            .choices
            .iter()
            .find(|choice| choice.id == saved_model || choice.label == saved_model)
            .cloned()
        else {
            self.pending_model_restore = None;
            return;
        };

        self.pending_model_restore = None;
        let Ok(events) = self.session.set_model(choice.id) else {
            return;
        };
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
    }

    /// Detect file writes from completed tool calls by examining tool summaries/titles.
    /// Apply verified file changes from the tracker to session state and tool diff previews.
    fn apply_tracker_changes(
        &mut self,
        call_id: &str,
        changes: Vec<crate::file_tracker::VerifiedFileChange>,
    ) -> bool {
        let mut changed = false;
        for change in changes {
            let normalized = normalize_tracked_path(&change.path);
            let existing_index = self
                .ui
                .session_changes
                .iter()
                .position(|c| normalize_tracked_path(&c.path) == normalized);
            let previous_session_new_text =
                existing_index.map(|index| self.ui.session_changes[index].new_text.clone());
            let effective_old_text = existing_index
                .and_then(|index| self.ui.session_changes[index].old_text.clone())
                .or_else(|| change.old_text.clone());
            let target_text = if change.change_type == FileChangeType::Deleted {
                None
            } else {
                Some(change.new_text.as_str())
            };
            let canonical = canonical_text_diff(
                &change.change_type,
                effective_old_text.as_deref(),
                target_text,
                (change.quality != DiffQuality::Exact).then_some(change.quality.clone()),
            );
            let exact_edit = self.exact_edit_text_for_tool(call_id, &normalized);
            let exact_edit_hunks = exact_edit.as_ref().map(|edit| edit.hunks.clone());
            let existing_tool_hunks = self.existing_tool_diff_hunks(call_id, &normalized);
            let tool_hunks = tool_hunks_for_tracker_update(
                change.skipped_diff,
                exact_edit_hunks,
                existing_tool_hunks,
                previous_session_new_text.as_deref(),
                change.old_text.as_deref(),
                &change.new_text,
                &canonical.hunks,
            );

            if canonical.quality == DiffQuality::Exact {
                if canonical.added_lines == 0 && canonical.removed_lines == 0 {
                    if let Some(index) = existing_index {
                        self.ui.session_changes.remove(index);
                        changed = true;
                    }
                    self.upsert_review_file_change_for_tool(
                        &change.path,
                        change.change_type.clone(),
                        exact_edit.as_ref(),
                        change
                            .old_text
                            .clone()
                            .or(previous_session_new_text.clone()),
                        change.new_text.clone(),
                    );
                    self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
                    continue;
                }

                let added = canonical.added_lines;
                let removed = canonical.removed_lines;

                if added == 0 && removed == 0 {
                    continue;
                }

                if let Some(index) = existing_index {
                    let existing = &mut self.ui.session_changes[index];
                    if existing.old_text.is_none() {
                        existing.old_text = canonical.old_text.clone();
                    }
                    existing.new_text = canonical.new_text.clone().unwrap_or_default();
                    existing.change_type = change.change_type.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                } else {
                    self.ui.session_changes.push(SessionFileChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        old_text: canonical.old_text.clone(),
                        new_text: canonical.new_text.clone().unwrap_or_default(),
                        added_lines: added,
                        removed_lines: removed,
                        timestamp: current_timestamp(),
                    });
                }
                self.upsert_review_file_change_for_tool(
                    &change.path,
                    change.change_type.clone(),
                    exact_edit.as_ref(),
                    change
                        .old_text
                        .clone()
                        .or(previous_session_new_text.clone())
                        .or(effective_old_text),
                    change.new_text.clone(),
                );
                changed = true;
            }

            // Attach only this tool's diff preview, not the cumulative session diff.
            self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
        }
        changed
    }

    fn exact_edit_text_for_tool(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        let tool = self.ui.tools.iter().find(|tool| tool.call_id == call_id)?;
        let input = tool.raw_input.as_deref()?;
        let json = serde_json::from_str::<serde_json::Value>(input).ok()?;
        let before = edit_input_before_text(&json)?;
        let after = edit_input_after_text(&json)?;
        let input_path = json
            .get("path")
            .or_else(|| json.get("file_path"))
            .or_else(|| json.get("filePath"))
            .and_then(|value| value.as_str())?;
        if normalize_path_for_storage(input_path, &self.ui.workspace.root) != normalized_path
            && normalize_tracked_path(input_path) != normalized_path
        {
            return None;
        }
        if looks_like_fragment_to_full_file_text(before, after) {
            return None;
        }
        let hunks = diff_to_hunks(Some(before), after);
        (!hunks.is_empty()).then_some(ExactEditText {
            old_text: before.to_string(),
            new_text: after.to_string(),
            hunks,
        })
    }

    fn existing_tool_diff_hunks(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<Vec<DiffHunk>> {
        self.ui
            .tools
            .iter()
            .find(|tool| tool.call_id == call_id)?
            .diff_previews
            .iter()
            .find(|preview| {
                normalize_tracked_path(&preview.path.display().to_string()) == normalized_path
            })
            .map(|preview| preview.hunks.clone())
            .filter(|hunks| !hunks.is_empty() && !looks_like_whole_file_addition_hunks(hunks))
    }

    fn attach_tool_diff_preview(
        &mut self,
        call_id: &str,
        path: &str,
        normalized_path: &str,
        hunks: Vec<DiffHunk>,
    ) {
        if hunks.is_empty() {
            return;
        }
        self.mark_tool_call_dirty(call_id);
        let Some(tool) = self.ui.tools.iter_mut().find(|t| t.call_id == call_id) else {
            return;
        };

        let path_buf = PathBuf::from(path);
        if !tool
            .diff_paths
            .iter()
            .any(|p| normalize_tracked_path(&p.display().to_string()) == normalized_path)
        {
            tool.diff_paths.push(path_buf.clone());
        }
        if let Some(preview) = tool
            .diff_previews
            .iter_mut()
            .find(|p| normalize_tracked_path(&p.path.display().to_string()) == normalized_path)
        {
            preview.path = path_buf;
            preview.hunks = hunks;
        } else {
            tool.diff_previews.push(ToolDiffPreview {
                path: path_buf,
                hunks,
            });
        }
    }

    /// CodeBuddy agent uses terminal commands (cat > file << 'EOF') to write files,
    /// so we can't rely on ToolDiff events from the ACP protocol. Instead, we check
    /// completed tools for edit-related patterns and read the current file content.
    fn detect_file_writes_from_tools(&mut self, completed_tool_ids: &[String]) -> bool {
        // Normalize path for comparison: forward slashes, lowercase drive letter on Windows
        fn normalize_path(p: &str) -> String {
            normalize_tracked_path(p)
        }

        let workspace_root = self.ui.workspace.root.clone();
        let mut changed = false;

        // Collect normalized paths already tracked in session_changes
        let tracked_paths: HashSet<String> = self
            .ui
            .session_changes
            .iter()
            .map(|c| normalize_path(&c.path))
            .collect();

        let completed_tool_ids: HashSet<&str> =
            completed_tool_ids.iter().map(String::as_str).collect();

        // Look only at tools that completed in this poll batch. Scanning all historical
        // tools every 220ms makes long CodeBuddy sessions burn the desktop process.
        let mut write_paths: Vec<(String, String)> = Vec::new();
        for tool in self.ui.tools.iter().filter(|t| {
            t.status == ToolStatus::Succeeded && completed_tool_ids.contains(t.call_id.as_str())
        }) {
            let mut add_path = |path: String| {
                if !tracked_paths.contains(&normalize_path(&path)) {
                    write_paths.push((tool.call_id.clone(), path));
                }
            };

            // Check diff_paths first (set by ToolDiff events from ACP WriteTextFileRequest).
            // Only treat it as a real write when the preview has actual changed lines;
            // a path-only or context-only preview is often just a no-op edit notification.
            if let Some(preview) = tool.diff_previews.iter().find(|preview| {
                preview.hunks.iter().any(|hunk| {
                    hunk.lines.iter().any(|line| {
                        matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed)
                    })
                })
            }) {
                let path = preview.path.display().to_string();
                add_path(path);
            }

            // Check summary for "Editing <path>" pattern
            if tool.summary.starts_with("Editing ") {
                let path = tool.summary.trim_start_matches("Editing ").to_string();
                add_path(path);
            }

            // Check raw_input JSON for file_path/filePath/path fields in edit/write tools
            if is_file_write_tool_identity(&tool.kind, &tool.name) {
                if let Some(ref input) = tool.raw_input {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
                        let file_path = json
                            .get("file_path")
                            .or_else(|| json.get("filePath"))
                            .or_else(|| json.get("path"))
                            .and_then(|v| v.as_str());
                        if let Some(path) = file_path {
                            add_path(path.to_string());
                        }
                    }
                }
            }

            // Shell tools can write files via command text (Set-Content, Out-File,
            // redirects, etc.) without emitting ACP ToolDiff events.
            for path in tool_event_hint_paths(tool.raw_input.as_deref()) {
                add_path(path);
            }
        }
        write_paths.sort();
        write_paths.dedup();

        // For each detected file write, read current content and update only
        // already-tracked changes. Creating a new change without a baseline makes
        // the UI render the whole file as added, so the file tracker must be the
        // source of new session_changes.
        for (call_id, path) in write_paths {
            let normalized = normalize_path_for_storage(&path, &workspace_root);
            let abs_path = workspace_root.join(&normalized);
            if let Ok(new_text) = std::fs::read_to_string(&abs_path)
                && let Some(index) = self
                    .ui
                    .session_changes
                    .iter()
                    .position(|c| normalize_path(&c.path) == normalized)
            {
                let exact_edit = self.exact_edit_text_for_tool(&call_id, &normalized);
                let old_text = self.ui.session_changes[index].old_text.clone();
                let previous_session_new_text = self.ui.session_changes[index].new_text.clone();
                let tool_hunks = exact_edit
                    .as_ref()
                    .map(|edit| edit.hunks.clone())
                    .unwrap_or_else(|| {
                        tool_diff_hunks_for_detected_write(
                            Some(&previous_session_new_text),
                            None,
                            &new_text,
                        )
                    });
                if old_text.as_deref().unwrap_or_default() == new_text {
                    self.ui.session_changes.remove(index);
                    self.upsert_review_file_change_for_tool(
                        &normalized,
                        FileChangeType::Modified,
                        exact_edit.as_ref(),
                        Some(previous_session_new_text.clone()),
                        new_text.clone(),
                    );
                    changed = true;
                    self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
                    continue;
                }

                let session_hunks = diff_to_hunks(old_text.as_deref(), &new_text);
                let added = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Added)
                    .count();
                let removed = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Removed)
                    .count();
                if added == 0 && removed == 0 {
                    continue;
                }

                let review_change_type = {
                    let existing = &mut self.ui.session_changes[index];
                    existing.new_text = new_text.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                    existing.change_type.clone()
                };
                self.upsert_review_file_change_for_tool(
                    &normalized,
                    review_change_type,
                    exact_edit.as_ref(),
                    Some(previous_session_new_text),
                    new_text,
                );
                changed = true;

                self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
            } else if let Some(change) = self.git_verified_file_change(&normalized) {
                changed |= self.apply_tracker_changes(&call_id, vec![change]);
            }
        }

        changed
    }

    fn git_verified_file_change(
        &self,
        normalized_path: &str,
    ) -> Option<crate::file_tracker::VerifiedFileChange> {
        let record = GitService::file_diff_auto(&self.ui.workspace.root, normalized_path)
            .ok()
            .flatten()?;
        let new_text = if record.change_type == FileChangeType::Deleted {
            String::new()
        } else {
            record.new_text.clone()?
        };
        Some(crate::file_tracker::VerifiedFileChange {
            path: record.path,
            change_type: record.change_type,
            old_text: record.old_text,
            new_text,
            skipped_diff: record.quality != DiffQuality::Exact,
            quality: record.quality,
        })
    }
}

fn tool_diff_hunks_for_tracker_change(
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

fn tool_hunks_for_tracker_update(
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

fn looks_like_fragment_to_full_file_text(old_text: &str, new_text: &str) -> bool {
    let old_lines = old_text.lines().count();
    let new_lines = new_text.lines().count();
    old_lines > 0 && new_lines >= 100 && old_lines * 4 < new_lines
}

fn looks_like_whole_file_addition_hunks(hunks: &[DiffHunk]) -> bool {
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

fn tool_diff_hunks_for_detected_write(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    tool_diff_hunks(previous_session_new_text, tool_old_text, tool_new_text)
        .or_else_non_empty(|| tool_diff_hunks(None, tool_old_text, tool_new_text))
}

fn expand_tool_diff_fragment_from_disk(
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

fn is_file_write_tool_identity(kind: &str, name: &str) -> bool {
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

/// Extract a concise session title from the user's first prompt.
/// Takes the first line, strips common prefixes, and truncates to 60 chars.
fn extract_title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();

    // Strip common conversational prefixes
    let stripped = first_line
        .strip_prefix("Please ")
        .or_else(|| first_line.strip_prefix("please "))
        .or_else(|| first_line.strip_prefix("Help me "))
        .or_else(|| first_line.strip_prefix("help me "))
        .or_else(|| first_line.strip_prefix("Can you "))
        .or_else(|| first_line.strip_prefix("can you "))
        .or_else(|| first_line.strip_prefix("I want to "))
        .or_else(|| first_line.strip_prefix("I need to "))
        .unwrap_or(first_line)
        .trim();

    let text = if stripped.is_empty() {
        first_line
    } else {
        stripped
    };

    if text.chars().count() <= 60 {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    }
}

fn same_session_title(left: &str, right: &str) -> bool {
    left.trim() == right.trim()
}

fn is_placeholder_session_title(title: &str) -> bool {
    matches!(title.trim(), "" | "新会话" | "New Session" | "新 ACP 会话")
}

#[cfg(test)]
mod tests {
    use super::{
        Application, InlineThinkFilter, canonical_text_diff, current_timestamp,
        edit_input_after_text, edit_input_before_text, expand_tool_diff_fragment_from_disk,
        is_file_write_tool_identity, is_placeholder_session_title,
        is_trustworthy_review_change_text, looks_like_fragment_to_full_file_text,
        looks_like_whole_file_addition_hunks, sanitize_session_file_changes, tool_diff_hunks,
        tool_diff_hunks_for_tracker_change, tool_event_hint_paths, tool_hunks_for_tracker_update,
        turn_finished_notice,
    };
    use acp_core::{ClientEvent, diff_to_hunks};
    use std::{collections::HashMap, fs};
    use workspace_model::{
        ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind,
        DiffQuality, FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest,
        ListChangeSetsRequest, MessageRole, SessionFileChange, TimelineItem, ToolInvocation,
        ToolStatus, TurnFileChanges,
    };

    #[test]
    fn inline_think_filter_strips_complete_blocks_from_visible_text() {
        let mut filter = InlineThinkFilter::default();

        assert_eq!(
            filter.filter_chunk("好的，<think>这里是推理</think>现在开始。"),
            Some("好的，现在开始。".into())
        );
        assert_eq!(filter.flush(), None);
    }

    #[test]
    fn inline_think_filter_strips_blocks_split_across_chunks() {
        let mut filter = InlineThinkFilter::default();

        assert_eq!(filter.filter_chunk("好<thi"), Some("好".into()));
        assert_eq!(filter.filter_chunk("nk>隐藏"), None);
        assert_eq!(filter.filter_chunk("推理</th"), None);
        assert_eq!(filter.filter_chunk("ink>，正文"), Some("，正文".into()));
        assert_eq!(filter.flush(), None);
    }

    #[test]
    fn inline_think_filter_preserves_literal_partial_tag_text_on_flush() {
        let mut filter = InlineThinkFilter::default();

        assert_eq!(
            filter.filter_chunk("普通文本 <thi"),
            Some("普通文本 ".into())
        );
        assert_eq!(filter.flush(), Some("<thi".into()));
    }

    #[test]
    fn placeholder_session_titles_are_not_meaningful_agent_titles() {
        assert!(is_placeholder_session_title("新会话"));
        assert!(is_placeholder_session_title("New Session"));
        assert!(!is_placeholder_session_title("修复登录流程"));
    }

    #[test]
    fn stale_prompt_title_sync_does_not_block_turn_end_refinement() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);

        app.needs_title = true;
        app.agent_title_received = false;
        app.ui.session.title = "修复登录".into();
        app.provisional_prompt_title = Some("修复登录".into());
        app.ui.messages.clear();
        app.ui.timeline.clear();

        app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
            title: "修复登录".into(),
        });

        assert_eq!(app.ui.session.title, "修复登录");
        assert!(!app.agent_title_received);

        app.ui.messages.push(ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::Assistant,
            body: "好的，我来修复登录流程。".into(),
            created_at: current_timestamp(),
        });

        app.refine_session_title();

        assert_eq!(app.ui.session.title, "修复登录流程。");
        app.session.shutdown();
    }

    #[test]
    fn tool_diff_uses_previous_session_new_text_for_repeated_file_edits() {
        let hunks = tool_diff_hunks(Some("one\ntwo\n"), Some("one\n"), "one\nthree\n");
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();

        assert_eq!(added, vec!["three"]);
        assert_eq!(removed, vec!["two"]);
    }

    #[test]
    fn tracker_tool_diff_prefers_tool_start_baseline_over_session_new_text() {
        let hunks = tool_diff_hunks_for_tracker_change(
            Some("one\ntwo\n"),
            Some("one\ntwo\n"),
            "one\nthree\n",
        );
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();

        assert_eq!(added, vec!["three"]);
        assert_eq!(removed, vec!["two"]);
    }

    #[test]
    fn tracker_tool_diff_preserves_existing_acp_preview() {
        let existing = vec![DiffHunk {
            heading: "@@ -1,1 +1,1 @@".into(),
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                content: "'react-refresh/only-export-components': [".into(),
            }],
        }];
        let tracker_full_file = vec![DiffHunk {
            heading: "@@ -0,0 +1,27 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "module.exports = {".into(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "  root: true,".into(),
                },
            ],
        }];

        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            Some(existing.clone()),
            None,
            None,
            "module.exports = {\n  root: true,\n",
            &tracker_full_file,
        );

        assert_eq!(hunks, existing);
    }

    #[test]
    fn tracker_tool_diff_prefers_codebuddy_old_new_string_exact_hunks() {
        let input = serde_json::json!({
            "file_path": "D:/work/InfiniteCanvasOL/smokeTest/tests/app-smoke.spec.ts",
            "old_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}",
            "new_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await page.waitForFunction(() => Boolean(document.querySelector('[data-testid=\"prompt-shell\"]')), undefined, { timeout: 10_000 });\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}"
        });
        let exact = diff_to_hunks(
            edit_input_before_text(&input),
            edit_input_after_text(&input).unwrap(),
        );
        let tracker_full_file = vec![DiffHunk {
            heading: "@@ -0,0 +1,847 @@".into(),
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                content: "import { test, expect, Page, TestInfo } from '@playwright/test';".into(),
            }],
        }];

        let hunks = tool_hunks_for_tracker_update(
            false,
            Some(exact.clone()),
            None,
            None,
            None,
            "full file content",
            &tracker_full_file,
        );

        assert_eq!(hunks, exact);
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        assert_eq!(added, 1);
    }

    #[test]
    fn tracker_tool_diff_rejects_fragment_to_full_file_existing_preview() {
        let bad_existing = vec![DiffHunk {
            heading: "@@ -1,3 +1,901 @@".into(),
            lines: (1..=901)
                .map(|line| DiffLine {
                    kind: DiffLineKind::Added,
                    content: format!("line {line}"),
                })
                .collect(),
        }];
        let full_old = (1..=901)
            .map(|line| {
                if line == 42 {
                    "old target".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let full_new = full_old.replace("old target", "new target\nextra target");

        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            Some(bad_existing),
            None,
            Some(&full_old),
            &full_new,
            &[],
        );

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
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn fragment_to_full_file_text_is_not_trusted_as_exact_edit() {
        let old_fragment = "function target() {\n  return 1;\n}\n";
        let new_whole_file = (1..=901)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(looks_like_fragment_to_full_file_text(
            old_fragment,
            &new_whole_file
        ));
    }

    #[test]
    fn canonical_diff_counts_from_same_base_and_target_pair() {
        let diff = canonical_text_diff(
            &FileChangeType::Modified,
            Some("alpha\r\nold\r\nomega\r\n"),
            Some("alpha\nnew\nextra\nomega\n"),
            None,
        );

        assert_eq!(diff.quality, DiffQuality::Exact);
        assert_eq!(diff.old_text.as_deref(), Some("alpha\nold\nomega\n"));
        assert_eq!(diff.new_text.as_deref(), Some("alpha\nnew\nextra\nomega\n"));
        assert_eq!(diff.added_lines, 2);
        assert_eq!(diff.removed_lines, 1);
        let counted_added = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        let counted_removed = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        assert_eq!(diff.added_lines, counted_added);
        assert_eq!(diff.removed_lines, counted_removed);
    }

    #[test]
    fn canonical_diff_records_quality_for_unavailable_inputs() {
        let missing = canonical_text_diff(
            &FileChangeType::Modified,
            None,
            Some("new whole file\n"),
            None,
        );
        assert_eq!(missing.quality, DiffQuality::MissingBaseline);
        assert_eq!(missing.added_lines, 0);
        assert!(missing.hunks.is_empty());

        let fragment = canonical_text_diff(
            &FileChangeType::Modified,
            Some("tiny fragment\n"),
            Some(
                &(1..=300)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            None,
        );
        assert_eq!(fragment.quality, DiffQuality::FragmentRejected);
        assert_eq!(fragment.added_lines, 0);

        let binary = canonical_text_diff(
            &FileChangeType::Modified,
            Some("old\n"),
            None,
            Some(DiffQuality::BinarySkipped),
        );
        assert_eq!(binary.quality, DiffQuality::BinarySkipped);
        assert!(binary.hunks.is_empty());
    }

    #[test]
    fn file_record_keeps_fragment_rejection_instead_of_full_file_stats() {
        let new_whole_file = (1..=400)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let change = SessionFileChange {
            path: "src/settings.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("const model = \"gpt-5.5\";\n".into()),
            new_text: new_whole_file,
            added_lines: 400,
            removed_lines: 1,
            timestamp: "1".into(),
        };

        let record = Application::file_record_from_session_change("change-set-1", &change)
            .expect("fragment rejection is still a reviewable record");

        assert_eq!(record.quality, DiffQuality::FragmentRejected);
        assert_eq!(record.added_lines, 0);
        assert_eq!(record.removed_lines, 0);
        assert_eq!(record.path, "src/settings.rs");
    }

    #[test]
    fn canonical_deleted_file_diff_keeps_target_absent_but_counts_removed_lines() {
        let diff = canonical_text_diff(
            &FileChangeType::Deleted,
            Some("one\ntwo\nthree\n"),
            None,
            None,
        );

        assert_eq!(diff.quality, DiffQuality::Exact);
        assert_eq!(diff.old_text.as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(diff.new_text, None);
        assert_eq!(diff.added_lines, 0);
        assert_eq!(diff.removed_lines, 3);
    }

    #[test]
    fn review_change_rejects_fragment_old_text_against_full_file_text() {
        let old_fragment = "const model = \"gpt-5.5\";\n";
        let new_whole_file = (1..=1609)
            .map(|line| {
                if line == 700 {
                    "const model = \"gpt-5.5\";".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!is_trustworthy_review_change_text(
            &FileChangeType::Modified,
            Some(old_fragment),
            &new_whole_file,
        ));
    }

    #[test]
    fn tool_diff_fragment_expands_to_full_file_snapshot_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("render_node_registry.py");
        let target = "\
import ipaddress

def normalize_reported_render_ip(value: str) -> str:
    parsed = ipaddress.ip_address(value.strip())
    parsed_ip = str(parsed)
    if not parsed.is_private or parsed_ip.startswith(\"9.134.\"):
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return parsed_ip

def other():
    return None
";
        fs::write(&path, target).unwrap();

        let (base, expanded_target) = expand_tool_diff_fragment_from_disk(
            &path,
            Some(
                "\
    if not parsed.is_private:
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return str(parsed)
",
            ),
            "\
    parsed_ip = str(parsed)
    if not parsed.is_private or parsed_ip.startswith(\"9.134.\"):
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return parsed_ip
",
        )
        .expect("fragment should expand against target file");

        assert_eq!(expanded_target, target);
        assert!(base.contains("return str(parsed)"));
        assert!(base.starts_with("import ipaddress\n\n"));

        let hunks = diff_to_hunks(Some(&base), &expanded_target);
        let first_changed_line = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .position(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
            .expect("diff should contain a changed line");
        assert!(
            first_changed_line > 1,
            "expanded full-file diff should keep leading file context before the edit",
        );
    }

    #[test]
    fn sanitizer_drops_persisted_fragment_to_full_file_change() {
        let mut changes = vec![SessionFileChange {
            path: "crates/app-core/src/settings.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("old line\n".into()),
            new_text: (1..=1609)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            added_lines: 1609,
            removed_lines: 1,
            timestamp: "1".into(),
        }];

        assert!(sanitize_session_file_changes(&mut changes));
        assert!(changes.is_empty());
    }

    #[test]
    fn whole_file_addition_hunks_are_not_preserved_as_existing_preview() {
        let hunks = vec![DiffHunk {
            heading: "@@ -1,3 +1,901 @@".into(),
            lines: (1..=901)
                .map(|line| DiffLine {
                    kind: DiffLineKind::Added,
                    content: format!("line {line}"),
                })
                .collect(),
        }];

        assert!(looks_like_whole_file_addition_hunks(&hunks));
    }

    #[test]
    fn tracker_tool_diff_without_any_baseline_does_not_render_full_file() {
        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            None,
            None,
            None,
            "module.exports = {\n  root: true,\n",
            &[],
        );

        assert!(hunks.is_empty());
    }

    #[test]
    fn write_tool_detection_does_not_match_editor_paths() {
        assert!(!is_file_write_tool_identity(
            "read",
            "docs\\editor-subsystem-design.md"
        ));
        assert!(!is_file_write_tool_identity(
            "read",
            "D:/work/kodex/docs/editor-subsystem-design.md"
        ));
        assert!(is_file_write_tool_identity("edit", "docs/architecture.md"));
        assert!(is_file_write_tool_identity("tool", "mcp__codebuddy__write"));
    }

    #[test]
    fn codex_powershell_command_array_yields_written_file_hint() {
        let raw_input = serde_json::json!({
            "call_id": "call_1",
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                "if (-not (Test-Path \"docs\")) { New-Item -ItemType Directory -Path \"docs\" -Force | Out-Null }; $guideContent = @\"\n# Guide\n\nSet-Content -Path \"fake.md\"\n\"@; Set-Content -Path \"docs/windows-guide.md\" -Value $guideContent -Encoding UTF8"
            ],
        })
        .to_string();

        let paths = tool_event_hint_paths(Some(&raw_input));

        assert_eq!(paths, vec!["docs/windows-guide.md"]);
    }

    #[test]
    fn powershell_positional_set_content_yields_written_file_hint() {
        let raw_input = serde_json::json!({
            "call_id": "call_1",
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                "$lines = Get-Content \"D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts\"; $lines[0] = 'after'; Set-Content \"D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts\" $lines"
            ],
        })
        .to_string();

        let paths = tool_event_hint_paths(Some(&raw_input));

        assert_eq!(
            paths,
            vec!["D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts"]
        );
    }

    #[test]
    fn completed_shell_write_hint_enters_review_via_git_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let repo = init_test_git_repo(dir.path());
        let relative_path = "smokeTest/tests/app-smoke.spec.ts";
        let file_path = dir.path().join(relative_path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, "before\n").unwrap();
        commit_paths(&repo, &[".gitignore", relative_path]);
        fs::write(&file_path, "after\n").unwrap();

        let raw_input = serde_json::json!({
            "call_id": "call-shell",
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                format!(
                    "$lines = Get-Content \"{}\"; $lines[0] = 'after'; Set-Content \"{}\" $lines",
                    file_path.display(),
                    file_path.display(),
                ),
            ],
            "cwd": dir.path().display().to_string(),
        })
        .to_string();
        app.ui.tools.push(ToolInvocation {
            id: uuid::Uuid::new_v4(),
            call_id: "call-shell".into(),
            parent_call_id: None,
            name: "Shell".into(),
            kind: "Shell".into(),
            summary: "Shell".into(),
            status: ToolStatus::Succeeded,
            is_subagent: false,
            detail_text: String::new(),
            logs: Vec::new(),
            diff_paths: Vec::new(),
            diff_previews: Vec::new(),
            raw_input: Some(raw_input),
            raw_output: None,
            terminal_output: None,
            error: None,
            permission_options: Vec::new(),
            permission_decision: None,
        });

        assert!(app.detect_file_writes_from_tools(&["call-shell".into()]));

        assert_eq!(app.ui.review_changes.len(), 1);
        assert_eq!(app.ui.review_changes[0].path, relative_path);
        assert_eq!(
            app.ui.review_changes[0].old_text.as_deref(),
            Some("before\n")
        );
        assert_eq!(app.ui.review_changes[0].new_text, "after\n");
        assert_eq!(app.ui.session_changes.len(), 1);
        let tool = app
            .ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "call-shell")
            .unwrap();
        assert!(
            tool.diff_previews
                .iter()
                .any(|preview| !preview.hunks.is_empty())
        );
    }

    #[test]
    fn current_turn_without_file_changes_does_not_inherit_recent_review_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_id = uuid::Uuid::new_v4();
        let assistant_id = uuid::Uuid::new_v4();
        let recent_change = SessionFileChange {
            path: "backend/scripts/run_dev.ps1".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("old\n".into()),
            new_text: "new\n".into(),
            added_lines: 1,
            removed_lines: 1,
            timestamp: "2026-05-13T00:00:00Z".into(),
        };

        app.ui.messages.clear();
        app.ui.timeline.clear();
        app.ui.messages.push(ChatMessage {
            id: user_id,
            role: MessageRole::User,
            body: "How do I start the frontend?".into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        });
        app.ui.messages.push(ChatMessage {
            id: assistant_id,
            role: MessageRole::Assistant,
            body: "Use npm run dev.".into(),
            created_at: "2026-05-13T00:00:01Z".into(),
        });
        app.ui.timeline.push(TimelineItem::Message(user_id));
        app.ui.timeline.push(TimelineItem::Message(assistant_id));
        app.current_turn_user_message_id = Some(user_id);
        app.review_changes_started = false;
        app.ui.review_changes = vec![recent_change.clone()];
        app.ui.turn_changes = vec![TurnFileChanges {
            message_id: assistant_id,
            changes: vec![recent_change],
        }];

        assert!(app.persist_current_turn_file_changes());

        assert_eq!(app.ui.review_changes.len(), 1);
        assert!(app.ui.turn_changes.is_empty());
        assert!(
            app.store
                .load_turn_file_changes(&app.ui.session.id.to_string())
                .unwrap()
                .is_empty()
        );
        assert!(
            app.store
                .list_change_sets(
                    Some(&app.ui.session.id.to_string()),
                    Some(ChangeSetSource::AgentTurn)
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn current_turn_changes_preserve_first_base_and_final_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_id = uuid::Uuid::new_v4();
        let assistant_id = uuid::Uuid::new_v4();

        app.ui.messages.clear();
        app.ui.timeline.clear();
        app.ui.messages.push(ChatMessage {
            id: user_id,
            role: MessageRole::User,
            body: "update file".into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        });
        app.ui.messages.push(ChatMessage {
            id: assistant_id,
            role: MessageRole::Assistant,
            body: "done".into(),
            created_at: "2026-05-13T00:00:01Z".into(),
        });
        app.ui.timeline.push(TimelineItem::Message(user_id));
        app.ui.timeline.push(TimelineItem::Message(assistant_id));
        app.current_turn_user_message_id = Some(user_id);

        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("before\n".into()),
            "middle\n".into(),
        );
        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("middle\n".into()),
            "after\n".into(),
        );

        let pending = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, ChangeSetStatus::Pending);
        assert_eq!(pending[0].message_id, None);
        assert_eq!(pending[0].added_lines, 1);
        assert_eq!(pending[0].removed_lines, 1);

        assert!(app.persist_current_turn_file_changes());

        let entry = app
            .ui
            .turn_changes
            .iter()
            .find(|entry| entry.message_id == assistant_id)
            .expect("turn changes should be attached to assistant message");
        assert_eq!(entry.changes.len(), 1);
        assert_eq!(entry.changes[0].old_text.as_deref(), Some("before\n"));
        assert_eq!(entry.changes[0].new_text, "after\n");
        assert_eq!(entry.changes[0].added_lines, 1);
        assert_eq!(entry.changes[0].removed_lines, 1);

        let completed = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].status, ChangeSetStatus::Complete);
        assert_eq!(completed[0].message_id, Some(assistant_id));
        let stored_diff = app
            .store
            .load_change_set_file_diff(&completed[0].id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(stored_diff.old_text.as_deref(), Some("before\n"));
        assert_eq!(stored_diff.new_text.as_deref(), Some("after\n"));

        let conversation = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentConversation),
            )
            .unwrap();
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].added_lines, 1);
        assert_eq!(conversation[0].removed_lines, 1);
    }

    #[test]
    fn current_turn_revert_removes_pending_change_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_id = uuid::Uuid::new_v4();
        app.current_turn_user_message_id = Some(user_id);

        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("A\n".into()),
            "B\n".into(),
        );
        assert_eq!(
            app.store
                .list_change_sets(
                    Some(&app.ui.session.id.to_string()),
                    Some(ChangeSetSource::AgentTurn)
                )
                .unwrap()
                .len(),
            1
        );

        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("B\n".into()),
            "A\n".into(),
        );

        assert!(
            app.store
                .list_change_sets(
                    Some(&app.ui.session.id.to_string()),
                    Some(ChangeSetSource::AgentTurn)
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn manual_editor_saves_use_manual_change_set_and_preserve_first_base() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save("src/main.rs", Some("A\n".into()), "B\n".into());
        app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "C\n".into());

        assert!(app.ui.session_changes.is_empty());
        assert!(app.ui.review_changes.is_empty());
        let manual_sets = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::ManualEdit),
            )
            .unwrap();
        assert_eq!(manual_sets.len(), 1);
        assert_eq!(manual_sets[0].added_lines, 1);
        assert_eq!(manual_sets[0].removed_lines, 1);
        let diff = app
            .store
            .load_change_set_file_diff(&manual_sets[0].id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(diff.old_text.as_deref(), Some("A\n"));
        assert_eq!(diff.new_text.as_deref(), Some("C\n"));
    }

    #[test]
    fn manual_editor_revert_removes_manual_change_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save("src/main.rs", Some("A\n".into()), "B\n".into());
        app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "A\n".into());

        assert!(
            app.store
                .list_change_sets(
                    Some(&app.ui.session.id.to_string()),
                    Some(ChangeSetSource::ManualEdit)
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn manual_and_agent_changes_for_same_path_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_id = uuid::Uuid::new_v4();
        let assistant_id = uuid::Uuid::new_v4();
        app.ui.messages.clear();
        app.ui.timeline.clear();
        app.ui.messages.push(ChatMessage {
            id: user_id,
            role: MessageRole::User,
            body: "agent edit".into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        });
        app.ui.messages.push(ChatMessage {
            id: assistant_id,
            role: MessageRole::Assistant,
            body: "done".into(),
            created_at: "2026-05-13T00:00:01Z".into(),
        });
        app.ui.timeline.push(TimelineItem::Message(user_id));
        app.ui.timeline.push(TimelineItem::Message(assistant_id));
        app.current_turn_user_message_id = Some(user_id);

        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("A\n".into()),
            "B\n".into(),
        );
        assert!(app.persist_current_turn_file_changes());
        app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "C\n".into());

        let conversation = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentConversation),
            )
            .unwrap();
        let manual = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::ManualEdit),
            )
            .unwrap();
        assert_eq!(conversation.len(), 1);
        assert_eq!(manual.len(), 1);
        let conversation_diff = app
            .store
            .load_change_set_file_diff(&conversation[0].id, "src/main.rs")
            .unwrap()
            .unwrap();
        let manual_diff = app
            .store
            .load_change_set_file_diff(&manual[0].id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(conversation_diff.old_text.as_deref(), Some("A\n"));
        assert_eq!(conversation_diff.new_text.as_deref(), Some("B\n"));
        assert_eq!(manual_diff.old_text.as_deref(), Some("B\n"));
        assert_eq!(manual_diff.new_text.as_deref(), Some("C\n"));
    }

    #[test]
    fn interrupted_turn_change_set_remains_user_owned_when_no_assistant_message_exists() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_id = uuid::Uuid::new_v4();
        app.ui.messages.clear();
        app.ui.timeline.clear();
        app.ui.messages.push(ChatMessage {
            id: user_id,
            role: MessageRole::User,
            body: "change then stop".into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        });
        app.ui.timeline.push(TimelineItem::Message(user_id));
        app.current_turn_user_message_id = Some(user_id);

        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("before\n".into()),
            "after\n".into(),
        );
        assert!(!app.persist_current_turn_file_changes());

        let pending = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, ChangeSetStatus::Pending);
        assert_eq!(pending[0].message_id, None);
        assert_eq!(
            pending[0].owner_key.as_deref(),
            Some(format!("user-message:{user_id}").as_str())
        );
    }

    #[test]
    fn historical_agent_turn_change_sets_keep_their_own_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let first_user = uuid::Uuid::new_v4();
        let first_assistant = uuid::Uuid::new_v4();
        let second_user = uuid::Uuid::new_v4();
        let second_assistant = uuid::Uuid::new_v4();

        app.ui.messages.clear();
        app.ui.timeline.clear();
        for (id, role, body) in [
            (first_user, MessageRole::User, "first"),
            (first_assistant, MessageRole::Assistant, "first done"),
        ] {
            app.ui.messages.push(ChatMessage {
                id,
                role,
                body: body.into(),
                created_at: "2026-05-13T00:00:00Z".into(),
            });
            app.ui.timeline.push(TimelineItem::Message(id));
        }

        app.current_turn_user_message_id = Some(first_user);
        app.review_changes_started = false;
        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("A\n".into()),
            "B\n".into(),
        );
        assert!(app.persist_current_turn_file_changes());

        for (id, role, body) in [
            (second_user, MessageRole::User, "second"),
            (second_assistant, MessageRole::Assistant, "second done"),
        ] {
            app.ui.messages.push(ChatMessage {
                id,
                role,
                body: body.into(),
                created_at: "2026-05-13T00:00:00Z".into(),
            });
            app.ui.timeline.push(TimelineItem::Message(id));
        }
        app.current_turn_user_message_id = Some(second_user);
        app.review_changes_started = false;
        app.upsert_review_file_change(
            "src/main.rs",
            FileChangeType::Modified,
            Some("B\n".into()),
            "C\n".into(),
        );
        assert!(app.persist_current_turn_file_changes());

        let turns = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap();
        assert_eq!(turns.len(), 2);
        let first_turn = turns
            .iter()
            .find(|summary| summary.message_id == Some(first_assistant))
            .unwrap();
        let first_diff = app
            .store
            .load_change_set_file_diff(&first_turn.id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(first_diff.old_text.as_deref(), Some("A\n"));
        assert_eq!(first_diff.new_text.as_deref(), Some("B\n"));

        let conversation = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentConversation),
            )
            .unwrap();
        assert_eq!(conversation.len(), 1);
        let conversation_diff = app
            .store
            .load_change_set_file_diff(&conversation[0].id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(conversation_diff.old_text.as_deref(), Some("A\n"));
        assert_eq!(conversation_diff.new_text.as_deref(), Some("C\n"));

        let reloaded = test_app(&dir);
        let reloaded_turns = reloaded
            .store
            .list_change_sets(
                Some(&reloaded.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap();
        assert_eq!(reloaded_turns.len(), 2);
        let reloaded_first = reloaded_turns
            .iter()
            .find(|summary| summary.message_id == Some(first_assistant))
            .unwrap();
        let reloaded_first_diff = reloaded
            .store
            .load_change_set_file_diff(&reloaded_first.id, "src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded_first_diff.old_text.as_deref(), Some("A\n"));
        assert_eq!(reloaded_first_diff.new_text.as_deref(), Some("B\n"));
    }

    #[test]
    fn missing_tool_diff_old_text_uses_session_target_as_turn_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        app.ui.session_changes = vec![SessionFileChange {
            path: "src/main.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("session base\n".into()),
            new_text: "before this turn\n".into(),
            added_lines: 1,
            removed_lines: 1,
            timestamp: "2026-05-13T00:00:00Z".into(),
        }];

        let baseline = app.tool_diff_baseline_text(
            "call-1",
            "src/main.rs",
            "after this turn\n",
            &HashMap::new(),
        );

        assert_eq!(baseline.as_deref(), Some("before this turn\n"));
    }

    #[test]
    fn abnormal_turn_notice_explains_refusal_without_blocking_followup() {
        let notice = turn_finished_notice("refusal", Some("CodeBuddy"))
            .expect("refusal should produce a visible notice");

        assert!(notice.contains("CodeBuddy"));
        assert!(notice.contains("refusal"));
        assert!(notice.contains("429"));
        assert!(turn_finished_notice("end_turn", Some("CodeBuddy")).is_none());
    }

    #[test]
    fn scoped_change_set_queries_keep_agent_sources_separate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let user_message_id = uuid::Uuid::new_v4();
        let assistant_message_id = uuid::Uuid::new_v4();
        let change = SessionFileChange {
            path: "src/main.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("before\n".into()),
            new_text: "after\n".into(),
            added_lines: 1,
            removed_lines: 1,
            timestamp: "1".into(),
        };

        app.current_turn_user_message_id = Some(user_message_id);
        app.ui.review_changes = vec![change.clone()];
        app.persist_current_agent_turn_change_set(
            Some(assistant_message_id),
            ChangeSetStatus::Complete,
        );
        app.ui.turn_changes.push(TurnFileChanges {
            message_id: assistant_message_id,
            changes: vec![change.clone()],
        });
        app.persist_agent_conversation_change_set_from_turns();

        let turn_sets = app.list_change_sets(ListChangeSetsRequest {
            source: Some(ChangeSetSource::AgentTurn),
            ..Default::default()
        });
        let conversation_sets = app.list_change_sets(ListChangeSetsRequest {
            source: Some(ChangeSetSource::AgentConversation),
            ..Default::default()
        });
        assert_eq!(turn_sets.len(), 1);
        assert_eq!(conversation_sets.len(), 1);

        let turn_diff = app
            .get_change_set_file_diff(GetChangeSetFileDiffRequest {
                change_set_id: turn_sets[0].id.clone(),
                path: "src/main.rs".into(),
            })
            .unwrap();
        let conversation_diff = app
            .get_change_set_file_diff(GetChangeSetFileDiffRequest {
                change_set_id: conversation_sets[0].id.clone(),
                path: "src/main.rs".into(),
            })
            .unwrap();
        let missing = app.get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: turn_sets[0].id.clone(),
            path: "src/other.rs".into(),
        });

        assert_eq!(turn_diff.old_text.as_deref(), Some("before\n"));
        assert_eq!(turn_diff.new_text.as_deref(), Some("after\n"));
        assert_eq!(conversation_diff.old_text.as_deref(), Some("before\n"));
        assert_eq!(conversation_diff.new_text.as_deref(), Some("after\n"));
        assert!(missing.is_none());
    }

    #[test]
    fn scoped_change_set_queries_keep_manual_source_separate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save(
            "src/manual.rs",
            Some("manual before\n".into()),
            "manual after\n".into(),
        );

        let manual_sets = app.list_change_sets(ListChangeSetsRequest {
            source: Some(ChangeSetSource::ManualEdit),
            ..Default::default()
        });
        assert_eq!(manual_sets.len(), 1);
        let files = app.list_change_set_files(ListChangeSetFilesRequest {
            change_set_id: manual_sets[0].id.clone(),
        });
        let diff = app
            .get_change_set_file_diff(GetChangeSetFileDiffRequest {
                change_set_id: manual_sets[0].id.clone(),
                path: "src/manual.rs".into(),
            })
            .unwrap();
        let agent_fallback = app.get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: "agent-conversation:missing".into(),
            path: "src/manual.rs".into(),
        });

        assert_eq!(files.files.len(), 1);
        assert_eq!(diff.old_text.as_deref(), Some("manual before\n"));
        assert_eq!(diff.new_text.as_deref(), Some("manual after\n"));
        assert!(agent_fallback.is_none());
    }

    #[test]
    fn scoped_change_set_queries_expose_git_worktree_without_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(&dir);
        let repo = init_test_git_repo(dir.path());
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "before\n").unwrap();
        commit_paths(&repo, &[".gitignore", "src/main.rs"]);

        fs::write(dir.path().join("src/main.rs"), "after\n").unwrap();
        app.refresh_repository();

        let git_sets = app.list_change_sets(ListChangeSetsRequest {
            source: Some(ChangeSetSource::GitWorktree),
            ..Default::default()
        });
        let unstaged = git_sets
            .iter()
            .find(|summary| summary.id == "git-worktree:unstaged")
            .expect("unstaged git change set should be exposed");
        let files = app.list_change_set_files(ListChangeSetFilesRequest {
            change_set_id: unstaged.id.clone(),
        });
        let diff = app
            .get_change_set_file_diff(GetChangeSetFileDiffRequest {
                change_set_id: unstaged.id.clone(),
                path: "src/main.rs".into(),
            })
            .unwrap();
        let persisted_git_sets = app
            .store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::GitWorktree),
            )
            .unwrap();

        assert_eq!(unstaged.status, ChangeSetStatus::Live);
        assert_eq!(files.files.len(), 1);
        assert_eq!(diff.old_text.as_deref(), Some("before\n"));
        assert_eq!(diff.new_text.as_deref(), Some("after\n"));
        assert!(persisted_git_sets.is_empty());
    }

    fn init_test_git_repo(path: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        fs::write(path.join(".gitignore"), "home/\n").unwrap();
        repo
    }

    fn commit_paths(repo: &git2::Repository, paths: &[&str]) {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(std::path::Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parent_oid = repo.head().ok().and_then(|head| head.target());
        let parent_commit = parent_oid.and_then(|oid| repo.find_commit(oid).ok());
        let parents = parent_commit.into_iter().collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parent_refs)
            .unwrap();
    }

    fn test_app(dir: &tempfile::TempDir) -> Application {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("app-core should live under crates/app-core")
            .join("Cargo.toml");
        let manifest = manifest.display().to_string().replace('\\', "/");
        Application::bootstrap_with_app_paths(
            dir.path(),
            format!(
                "cargo run --manifest-path {} -p mock-acp-agent --quiet --",
                manifest
            ),
            crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex")),
        )
        .unwrap()
    }
}

/// Try to extract a refined title from the assistant's first response.
/// Returns None if no good title can be extracted (keeps the prompt-based title).
fn extract_title_from_response(response: &str) -> Option<String> {
    // Get the first meaningful line (skip empty lines and markdown headers)
    let first_line = response
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))?;

    // Strip common assistant prefixes to get the action description
    let prefixes = [
        "I'll help you ",
        "I'll ",
        "I will ",
        "Let me ",
        "Sure, I'll ",
        "Sure! I'll ",
        "OK, I'll ",
        "Alright, I'll ",
        "Here's how to ",
        "I can help with ",
        "I can help you ",
        // Chinese prefixes
        "我来帮你",
        "让我",
        "好的，我来",
        "好的，让我",
        "我会",
        "我将",
    ];

    let mut text = first_line;
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
            break;
        }
    }

    let text = text.trim_end_matches('.');
    let text = text.trim();

    // If too short or same as just a function word, not useful
    if text.len() < 5 {
        return None;
    }

    // Capitalize first letter
    let title = if text.starts_with(|c: char| c.is_lowercase()) {
        let mut chars = text.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => return None,
        }
    } else {
        text.to_string()
    };

    // Truncate to 60 chars
    if title.chars().count() <= 60 {
        Some(title)
    } else {
        let truncated: String = title.chars().take(57).collect();
        Some(format!("{truncated}..."))
    }
}
