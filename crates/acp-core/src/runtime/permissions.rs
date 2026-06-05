use agent_client_protocol::schema::{PermissionOptionKind, RequestPermissionRequest, ToolKind};
use anyhow::anyhow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use super::workspace_paths::paths_are_inside_workspace;

#[derive(Clone, Debug, Default)]
pub(crate) struct PermissionBroker {
    state: Arc<Mutex<PermissionBrokerState>>,
    mode: Arc<Mutex<PermissionPolicyMode>>,
}

#[derive(Debug, Default)]
struct PermissionBrokerState {
    pending: HashMap<String, mpsc::Sender<Option<String>>>,
    early_resolutions: HashMap<String, Option<String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PermissionPolicyMode {
    Plan,
    #[default]
    Build,
}

impl PermissionBroker {
    pub(crate) fn register(
        &self,
        request_id: String,
    ) -> anyhow::Result<mpsc::Receiver<Option<String>>> {
        let (tx, rx) = mpsc::channel();

        let early_resolution = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(option_id) = state.early_resolutions.remove(&request_id) {
                Some(option_id)
            } else {
                state.pending.insert(request_id, tx.clone());
                None
            }
        };

        if let Some(option_id) = early_resolution {
            tx.send(option_id)
                .map_err(|_| anyhow!("permission request already closed"))?;
        }

        Ok(rx)
    }

    pub(crate) fn resolve(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> anyhow::Result<bool> {
        let sender = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            if let Some(sender) = state.pending.remove(request_id) {
                Some(sender)
            } else {
                state
                    .early_resolutions
                    .insert(request_id.to_string(), option_id.clone());
                None
            }
        };

        let Some(sender) = sender else {
            return Ok(false);
        };

        sender
            .send(option_id)
            .map_err(|_| anyhow!("permission request already closed"))?;
        Ok(true)
    }

    pub(crate) fn clear_early_resolution(&self, request_id: &str) -> anyhow::Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow!("permission broker lock poisoned"))?
            .early_resolutions
            .remove(request_id);
        Ok(())
    }

    pub(crate) fn set_mode(&self, mode_id: &str) -> anyhow::Result<()> {
        let mode = if mode_id.eq_ignore_ascii_case("build") {
            PermissionPolicyMode::Build
        } else {
            PermissionPolicyMode::Plan
        };
        *self
            .mode
            .lock()
            .map_err(|_| anyhow!("permission broker lock poisoned"))? = mode;
        Ok(())
    }

    pub(super) fn mode(&self) -> PermissionPolicyMode {
        self.mode.lock().map(|mode| *mode).unwrap_or_default()
    }

    pub(crate) fn cancel_all(&self) -> anyhow::Result<()> {
        let pending = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("permission broker lock poisoned"))?;
            state.early_resolutions.clear();
            std::mem::take(&mut state.pending)
        };
        for (_, sender) in pending {
            let _ = sender.send(None);
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PermissionDecision {
    Select(String),
    Cancel,
    Ask,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CodeBuddyTerminalPermissionDecision {
    Allow,
    Ask(Vec<PathBuf>),
    Reject,
}

pub(super) fn decide_permission(
    mode: PermissionPolicyMode,
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    if is_codebuddy_bash_request(request) {
        return decide_codebuddy_bash_permission(workspace_root, request);
    }

    match mode {
        PermissionPolicyMode::Plan => decide_plan_permission(workspace_root, request),
        PermissionPolicyMode::Build => decide_build_permission(workspace_root, request),
    }
}

fn decide_codebuddy_bash_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    let mut commands = Vec::new();
    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_shell_commands(raw_input, &mut commands);
    }

    if !commands.is_empty()
        && commands.iter().all(|command| {
            shell_command_is_plan_read_only(command)
                && shell_command_absolute_paths_stay_inside_workspace(workspace_root, command)
        })
    {
        return select_permission_option(request, true);
    }

    let write_hint_paths = codebuddy_bash_write_hint_paths(request);
    if !write_hint_paths.is_empty() {
        return PermissionDecision::Ask;
    }

    if commands
        .iter()
        .any(|command| shell_command_directly_mutates_files(command))
    {
        return select_permission_option(request, false);
    }

    if !commands.is_empty() {
        return PermissionDecision::Ask;
    }

    select_permission_option(request, false)
}

