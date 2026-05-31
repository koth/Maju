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

pub(super) fn decide_permission(
    mode: PermissionPolicyMode,
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match mode {
        PermissionPolicyMode::Plan => decide_plan_permission(workspace_root, request),
        PermissionPolicyMode::Build => decide_build_permission(workspace_root, request),
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
                select_permission_option(request, false)
            }
        }
        ToolKind::Edit => {
            let paths = permission_paths(request);
            if !paths.is_empty()
                && paths_are_inside_workspace(workspace_root, &paths)
                && paths.iter().all(is_markdown_path)
            {
                select_permission_option(request, true)
            } else {
                select_permission_option(request, false)
            }
        }
        ToolKind::Execute | ToolKind::Delete | ToolKind::Move => {
            select_permission_option(request, false)
        }
        _ => select_permission_option(request, false),
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
    let option = request.options.iter().find(|option| {
        matches!(
            (allow, option.kind),
            (
                true,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            ) | (
                false,
                PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
            )
        )
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

fn is_markdown_path(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "md" | "mdx"))
        .unwrap_or(false)
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
}

fn contains_command_token(text: &str, token: &str) -> bool {
    let mut offset = 0;
    while let Some(index) = text[offset..].find(token) {
        let index = offset + index;
        let before = text[..index].chars().next_back();
        let after = text[index + token.len()..].chars().next();
        if !before.is_some_and(is_command_word_char) && !after.is_some_and(is_command_word_char) {
            return true;
        }
        offset = index + token.len();
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        PermissionOption, PermissionOptionKind, RequestPermissionRequest, SessionId,
        ToolCallUpdate, ToolCallUpdateFields,
    };
    use serde_json::json;

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
}
