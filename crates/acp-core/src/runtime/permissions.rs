use agent_client_protocol::schema::{PermissionOptionKind, RequestPermissionRequest, ToolKind};
use std::path::{Path, PathBuf};

use super::workspace_paths::paths_are_inside_workspace;
use crate::events::AgentEditPolicy;

mod broker;
mod shell;

pub(super) use broker::PermissionPolicyMode;
pub(crate) use broker::{PermissionBroker, PermissionResolution};
pub(super) use shell::shell_command_prefers_apply_patch_for_writes;
use shell::{
    collect_shell_commands, extract_write_paths_from_command_text, is_usable_write_path,
    request_shell_write_should_retry_with_apply_patch, resolve_paths_against_workspace,
    shell_command_absolute_paths_stay_inside_workspace, shell_command_directly_mutates_files,
    shell_command_is_plan_read_only,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PermissionDecision {
    Select(String),
    SelectWithGuidance(String, String),
    Cancel,
    Ask,
}

fn reject_permission_option_id(request: &RequestPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::RejectOnce)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::RejectAlways)
        })
        .map(|option| option.option_id.0.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CodeBuddyTerminalPermissionDecision {
    Allow,
    Ask(Vec<PathBuf>),
    Reject,
}

#[cfg(test)]
pub(super) fn decide_permission(
    mode: PermissionPolicyMode,
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    decide_permission_with_edit_policy(mode, AgentEditPolicy::None, workspace_root, request)
}

pub(super) fn decide_permission_with_edit_policy(
    mode: PermissionPolicyMode,
    edit_policy: AgentEditPolicy,
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    if request_has_user_input_questions(request) {
        return PermissionDecision::Ask;
    }

    if mode != PermissionPolicyMode::FullAccess
        && edit_policy == AgentEditPolicy::PreferApplyPatch
        && request_should_retry_with_apply_patch(workspace_root, request)
    {
        return reject_with_apply_patch_guidance(request);
    }

    if is_codebuddy_bash_request(request) {
        return decide_codebuddy_bash_permission(mode, workspace_root, request);
    }

    match mode {
        PermissionPolicyMode::ReadOnly => decide_read_only_permission(workspace_root, request),
        PermissionPolicyMode::Build => decide_build_permission(workspace_root, request),
        PermissionPolicyMode::FullAccess => decide_full_access_permission(workspace_root, request),
    }
}

const APPLY_PATCH_RETRY_GUIDANCE: &str = "Use the apply_patch tool for ordinary text file create, update, and delete operations. Retry this edit with an apply_patch patch. Direct filesystem writes are reserved for formatters, generators, package managers, lockfiles, and binary or media files.";

pub(super) fn apply_patch_retry_guidance() -> &'static str {
    APPLY_PATCH_RETRY_GUIDANCE
}

fn reject_with_apply_patch_guidance(request: &RequestPermissionRequest) -> PermissionDecision {
    reject_permission_option_id(request)
        .map(|option_id| {
            PermissionDecision::SelectWithGuidance(
                option_id,
                APPLY_PATCH_RETRY_GUIDANCE.to_string(),
            )
        })
        .unwrap_or(PermissionDecision::Cancel)
}

pub(super) fn path_prefers_apply_patch(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        file_name.as_str(),
        "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lockb"
    ) {
        return false;
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension.is_empty() {
        return true;
    }

    !matches!(
        extension.as_str(),
        "7z" | "a"
            | "avi"
            | "avif"
            | "bin"
            | "bmp"
            | "class"
            | "dll"
            | "dmg"
            | "doc"
            | "docx"
            | "dylib"
            | "eot"
            | "exe"
            | "gif"
            | "gz"
            | "ico"
            | "jar"
            | "jpeg"
            | "jpg"
            | "lock"
            | "mov"
            | "mp3"
            | "mp4"
            | "o"
            | "otf"
            | "pdf"
            | "png"
            | "ppt"
            | "pptx"
            | "pyc"
            | "rar"
            | "so"
            | "sqlite"
            | "tar"
            | "tgz"
            | "ttf"
            | "wasm"
            | "webm"
            | "webp"
            | "woff"
            | "woff2"
            | "xls"
            | "xlsx"
            | "zip"
    )
}

fn request_should_retry_with_apply_patch(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> bool {
    if request_is_codex_apply_patch_approval(request) {
        return false;
    }

    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            let paths = resolve_paths_against_workspace(workspace_root, permission_paths(request));
            !paths.is_empty()
                && paths_are_inside_workspace(workspace_root, &paths)
                && paths.iter().any(|path| path_prefers_apply_patch(path))
        }
        ToolKind::Execute => {
            request_shell_write_should_retry_with_apply_patch(workspace_root, request)
        }
        _ => false,
    }
}

fn request_has_user_input_questions(request: &RequestPermissionRequest) -> bool {
    request
        .tool_call
        .fields
        .raw_input
        .as_ref()
        .and_then(|raw_input| raw_input.get("questions"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|questions| !questions.is_empty())
}

fn decide_codebuddy_bash_permission(
    mode: PermissionPolicyMode,
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
        return PermissionDecision::Ask;
    }

    if !commands.is_empty() {
        return PermissionDecision::Ask;
    }

    match mode {
        PermissionPolicyMode::FullAccess => PermissionDecision::Ask,
        _ => select_permission_option(request, false),
    }
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
        return CodeBuddyTerminalPermissionDecision::Ask(Vec::new());
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

fn decide_read_only_permission(
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
            PermissionDecision::Ask
        }
        ToolKind::Read | ToolKind::Search => {
            let paths = permission_paths(request);
            if paths_are_inside_workspace(workspace_root, &paths) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
            let paths = resolve_paths_against_workspace(workspace_root, permission_paths(request));
            if !paths.is_empty() && paths_are_inside_workspace(workspace_root, &paths) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        _ => select_permission_option(request, true),
    }
}

fn decide_full_access_permission(
    workspace_root: &str,
    request: &RequestPermissionRequest,
) -> PermissionDecision {
    match request.tool_call.fields.kind.unwrap_or(ToolKind::Other) {
        ToolKind::SwitchMode => PermissionDecision::Ask,
        ToolKind::Read | ToolKind::Search => {
            let paths = permission_paths(request);
            if paths_are_inside_workspace(workspace_root, &paths) {
                select_permission_option(request, true)
            } else {
                PermissionDecision::Ask
            }
        }
        ToolKind::Execute if request_has_direct_shell_file_mutation(request) => {
            PermissionDecision::Ask
        }
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => PermissionDecision::Ask,
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

fn request_is_codex_apply_patch_approval(request: &RequestPermissionRequest) -> bool {
    let has_apply_patch_approval_options = request.options.iter().any(|option| {
        option.option_id.0.as_ref() == "approved"
            && option.kind == PermissionOptionKind::AllowOnce
            && option.name.trim().eq_ignore_ascii_case("Yes")
    }) && request.options.iter().any(|option| {
        option.option_id.0.as_ref() == "abort"
            && option.kind == PermissionOptionKind::RejectOnce
            && option
                .name
                .trim()
                .eq_ignore_ascii_case("No, provide feedback")
    });

    has_apply_patch_approval_options
        && request
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .is_some_and(|raw_input| {
                raw_input.get("call_id").is_some() && raw_input.get("changes").is_some()
            })
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

pub(super) fn request_has_direct_shell_file_mutation(request: &RequestPermissionRequest) -> bool {
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

#[cfg(test)]
mod tests;