pub(super) fn decide_codebuddy_terminal_permission(
    workspace_root: &str,
    command: &str,
) -> CodeBuddyTerminalPermissionDecision {
    if shell_command_is_plan_read_only(command)
        && shell_command_absolute_paths_stay_inside_workspace(workspace_root, command)
    {
        return CodeBuddyTerminalPermissionDecision::Allow;
    }

    let mut paths = extract_write_paths_from_command_text(command)
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    if !paths.is_empty() {
        return CodeBuddyTerminalPermissionDecision::Ask(paths);
    }

    if shell_command_directly_mutates_files(command) {
        return CodeBuddyTerminalPermissionDecision::Reject;
    }

    if !command.trim().is_empty() {
        return CodeBuddyTerminalPermissionDecision::Ask(Vec::new());
    }

    CodeBuddyTerminalPermissionDecision::Reject
}

fn is_codebuddy_bash_request(request: &RequestPermissionRequest) -> bool {
    codebuddy_permission_tool_name(request)
        .as_deref()
        .is_some_and(|tool_name| tool_name.eq_ignore_ascii_case("Bash"))
}

fn codebuddy_permission_tool_name(request: &RequestPermissionRequest) -> Option<String> {
    let payload = serde_json::to_value(request).ok()?;
    find_codebuddy_tool_name(&payload).map(str::to_string)
}

fn find_codebuddy_tool_name(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(meta) = object.get("_meta").and_then(|value| value.as_object())
                && let Some(tool_name) = meta
                    .get("codebuddy.ai/toolName")
                    .and_then(serde_json::Value::as_str)
            {
                return Some(tool_name);
            }
            object.values().find_map(find_codebuddy_tool_name)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_codebuddy_tool_name),
        _ => None,
    }
}

fn decide_plan_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::SwitchMode => PermissionDecision::Ask,
        ToolKind::Read | ToolKind::Search => {
            if paths_are_inside_workspace(workspace_root, &permission_paths(request)) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        ToolKind::Execute => {
            if request_is_plan_read_only_shell(workspace_root, request) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => PermissionDecision::Ask,
        _ => PermissionDecision::Ask,
    }
}

fn decide_build_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::SwitchMode => PermissionDecision::Ask,
        ToolKind::Execute if request_has_direct_shell_file_mutation(request) => {
            select_permission_option(request, false)
        }
        ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            let paths = permission_paths(request);
            if paths_are_inside_workspace(workspace_root, &paths) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        _ => select_permission_option(request, true),
    }
}

fn select_permission_option(request: &RequestPermissionRequest, allow: bool) -> PermissionDecision {
    let preferred_kind = if allow {
        PermissionOptionKind::AllowOnce
    } else {
        PermissionOptionKind::RejectOnce
    };
    let fallback_kind = if allow {
        PermissionOptionKind::AllowAlways
    } else {
        PermissionOptionKind::RejectAlways
    };
    let option = request
        .options
        .iter()
        .find(|option| option.kind == preferred_kind)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == fallback_kind)
        });

    option
        .map(|option| PermissionDecision::Select(option.option_id.0.to_string()))
        .unwrap_or(PermissionDecision::Cancel)
}

fn permission_paths(request: &RequestPermissionRequest) -> Vec<PathBuf> {
    let mut paths = request
        .tool_call
        .fields
        .locations
        .as_ref()
        .map(|locations| {
            locations
                .iter()
                .map(|location| location.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_path_like_values(raw_input, &mut paths);
    }

    paths
}

pub(super) fn codebuddy_bash_write_hint_paths(request: &RequestPermissionRequest) -> Vec<PathBuf> {
    let mut paths = Vec::<String>::new();
    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_explicit_target_path_values(raw_input, &mut paths);
        let mut commands = Vec::new();
        collect_shell_commands(raw_input, &mut commands);
        for command in commands {
            paths.extend(extract_write_paths_from_command_text(&command));
        }
    }

    let mut paths = paths
        .into_iter()
        .map(|path| path.trim().to_string())
        .filter(|path| is_usable_write_path(path))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn collect_explicit_target_path_values(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                let key = key.trim_matches('"');
                if matches!(key, "path" | "file" | "file_path" | "filepath")
                    || key.ends_with("path")
                    || key.ends_with("file")
                {
                    if let Some(path) = value.as_str() {
                        paths.push(path.to_string());
                        continue;
                    }
                }
                collect_explicit_target_path_values(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_explicit_target_path_values(item, paths);
            }
        }
        _ => {}
    }
}

fn collect_path_like_values(value: &serde_json::Value, paths: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if key.contains("path") || key == "file" || key == "cwd" || key.ends_with("file") {
                    if let Some(path) = value.as_str() {
                        paths.push(PathBuf::from(path));
                        continue;
                    }
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

fn request_has_direct_shell_file_mutation(request: &RequestPermissionRequest) -> bool {
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
        .any(|command| shell_command_directly_mutates_files(command))
}

fn request_is_plan_read_only_shell(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> bool {
    let mut commands = Vec::new();
    if let Some(raw_input) = &request.tool_call.fields.raw_input {
        collect_shell_commands(raw_input, &mut commands);
    }

    !commands.is_empty()
        && commands.iter().all(|command| {
            shell_command_is_plan_read_only(command)
                && shell_command_absolute_paths_stay_inside_workspace(workspace_root, command)
        })
}

fn collect_shell_commands(value: &serde_json::Value, commands: &mut Vec<String>) {
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

fn shell_command_is_plan_read_only(command: &str) -> bool {
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

fn shell_command_absolute_paths_stay_inside_workspace(workspace_root: &str, command: &str) -> bool {
    let Some(paths) = shell_command_absolute_paths(command) else {
        return false;
    };

    paths.is_empty() || paths_are_inside_workspace(workspace_root, &paths)
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

    None
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

fn shell_command_directly_mutates_files(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if contains_command_token(&lower, "apply_patch") {
        return false;
    }
    shell_redirection_writes_file(command)
        || contains_command_token(&lower, "tee")
        || contains_command_token(&lower, "truncate")
        || contains_command_token(&lower, "touch")
        || contains_command_token(&lower, "rm")
        || contains_command_token(&lower, "mv")
        || contains_command_token(&lower, "cp")
        || contains_command_token(&lower, "set-content")
        || contains_command_token(&lower, "add-content")
        || contains_command_token(&lower, "out-file")
        || contains_command_token(&lower, "remove-item")
        || contains_command_token(&lower, "move-item")
        || contains_command_token(&lower, "copy-item")
        || (contains_command_token(&lower, "new-item")
            && lower.contains("-itemtype")
            && lower.contains("file"))
        || (contains_command_token(&lower, "sed") && lower.contains(" -i"))
        || (contains_command_token(&lower, "perl") && lower.contains(" -pi"))
        || lower.contains(".write_text(")
        || lower.contains(".write_bytes(")
        || lower.contains("writefile")
        || lower.contains("writefilesync")
}

fn contains_command_token(text: &str, token: &str) -> bool {
    find_command_token(text, token).is_some()
}

fn find_command_token(text: &str, token: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(index) = text[offset..].find(token) {
        let index = offset + index;
        let before = text[..index].chars().next_back();
        let after = text[index + token.len()..].chars().next();
        if !before.is_some_and(is_command_word_char) && !after.is_some_and(is_command_word_char) {
            return Some(index);
        }
        offset = index + token.len();
    }
    None
}

fn is_command_word_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '_' | '-')
}

fn shell_redirection_writes_file(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'>' {
            index += 1;
            continue;
        }
        if index > 0 && bytes[index - 1].is_ascii_digit() {
            index += 1;
            continue;
        }
        let mut target_start = index + 1;
        if target_start < bytes.len() && bytes[target_start] == b'>' {
            target_start += 1;
        }
        if let Some(target) = shell_redirection_target(command, target_start)
            && !is_null_redirection_target(&target)
            && !target.starts_with('&')
        {
            return true;
        }
        index = target_start;
    }
    false
}

fn shell_redirection_target(command: &str, start: usize) -> Option<String> {
    let mut index = start;
    let chars = command.as_bytes();
    while index < chars.len() && chars[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }

    let quote = chars[index];
    if quote == b'\'' || quote == b'"' {
        let mut end = index + 1;
        while end < chars.len() && chars[end] != quote {
            end += 1;
        }
        return Some(command[index + 1..end].trim().to_string());
    }

    let mut end = index;
    while end < chars.len()
        && !chars[end].is_ascii_whitespace()
        && !matches!(chars[end], b';' | b'|')
    {
        end += 1;
    }
    Some(command[index..end].trim().to_string()).filter(|target| !target.is_empty())
}

fn is_null_redirection_target(target: &str) -> bool {
    matches!(
        target.trim().to_ascii_lowercase().as_str(),
        "/dev/null" | "$null" | "nul" | "null"
    )
}

fn extract_write_paths_from_command_text(command: &str) -> Vec<String> {
    let command = strip_powershell_here_strings(command);
    let mut paths = Vec::new();
    collect_shell_redirection_paths(&command, &mut paths);
    collect_powershell_write_cmdlet_paths(&command, &mut paths);
    collect_python_pathlib_write_paths(&command, &mut paths);
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

fn collect_common_mutation_command_paths(command: &str, paths: &mut Vec<String>) {
    for segment in split_shell_segments(command) {
        let words = shell_words(&segment);
        let Some(command_word) = shell_command_word(&words) else {
            continue;
        };
        let command = shell_command_basename(command_word);
        let args = words
            .iter()
            .skip_while(|word| word.as_str() != command_word)
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>();

        match command.as_str() {
            "mkdir" | "touch" | "rm" | "rmdir" | "del" | "erase" | "remove-item" => {
                paths.extend(command_path_args(&args));
            }
            "mv" | "move" | "move-item" | "cp" | "copy" | "copy-item" => {
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

fn split_shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for pipeline in split_shell_pipeline(command) {
        for segment in pipeline.split([';', '\n']) {
            for part in segment.split("&&") {
                for item in part.split("||") {
                    let item = item.trim();
                    if !item.is_empty() {
                        segments.push(item.to_string());
                    }
                }
            }
        }
    }
    segments
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        PermissionOption, PermissionOptionKind, RequestPermissionRequest, SessionId,
        ToolCallUpdate, ToolCallUpdateFields,
    };
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn switch_mode_request() -> RequestPermissionRequest {
        RequestPermissionRequest::new(
            SessionId::new("session-1"),
            ToolCallUpdate::new(
                "exit-plan",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::SwitchMode)
                    .title("Ready to code?".to_string()),
            ),
            vec![
                PermissionOption::new("default", "Yes", PermissionOptionKind::AllowOnce),
                PermissionOption::new("plan", "No", PermissionOptionKind::RejectOnce),
            ],
        )
    }

    fn execute_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
        RequestPermissionRequest::new(
            SessionId::new("session-1"),
            ToolCallUpdate::new(
                "shell",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .title("Shell".to_string())
                    .raw_input(raw_input),
            ),
            vec![
                PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
            ],
        )
    }

    fn codebuddy_bash_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
        let mut payload = serde_json::to_value(execute_request(raw_input)).unwrap();
        let tool_call_key = if payload.get("toolCall").is_some() {
            "toolCall"
        } else {
            "tool_call"
        };
        let tool_call = payload
            .get_mut(tool_call_key)
            .and_then(serde_json::Value::as_object_mut)
            .expect("request should serialize a tool call object");
        tool_call.insert(
            "_meta".into(),
            json!({
                "codebuddy.ai/toolName": "Bash"
            }),
        );
        serde_json::from_value(payload).unwrap()
    }

    fn execute_request_with_permission_options(
        raw_input: serde_json::Value,
        options: Vec<PermissionOption>,
    ) -> RequestPermissionRequest {
        RequestPermissionRequest::new(
            SessionId::new("session-1"),
            ToolCallUpdate::new(
                "shell",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .title("Shell".to_string())
                    .raw_input(raw_input),
            ),
            options,
        )
    }

    fn temp_workspace(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kodex-permissions-{name}-{nanos}"));
        fs::create_dir_all(root.join("packages/backend/src")).expect("workspace should be created");
        root
    }

    #[test]
    fn switch_mode_permission_is_always_interactive() {
        let request = switch_mode_request();

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Ask,
        );
        assert_eq!(
            decide_permission(PermissionPolicyMode::Plan, "D:/work/repo", &request),
            PermissionDecision::Ask,
        );
    }

    #[test]
    fn build_permission_rejects_shell_redirection_file_writes() {
        let request = execute_request(json!({
            "command": "cat > AGENTS.md << 'ENDOFFILE'\n# Guidelines\nENDOFFILE"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("reject".to_string()),
        );
    }

    #[test]
    fn build_permission_rejects_powershell_file_writes() {
        let request = execute_request(json!({
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                "Set-Content -Path AGENTS.md -Value '# Guidelines'"
            ]
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("reject".to_string()),
        );
    }

    #[test]
    fn build_permission_allows_shell_reads_and_apply_patch_wrappers() {
        let read_request = execute_request(json!({ "command": "rg -n \"TODO\" src 2>/dev/null" }));
        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &read_request),
            PermissionDecision::Select("allow".to_string()),
        );

        let patch_request = execute_request(json!({
            "command": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\nPATCH"
        }));
        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &patch_request),
            PermissionDecision::Select("allow".to_string()),
        );
    }

    #[test]
    fn automatic_permission_selection_prefers_once_over_always() {
        let request = execute_request_with_permission_options(
            json!({ "command": "rg -n \"TODO\" src" }),
            vec![
                PermissionOption::new(
                    "allow_always",
                    "Always Allow",
                    PermissionOptionKind::AllowAlways,
                ),
                PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
            ],
        );
        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("allow".to_string()),
        );

        let request = execute_request_with_permission_options(
            json!({ "command": "cat > AGENTS.md << 'END'\n# Guidelines\nEND" }),
            vec![
                PermissionOption::new(
                    "reject_always",
                    "Always Reject",
                    PermissionOptionKind::RejectAlways,
                ),
                PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
            ],
        );
        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("reject".to_string()),
        );
    }

    #[test]
    fn codebuddy_bash_read_only_command_is_auto_allowed() {
        let request = codebuddy_bash_request(json!({
            "command": "rg -n \"TODO\" src"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("allow".to_string()),
        );
    }

    #[test]
    fn codebuddy_terminal_read_only_command_is_allowed() {
        assert_eq!(
            decide_codebuddy_terminal_permission("D:/work/repo", "rg -n \"TODO\" src"),
            CodeBuddyTerminalPermissionDecision::Allow,
        );
    }

    #[test]
    fn codebuddy_bash_windows_find_pipeline_inside_workspace_is_auto_allowed() {
        let command = r#"find "d:\work\ArtAssets\packages\frontend\src" -name "*auth*" -o -name "*user*" | head -20"#;
        let request = codebuddy_bash_request(json!({
            "command": command
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/ArtAssets", &request),
            PermissionDecision::Select("allow".to_string()),
        );
        assert_eq!(
            decide_codebuddy_terminal_permission("D:/work/ArtAssets", command),
            CodeBuddyTerminalPermissionDecision::Allow,
        );
    }

    #[test]
    fn codebuddy_terminal_pathlib_write_with_explicit_path_is_interactive() {
        assert_eq!(
            decide_codebuddy_terminal_permission(
                "D:/work/repo",
                "python - <<'PY'\nfrom pathlib import Path\np=Path('packages/backend/src/service.ts')\np.write_text('ok', encoding='utf-8')\nPY",
            ),
            CodeBuddyTerminalPermissionDecision::Ask(vec![PathBuf::from(
                "packages/backend/src/service.ts"
            )]),
        );
    }

    #[test]
    fn codebuddy_terminal_suspected_write_without_static_path_is_rejected() {
        assert_eq!(
            decide_codebuddy_terminal_permission(
                "D:/work/repo",
                "python - <<'PY'\nfrom pathlib import Path\np=Path.cwd() / 'generated.ts'\np.write_text('ok', encoding='utf-8')\nPY",
            ),
            CodeBuddyTerminalPermissionDecision::Reject,
        );
    }

    #[test]
    fn codebuddy_terminal_build_command_without_static_path_is_interactive() {
        assert_eq!(
            decide_codebuddy_terminal_permission("D:/work/repo", "pnpm build"),
            CodeBuddyTerminalPermissionDecision::Ask(Vec::new()),
        );
    }

    #[test]
    fn codebuddy_bash_write_with_explicit_path_is_interactive() {
        let request = codebuddy_bash_request(json!({
            "command": "cat > src/main.rs << 'EOF'\nfn main() {}\nEOF"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Ask,
        );
        assert_eq!(
            codebuddy_bash_write_hint_paths(&request),
            vec![PathBuf::from("src/main.rs")]
        );
    }

    #[test]
    fn codebuddy_bash_pathlib_write_with_explicit_path_is_interactive() {
        let request = codebuddy_bash_request(json!({
            "command": "python - <<'PY'\nfrom pathlib import Path\np=Path('packages/backend/src/service.ts')\np.write_text('ok', encoding='utf-8')\nPY"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Ask,
        );
        assert_eq!(
            codebuddy_bash_write_hint_paths(&request),
            vec![PathBuf::from("packages/backend/src/service.ts")]
        );
    }

    #[test]
    fn codebuddy_bash_suspected_write_without_static_path_is_rejected() {
        let request = codebuddy_bash_request(json!({
            "command": "python - <<'PY'\nfrom pathlib import Path\np=Path.cwd() / 'generated.ts'\np.write_text('ok', encoding='utf-8')\nPY"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Select("reject".to_string()),
        );
    }

    #[test]
    fn codebuddy_bash_build_command_without_static_path_is_interactive() {
        let request = codebuddy_bash_request(json!({
            "command": "pnpm build"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
            PermissionDecision::Ask,
        );
    }

    #[test]
    fn plan_permission_allows_read_only_shell_exploration_inside_workspace() {
        let root = temp_workspace("readonly");
        let root_display = root.to_string_lossy().replace('\\', "/");
        let request = execute_request(json!({
            "command": format!(
                "find {root_display}/packages/backend/src -type f -name \"*.ts\" | grep -E \"(search|score)\" | head -20"
            )
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Plan, root.to_str().unwrap(), &request),
            PermissionDecision::Select("allow".to_string()),
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn plan_permission_allows_codebuddy_unix_drive_shell_paths_inside_workspace() {
        let root = std::env::current_dir().expect("test should run in the workspace");
        let root_display = root.to_string_lossy().replace('\\', "/");
        let mut chars = root_display.chars();
        let drive = chars
            .next()
            .expect("windows current dir should start with a drive letter");
        assert_eq!(chars.next(), Some(':'));
        let unix_drive_root = format!("/{}{}", drive.to_ascii_lowercase(), chars.as_str());
        let request = execute_request(json!({
            "command": format!(
                "find {unix_drive_root}/crates/acp-core/src -type f -name \"*.rs\" | head -20"
            )
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Plan, root.to_str().unwrap(), &request),
            PermissionDecision::Select("allow".to_string()),
        );
    }

    #[test]
    fn plan_permission_asks_for_shell_file_mutations() {
        let root = temp_workspace("mutation");
        let request = execute_request(json!({
            "command": "find packages -type f -name \"*.ts\" -delete"
        }));

        assert_eq!(
            decide_permission(PermissionPolicyMode::Plan, root.to_str().unwrap(), &request),
            PermissionDecision::Ask,
        );

        let _ = fs::remove_dir_all(root);
    }
}
