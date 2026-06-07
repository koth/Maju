use crate::application::diff_utils::reverse_apply_diff_hunks;
use acp_core::{ClientEvent, diff_to_hunks};
use serde_json::{Map, Value};
use workspace_model::{
    AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, ChatMessage, DiffHunk,
    DiffLineKind, SessionStatus, SidebarSection, TerminalOutput, ThinkingStatus, TimelineItem,
    ToolDiffPreview, ToolInvocation, ToolLogEntry, ToolStatus, UiSnapshot,
};

const MAX_TOOL_DETAIL_CHARS: usize = 32 * 1024;
const MAX_TOOL_RAW_INPUT_CHARS: usize = 16 * 1024;
const MAX_TOOL_RAW_OUTPUT_CHARS: usize = 32 * 1024;
const MAX_TOOL_LOG_CHARS: usize = 4 * 1024;

pub(crate) fn apply_event(ui: &mut UiSnapshot, event: ClientEvent) {
    match event {
        ClientEvent::SessionStarted { .. } => {}
        ClientEvent::ThinkingActivity { active } => {
            if active {
                if ui.thinking_status != Some(ThinkingStatus::Active) {
                    ui.timeline.push(TimelineItem::Thinking);
                }
                ui.thinking_status = Some(ThinkingStatus::Active);
            } else {
                ui.thinking_status = Some(ThinkingStatus::Completed);
            }
        }
        ClientEvent::TurnFinished { stop_reason } => {
            finalize_open_tools(ui, &stop_reason);
            ui.thinking_status = None;
            ui.agent_plan.clear();
            ui.session.status = SessionStatus::Idle;
            let section_title = if stop_reason == "end_turn" {
                "轮次结果"
            } else {
                "轮次异常"
            };
            ui.inspector_sections.push(SidebarSection {
                title: section_title.into(),
                items: vec![stop_reason],
            });
        }
        ClientEvent::MessageChunk { role, content } => {
            if role == workspace_model::MessageRole::System
                && is_internal_session_metadata(&content)
            {
                return;
            }
            if role == workspace_model::MessageRole::Assistant {
                if ui.thinking_status == Some(ThinkingStatus::Active) {
                    ui.thinking_status = Some(ThinkingStatus::Completed);
                }
                ui.session.status = SessionStatus::Streaming;
            }
            push_message(ui, role, content);
        }
        ClientEvent::ContextCompactionStarted { message } => {
            push_context_compaction_notice(ui, context_compaction_started_notice(&message));
        }
        ClientEvent::ContextCompacted { message } => {
            push_or_replace_context_compaction_notice(ui, context_compacted_notice(&message));
        }
        ClientEvent::ToolMessageChunk { id, content } => {
            finalize_running_children(ui, &id, None);
            let tool = ensure_tool(ui, &id, None, "tool", "tool", false);
            tool.status = ToolStatus::Running;
            tool.detail_text.push_str(&content);
            cap_string_in_place(&mut tool.detail_text, MAX_TOOL_DETAIL_CHARS);
            if !tool.detail_text.ends_with('\n') {
                push_tool_log(tool, "Agent", collapse_whitespace(&content));
            }
            ui.session.status = SessionStatus::WaitingForTool;
        }
        ClientEvent::ToolStarted {
            id,
            parent_id,
            name,
            kind,
            summary,
            is_subagent,
            raw_input,
        } => {
            let task_update_raw_input = if name == "TaskUpdate" {
                raw_input.clone()
            } else {
                None
            };
            let todo_write_raw_input = if is_codebuddy_todo_write_tool(&name) {
                raw_input.clone()
            } else {
                None
            };
            if let Some(parent_id) = parent_id.as_deref() {
                finalize_running_children(ui, parent_id, Some(&id));
            }
            let display_summary = normalize_tool_summary_for_display(ui, &summary);
            let tool = ensure_tool(ui, &id, parent_id, &name, &kind, is_subagent);
            tool.name = name;
            tool.kind = kind;
            tool.summary = display_summary.clone();
            tool.status = ToolStatus::Running;
            tool.is_subagent = is_subagent;
            tool.raw_input = cap_tool_raw_input(raw_input);
            tool.raw_output = None;
            tool.terminal_output = None;
            tool.error = None;
            push_tool_log(tool, "Requested", display_summary);
            apply_codebuddy_task_update(ui, task_update_raw_input.as_deref());
            apply_codebuddy_todo_write(ui, todo_write_raw_input.as_deref());
            ui.session.status = SessionStatus::WaitingForTool;
        }
        ClientEvent::ToolUpdated {
            id,
            parent_id,
            name,
            kind,
            summary,
            is_subagent,
            raw_input,
            raw_output,
            terminal_output,
            is_partial,
        } => {
            if let Some(parent_id) = parent_id.as_deref() {
                finalize_running_children(ui, parent_id, Some(&id));
            }
            let display_summary = summary
                .as_deref()
                .map(|summary| normalize_tool_summary_for_display(ui, summary));
            let (updated_tool_name, updated_raw_input) = {
                let tool = ensure_tool(ui, &id, parent_id, "tool", "tool", is_subagent);
                if let Some(name) = name {
                    tool.name = name;
                }
                if let Some(kind) = kind {
                    tool.kind = kind;
                }
                if is_subagent {
                    tool.is_subagent = true;
                }
                if let Some(summary) = display_summary {
                    tool.summary = summary.clone();
                    if !is_partial {
                        push_tool_log(tool, "Update", summary);
                    }
                }
                if let Some(raw_input) = cap_tool_raw_input(raw_input) {
                    tool.raw_input = Some(raw_input);
                }
                if let Some(raw_output) = cap_optional_string(raw_output, MAX_TOOL_RAW_OUTPUT_CHARS)
                {
                    tool.raw_output = Some(raw_output);
                }
                if let Some(terminal_output) = terminal_output {
                    tool.terminal_output = Some(cap_terminal_output(terminal_output));
                }
                if !is_partial && !matches!(tool.status, ToolStatus::Succeeded | ToolStatus::Failed)
                {
                    tool.status = ToolStatus::Running;
                }
                (tool.name.clone(), tool.raw_input.clone())
            };
            if updated_tool_name == "TaskUpdate" {
                apply_codebuddy_task_update(ui, updated_raw_input.as_deref());
            }
            if is_codebuddy_todo_write_tool(&updated_tool_name) {
                apply_codebuddy_todo_write(ui, updated_raw_input.as_deref());
            }
            if !is_partial {
                ui.session.status = SessionStatus::WaitingForTool;
            }
        }
        ClientEvent::ToolPermissionRequest {
            id,
            name,
            options,
            details,
            input,
        } => {
            let tool = ensure_tool(ui, &id, None, &name, "permission", false);
            let summary = if options.is_empty() {
                "等待权限".to_string()
            } else {
                let labels = options
                    .iter()
                    .map(|option| option.label.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                format!("等待权限 | {labels}")
            };
            tool.name = name;
            tool.kind = "permission".into();
            tool.summary = summary.clone();
            tool.status = ToolStatus::Running;
            tool.error = None;
            tool.permission_options = options;
            tool.permission_input = input;
            tool.permission_decision = None;
            if let Some(details) = details.filter(|details| !details.trim().is_empty()) {
                tool.detail_text = details.clone();
                tool.raw_input = Some(details.clone());
                push_tool_log(tool, "Plan", collapse_whitespace(&details));
            }
            push_tool_log(tool, "Permission", summary);
            ui.session.status = SessionStatus::WaitingForTool;
        }
        ClientEvent::ToolPermissionResolved { id, outcome } => {
            if let Some(tool) = ui.tools.iter_mut().find(|tool| tool.call_id == id) {
                if permission_resolution_should_preserve_local_reject(
                    tool.permission_decision.as_deref(),
                    &outcome,
                ) {
                    push_tool_log(tool, "Decision", outcome);
                    refresh_session_status(ui);
                    return;
                }
                tool.summary = outcome.clone();
                tool.status = ToolStatus::Succeeded;
                tool.permission_options.clear();
                tool.permission_input = None;
                tool.permission_decision = Some(outcome.clone());
                tool.error = None;
                push_tool_log(tool, "Decision", outcome);
                refresh_session_status(ui);
            }
        }
        ClientEvent::SessionConfigUpdated { mut state } => {
            preserve_local_mode(&mut state, ui.session.mode.as_deref());
            ui.session_config = state;
            sync_session_summary_from_config(ui);
        }
        ClientEvent::PromptCapabilitiesUpdated { capabilities } => {
            ui.prompt_capabilities = capabilities;
        }
        ClientEvent::AvailableCommandsUpdated { commands } => {
            ui.available_commands = commands;
        }
        ClientEvent::SessionTitleUpdated { title } => {
            ui.session.title = title;
        }
        ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id,
            value_label,
        } => {
            apply_config_value_change(ui, &control_id, &value_id, value_label);
            sync_session_summary_from_config(ui);
        }
        ClientEvent::PlanUpdated { entries } => {
            ui.agent_plan = entries;
        }
        ClientEvent::ToolProgress { id, content } => {
            let tool = ensure_tool(ui, &id, None, "tool", "tool", false);
            tool.summary = content.clone();
            tool.status = ToolStatus::Running;
            tool.error = None;
            push_tool_log(tool, "Progress", content);
            ui.session.status = SessionStatus::WaitingForTool;
        }
        ClientEvent::ToolCompleted {
            id,
            name,
            outcome,
            raw_output,
            terminal_output,
        } => {
            let fallback_name = name.as_deref().unwrap_or("tool");
            let display_summary =
                normalize_tool_summary_for_display(ui, &summarize_completion(&outcome));
            let (completed_tool_name, completed_raw_input) = {
                let tool = ensure_tool(ui, &id, None, fallback_name, "tool", false);
                if let Some(name) = name {
                    tool.name = name;
                }
                tool.summary = display_summary;
                tool.status = ToolStatus::Succeeded;
                tool.raw_output = cap_optional_string(raw_output, MAX_TOOL_RAW_OUTPUT_CHARS);
                tool.terminal_output = terminal_output.map(cap_terminal_output);
                tool.error = None;
                push_tool_log(tool, "已完成", outcome.clone());
                (tool.name.clone(), tool.raw_input.clone())
            };
            if completed_tool_name == "TaskCreate" {
                apply_codebuddy_task_create(ui, completed_raw_input.as_deref(), &outcome);
            }
            refresh_session_status(ui);
        }
        ClientEvent::ToolFailed {
            id,
            name,
            error,
            raw_output,
            terminal_output,
        } => {
            let fallback_name = name.as_deref().unwrap_or("tool");
            let tool = ensure_tool(ui, &id, None, fallback_name, "tool", false);
            if let Some(name) = name {
                tool.name = name;
            }
            tool.summary = "工具失败".into();
            tool.status = ToolStatus::Failed;
            tool.raw_output = cap_optional_string(raw_output, MAX_TOOL_RAW_OUTPUT_CHARS);
            tool.terminal_output = terminal_output.map(cap_terminal_output);
            tool.error = Some(error.clone());
            push_tool_log(tool, "错误", error);
            refresh_session_status(ui);
        }
        ClientEvent::ToolDiff {
            id,
            path,
            old_text,
            new_text,
        } => {
            let is_synthetic_write = id.starts_with("fs_write:");
            let normalized_path = normalize_change_path(&path);
            let has_trustworthy_old_text = old_text.as_deref().is_some_and(|text| {
                text.is_empty() || !looks_like_fragment_old_text(text, &new_text)
            });
            let diff_hunks = if is_synthetic_write || has_trustworthy_old_text {
                diff_to_hunks(old_text.as_deref(), &new_text)
            } else {
                Vec::new()
            };
            let is_bogus_whole_file_diff = looks_like_full_file_or_fragment_expansion(&diff_hunks);

            if let Some(tool) = if is_synthetic_write {
                find_recent_tool_for_path(ui, &normalized_path)
            } else {
                Some(ensure_tool(ui, &id, None, "Edit", "edit", false))
            } {
                let path_buf = std::path::PathBuf::from(&path);
                let should_attach_diff = !diff_hunks.is_empty()
                    && (!is_synthetic_write
                        || (has_trustworthy_old_text && !is_bogus_whole_file_diff));
                if should_attach_diff {
                    if !tool.diff_paths.iter().any(|existing| {
                        normalize_change_path(&existing.display().to_string()) == normalized_path
                    }) {
                        tool.diff_paths.push(path_buf.clone());
                    }
                    if let Some(preview) = tool.diff_previews.iter_mut().find(|preview| {
                        normalize_change_path(&preview.path.display().to_string())
                            == normalized_path
                    }) {
                        preview.path = path_buf;
                        let cumulative_hunks = if let Some(baseline) =
                            reverse_apply_diff_hunks(&new_text, &preview.hunks)
                        {
                            diff_to_hunks(Some(&baseline), &new_text)
                        } else {
                            diff_hunks.clone()
                        };
                        preview.hunks = cumulative_hunks;
                    } else {
                        tool.diff_previews.push(ToolDiffPreview {
                            path: path_buf,
                            hunks: diff_hunks.clone(),
                        });
                    }
                }
                if !is_synthetic_write {
                    tool.summary = format!("正在编辑 {path}");
                    if !matches!(tool.status, ToolStatus::Succeeded | ToolStatus::Failed) {
                        tool.status = ToolStatus::Running;
                    }
                    tool.error = None;
                    push_tool_log(tool, "编辑", path.clone());
                    ui.session.status = SessionStatus::WaitingForTool;
                }
            }

            if let Some(changed_file) = ui
                .repository
                .changed_files
                .iter_mut()
                .find(|file| file.path.display().to_string() == path)
            {
                let cumulative_hunks = if let Some(baseline) =
                    reverse_apply_diff_hunks(&new_text, &changed_file.hunks)
                {
                    diff_to_hunks(Some(&baseline), &new_text)
                } else {
                    diff_hunks.clone()
                };
                changed_file.hunks = cumulative_hunks;
            }
        }
        ClientEvent::ToolDiffPreview { id, path, hunks } => {
            let normalized_path = normalize_change_path(&path);
            let path_buf = std::path::PathBuf::from(&path);
            let tool = ensure_tool(ui, &id, None, "Edit", "edit", false);
            if !hunks.is_empty() {
                if !tool.diff_paths.iter().any(|existing| {
                    normalize_change_path(&existing.display().to_string()) == normalized_path
                }) {
                    tool.diff_paths.push(path_buf.clone());
                }
                if let Some(preview) = tool.diff_previews.iter_mut().find(|preview| {
                    normalize_change_path(&preview.path.display().to_string()) == normalized_path
                }) {
                    preview.path = path_buf;
                    preview.hunks = hunks.clone();
                } else {
                    tool.diff_previews.push(ToolDiffPreview {
                        path: path_buf,
                        hunks: hunks.clone(),
                    });
                }
            }
            tool.summary = format!("正在编辑 {path}");
            if !matches!(tool.status, ToolStatus::Succeeded | ToolStatus::Failed) {
                tool.status = ToolStatus::Running;
            }
            tool.error = None;
            push_tool_log(tool, "编辑", path.clone());
            ui.session.status = SessionStatus::WaitingForTool;

            if let Some(changed_file) = ui.repository.changed_files.iter_mut().find(|file| {
                normalize_change_path(&file.path.display().to_string()) == normalized_path
            }) {
                changed_file.hunks = hunks;
            }
        }
        ClientEvent::Interrupted { reason } => {
            ui.agent_plan.clear();
            ui.session.status = workspace_model::SessionStatus::Interrupted;
            for tool in ui
                .tools
                .iter_mut()
                .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
            {
                tool.status = ToolStatus::Interrupted;
                tool.summary = reason.clone();
                tool.error = Some(reason.clone());
                push_tool_log(tool, "已中断", reason.clone());
            }
            ui.inspector_sections.push(SidebarSection {
                title: "中断".into(),
                items: vec![reason],
            });
        }
    }
}

fn push_message(ui: &mut UiSnapshot, role: workspace_model::MessageRole, content: String) {
    if let Some(last_id) = ui.timeline.last().and_then(last_message_id)
        && let Some(last_message) = ui.messages.iter_mut().find(|message| message.id == last_id)
        && last_message.role == role
        && role != workspace_model::MessageRole::User
    {
        last_message.body.push_str(&content);
        return;
    }

    let content = new_message_content(role.clone(), content);
    let Some(content) = content else {
        return;
    };

    let message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role,
        body: content,
        created_at: chrono_now_iso(),
    };
    ui.timeline.push(TimelineItem::Message(message.id));
    ui.messages.push(message);
}

fn push_standalone_message(
    ui: &mut UiSnapshot,
    role: workspace_model::MessageRole,
    content: String,
) {
    let Some(content) = new_message_content(role.clone(), content) else {
        return;
    };

    let message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role,
        body: content,
        created_at: chrono_now_iso(),
    };
    ui.timeline.push(TimelineItem::Message(message.id));
    ui.messages.push(message);
}

fn new_message_content(role: workspace_model::MessageRole, content: String) -> Option<String> {
    if role == workspace_model::MessageRole::User {
        return Some(content);
    }

    if content.trim().is_empty() {
        return None;
    }

    Some(content.trim_start_matches(['\r', '\n']).to_string())
}

fn context_compacted_notice(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        "上下文已自动压缩".into()
    } else {
        message.to_string()
    }
}

fn context_compaction_started_notice(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        "正在压缩上下文".into()
    } else {
        message.to_string()
    }
}

fn is_context_compaction_started_notice(body: &str) -> bool {
    body.trim() == "正在压缩上下文"
}

fn push_context_compaction_notice(ui: &mut UiSnapshot, content: String) {
    if ui.messages.last().is_some_and(|message| {
        message.role == workspace_model::MessageRole::System
            && is_context_compaction_started_notice(&message.body)
    }) {
        return;
    }
    push_standalone_message(ui, workspace_model::MessageRole::System, content);
}

fn push_or_replace_context_compaction_notice(ui: &mut UiSnapshot, content: String) {
    let last_message_id = ui.timeline.last().and_then(|item| match item {
        TimelineItem::Message(id) => Some(*id),
        TimelineItem::Tool(_) | TimelineItem::Thinking => None,
    });
    if let Some(message_id) = last_message_id
        && let Some(message) = ui.messages.iter_mut().find(|message| {
            message.id == message_id
                && message.role == workspace_model::MessageRole::System
                && is_context_compaction_started_notice(&message.body)
        })
    {
        message.body = content;
        return;
    }

    push_standalone_message(ui, workspace_model::MessageRole::System, content);
}

fn is_internal_session_metadata(content: &str) -> bool {
    let content = content.trim();
    content.starts_with("ACP capabilities:") || content.starts_with("Connected to ACP workspace ")
}

fn find_recent_tool_for_path<'a>(
    ui: &'a mut UiSnapshot,
    normalized_path: &str,
) -> Option<&'a mut ToolInvocation> {
    ui.tools.iter_mut().rev().find(|tool| {
        tool.diff_paths
            .iter()
            .any(|path| normalize_change_path(&path.display().to_string()) == normalized_path)
            || tool
                .summary
                .strip_prefix("Editing ")
                .or_else(|| tool.summary.strip_prefix("Edited "))
                .or_else(|| tool.summary.strip_prefix("已编辑 "))
                .map(|path| normalize_change_path(path) == normalized_path)
                .unwrap_or(false)
    })
}

fn looks_like_full_file_or_fragment_expansion(hunks: &[DiffHunk]) -> bool {
    let mut added = 0;
    let mut removed = 0;
    for line in hunks.iter().flat_map(|hunk| &hunk.lines) {
        match line.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    added >= 20 && (removed == 0 || added > removed * 4)
}

fn normalize_change_path(path: &str) -> String {
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

fn normalize_unix_drive_prefix(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    for prefix in ["/mnt/", "/cygdrive/"] {
        if lower.starts_with(prefix) && path.len() > prefix.len() + 1 {
            let drive = path[prefix.len()..].chars().next().unwrap();
            let rest_start = prefix.len() + drive.len_utf8();
            if drive.is_ascii_alphabetic() && path[rest_start..].starts_with('/') {
                return format!("{}:{}", drive.to_ascii_lowercase(), &path[rest_start..]);
            }
        }
    }

    if path.len() > 2 && path.starts_with('/') {
        let mut chars = path.chars();
        let _slash = chars.next();
        if let Some(drive) = chars.next()
            && drive.is_ascii_alphabetic()
            && chars.next() == Some('/')
        {
            let rest_start = 1 + drive.len_utf8();
            return format!("{}:{}", drive.to_ascii_lowercase(), &path[rest_start..]);
        }
    }

    path.to_string()
}

fn workspace_relative_display_path(ui: &UiSnapshot, path: &str) -> String {
    let normalized = normalize_change_path(path);
    let root = normalize_change_path(&ui.workspace.root.display().to_string());
    let root_prefix = if root.ends_with('/') {
        root
    } else {
        format!("{root}/")
    };
    normalized
        .strip_prefix(&root_prefix)
        .unwrap_or(&normalized)
        .to_string()
}

fn normalize_tool_summary_for_display(ui: &UiSnapshot, summary: &str) -> String {
    for prefix in ["Editing ", "Edited ", "已编辑 "] {
        if let Some(path) = summary.strip_prefix(prefix) {
            return format!("{prefix}{}", workspace_relative_display_path(ui, path));
        }
    }
    summary.to_string()
}

fn apply_codebuddy_task_create(ui: &mut UiSnapshot, raw_input: Option<&str>, outcome: &str) {
    let input = raw_input.and_then(parse_json);
    let Some(content) = input
        .as_ref()
        .and_then(task_content_from_input)
        .or_else(|| parse_created_task_content(outcome))
    else {
        return;
    };

    upsert_plan_entry(
        ui,
        AgentPlanEntry {
            id: parse_created_task_id(outcome),
            content,
            priority: AgentPlanEntryPriority::Medium,
            status: AgentPlanEntryStatus::Pending,
        },
    );
}

fn apply_codebuddy_task_update(ui: &mut UiSnapshot, raw_input: Option<&str>) {
    let Some(input) = raw_input.and_then(parse_json) else {
        return;
    };
    let Some(task_id) = json_string(&input, "taskId") else {
        return;
    };
    let Some(status) =
        json_string(&input, "status").and_then(|status| plan_status_from_task(&status))
    else {
        return;
    };

    if let Some(entry) = ui
        .agent_plan
        .iter_mut()
        .find(|entry| entry.id.as_deref() == Some(task_id.as_str()))
    {
        entry.status = status;
        return;
    }

    let mut entry = find_created_task_entry(ui, &task_id)
        .or_else(|| {
            task_content_from_input(&input).map(|content| AgentPlanEntry {
                id: Some(task_id.clone()),
                content,
                priority: AgentPlanEntryPriority::Medium,
                status: AgentPlanEntryStatus::Pending,
            })
        })
        .unwrap_or_else(|| AgentPlanEntry {
            id: Some(task_id.clone()),
            content: format!("任务 #{task_id}"),
            priority: AgentPlanEntryPriority::Medium,
            status: AgentPlanEntryStatus::Pending,
        });
    entry.status = status;
    upsert_plan_entry(ui, entry);
}

fn is_codebuddy_todo_write_tool(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    normalized == "todowrite"
        || normalized == "todo write"
        || normalized == "todo: todo write"
        || normalized.contains("todo write")
}

fn apply_codebuddy_todo_write(ui: &mut UiSnapshot, raw_input: Option<&str>) {
    let Some(input) = raw_input.and_then(parse_json) else {
        return;
    };
    let Some(content) = json_string(&input, "content") else {
        return;
    };
    let entries = parse_markdown_todo_entries(&content);
    if !entries.is_empty() {
        ui.agent_plan = entries;
    }
}

fn parse_markdown_todo_entries(content: &str) -> Vec<AgentPlanEntry> {
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_markdown_todo_entry(index, line))
        .collect()
}

fn parse_markdown_todo_entry(index: usize, line: &str) -> Option<AgentPlanEntry> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- [")
        .or_else(|| trimmed.strip_prefix("* ["))?;
    let (marker, text) = rest.split_once(']')?;
    let text = text
        .trim_start_matches(|ch: char| ch == ' ' || ch == '\t' || ch == '-' || ch == ':')
        .trim();
    if text.is_empty() {
        return None;
    }

    let status = match marker.trim().to_ascii_lowercase().as_str() {
        "" | " " => AgentPlanEntryStatus::Pending,
        "x" | "✓" | "done" | "completed" => AgentPlanEntryStatus::Completed,
        "-" | "~" | "/" | ">" | "in_progress" | "running" => AgentPlanEntryStatus::InProgress,
        "cancelled" | "canceled" => AgentPlanEntryStatus::Cancelled,
        _ => AgentPlanEntryStatus::Pending,
    };

    Some(AgentPlanEntry {
        id: Some(format!("todo-{index}")),
        content: text.to_string(),
        priority: AgentPlanEntryPriority::Medium,
        status,
    })
}

fn find_created_task_entry(ui: &UiSnapshot, task_id: &str) -> Option<AgentPlanEntry> {
    ui.tools.iter().rev().find_map(|tool| {
        let raw_output = tool.raw_output.as_deref().map(task_output_text);
        let id = raw_output.as_deref().and_then(parse_created_task_id)?;
        if id != task_id {
            return None;
        }

        let content = tool
            .raw_input
            .as_deref()
            .and_then(parse_json)
            .and_then(|input| task_content_from_input(&input))
            .or_else(|| raw_output.as_deref().and_then(parse_created_task_content))?;

        Some(AgentPlanEntry {
            id: Some(id),
            content,
            priority: AgentPlanEntryPriority::Medium,
            status: AgentPlanEntryStatus::Pending,
        })
    })
}

fn upsert_plan_entry(ui: &mut UiSnapshot, next: AgentPlanEntry) {
    if let Some(id) = next.id.as_deref() {
        if let Some(entry) = ui
            .agent_plan
            .iter_mut()
            .find(|entry| entry.id.as_deref() == Some(id))
        {
            *entry = next;
            return;
        }
    } else if ui
        .agent_plan
        .iter()
        .any(|entry| entry.id.is_none() && entry.content == next.content)
    {
        return;
    }

    ui.agent_plan.push(next);
}

fn parse_json(raw: &str) -> Option<Value> {
    serde_json::from_str(raw).ok()
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn task_content_from_input(input: &Value) -> Option<String> {
    json_string(input, "subject")
        .or_else(|| json_string(input, "activeForm"))
        .or_else(|| json_string(input, "content"))
        .or_else(|| json_string(input, "title"))
        .or_else(|| json_string(input, "name"))
        .or_else(|| json_string(input, "description"))
}

fn task_output_text(raw_output: &str) -> String {
    parse_json(raw_output)
        .and_then(|value| json_text_payload(&value))
        .unwrap_or_else(|| raw_output.to_string())
}

fn json_text_payload(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_string()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(json_text_payload)
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| map.get("content").and_then(json_text_payload)),
        _ => None,
    }
}

fn parse_created_task_id(outcome: &str) -> Option<String> {
    let (_, rest) = outcome.split_once("Task #")?;
    let id = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect::<String>();
    (!id.is_empty()).then_some(id)
}

fn parse_created_task_content(outcome: &str) -> Option<String> {
    outcome
        .split_once("created successfully:")
        .map(|(_, content)| content.trim())
        .filter(|content| !content.is_empty())
        .map(str::to_string)
}

fn plan_status_from_task(status: &str) -> Option<AgentPlanEntryStatus> {
    match status {
        "pending" => Some(AgentPlanEntryStatus::Pending),
        "in_progress" | "running" => Some(AgentPlanEntryStatus::InProgress),
        "completed" | "done" => Some(AgentPlanEntryStatus::Completed),
        "cancelled" | "canceled" => Some(AgentPlanEntryStatus::Cancelled),
        _ => None,
    }
}

fn apply_config_value_change(
    ui: &mut UiSnapshot,
    control_id: &str,
    value_id: &str,
    value_label: Option<String>,
) {
    let Some(control) = ui
        .session_config
        .controls
        .iter_mut()
        .find(|control| control.id == control_id)
    else {
        if control_id == "mode" {
            ui.session.mode = Some(value_label.unwrap_or_else(|| value_id.to_string()));
        }
        return;
    };

    let label = value_label.unwrap_or_else(|| {
        control
            .choices
            .iter()
            .find(|choice| choice.id == value_id)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| value_id.to_string())
    });
    control.current_value_id = value_id.to_string();
    control.current_value_label = label;
}

fn preserve_local_mode(
    state: &mut workspace_model::SessionConfigState,
    current_mode: Option<&str>,
) {
    let Some(current_mode) = current_mode else {
        return;
    };

    for control in &mut state.controls {
        if control.category != workspace_model::SessionConfigCategory::Mode
            || control.source != workspace_model::SessionConfigSource::LocalMode
        {
            continue;
        }

        if let Some(choice) = control.choices.iter().find(|choice| {
            choice.id.eq_ignore_ascii_case(current_mode)
                || choice.label.eq_ignore_ascii_case(current_mode)
        }) {
            control.current_value_id = choice.id.clone();
            control.current_value_label = choice.label.clone();
        }
    }
}

fn sync_session_summary_from_config(ui: &mut UiSnapshot) {
    for control in &ui.session_config.controls {
        match control.category {
            workspace_model::SessionConfigCategory::Model => {
                ui.session.model = control.current_value_label.clone();
            }
            workspace_model::SessionConfigCategory::Mode => {
                ui.session.mode = Some(control.current_value_label.clone());
            }
            _ => {}
        }
    }
}

fn ensure_tool<'a>(
    ui: &'a mut UiSnapshot,
    call_id: &str,
    parent_call_id: Option<String>,
    name: &str,
    kind: &str,
    is_subagent: bool,
) -> &'a mut ToolInvocation {
    if let Some(index) = ui.tools.iter().position(|tool| tool.call_id == call_id) {
        if let Some(parent_call_id) = parent_call_id.clone() {
            ui.tools[index].parent_call_id = Some(parent_call_id);
        }
        if is_subagent {
            ui.tools[index].is_subagent = true;
        }
        return &mut ui.tools[index];
    }

    let tool = ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: call_id.to_string(),
        parent_call_id,
        name: name.to_string(),
        kind: kind.to_string(),
        summary: "等待活动".into(),
        status: ToolStatus::Pending,
        is_subagent,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: None,
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_input: None,
        permission_decision: None,
    };
    let id = tool.id;
    ui.tools.push(tool);
    ui.timeline.push(TimelineItem::Tool(id));

    ui.tools.last_mut().expect("tool should exist")
}

fn refresh_session_status(ui: &mut UiSnapshot) {
    ui.session.status = if ui
        .tools
        .iter()
        .any(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        SessionStatus::WaitingForTool
    } else {
        SessionStatus::Streaming
    };
}

fn finalize_running_children(
    ui: &mut UiSnapshot,
    parent_call_id: &str,
    except_call_id: Option<&str>,
) {
    for tool in ui.tools.iter_mut().filter(|tool| {
        tool.parent_call_id.as_deref() == Some(parent_call_id)
            && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            && except_call_id != Some(tool.call_id.as_str())
    }) {
        tool.status = ToolStatus::Succeeded;
        if tool.summary.trim().is_empty() || tool.summary == "等待活动" {
            tool.summary = "已完成".into();
        }
        push_tool_log(tool, "已完成", "根据后续父活动推断");
    }
}

fn finalize_open_tools(ui: &mut UiSnapshot, stop_reason: &str) {
    let normal_finish = stop_reason == "end_turn";
    let cancelled = stop_reason == "cancelled";
    for tool in ui
        .tools
        .iter_mut()
        .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        if tool.error.is_some() {
            tool.status = ToolStatus::Failed;
            continue;
        }

        tool.status = if normal_finish {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Interrupted
        };
        if tool.summary.trim().is_empty()
            || tool.summary == "等待活动"
            || tool.summary.starts_with("等待权限")
        {
            tool.summary = if cancelled {
                "已取消".into()
            } else if normal_finish {
                "已完成".into()
            } else {
                format!("异常结束：{stop_reason}")
            };
        }
        if tool.kind == "permission" {
            tool.permission_options.clear();
            if tool.permission_decision.is_none() {
                let decision = if cancelled {
                    "已取消"
                } else if normal_finish {
                    "已完成"
                } else {
                    "已中断"
                };
                tool.permission_decision = Some(decision.into());
            }
        }
        if !normal_finish {
            tool.error
                .get_or_insert_with(|| format!("轮次异常结束：{stop_reason}"));
        }
        let label = if cancelled {
            "已取消"
        } else if normal_finish {
            "已完成"
        } else {
            "已中断"
        };
        push_tool_log(tool, label, format!("轮次结束：{stop_reason}"));
    }
}

fn summarize_completion(outcome: &str) -> String {
    let compact = collapse_whitespace(outcome);
    if compact.is_empty() {
        "已完成".into()
    } else if compact.chars().count() > 120 {
        let truncated: String = compact.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        compact
    }
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn permission_resolution_should_preserve_local_reject(
    current_decision: Option<&str>,
    next_outcome: &str,
) -> bool {
    let Some(current_decision) = current_decision else {
        return false;
    };
    let current = current_decision.to_ascii_lowercase();
    if !current.contains("reject") && !current.contains("deny") {
        return false;
    }
    let next = next_outcome.to_ascii_lowercase();
    next.contains("permission resolved") && (next.contains("allow") || next.contains("default"))
}

fn looks_like_fragment_old_text(old_text: &str, new_text: &str) -> bool {
    let old_lines = old_text.lines().count();
    let new_lines = new_text.lines().count();
    old_lines > 0 && new_lines >= 20 && old_lines * 4 < new_lines
}

fn push_tool_log(tool: &mut ToolInvocation, title: impl Into<String>, body: impl Into<String>) {
    let body = cap_string(body.into(), MAX_TOOL_LOG_CHARS);
    if body.is_empty() {
        return;
    }

    if tool.logs.last().map(|entry| entry.body.as_str()) == Some(body.as_str()) {
        return;
    }

    tool.logs.push(ToolLogEntry {
        title: title.into(),
        body,
    });

    if tool.logs.len() > 12 {
        let keep_from = tool.logs.len() - 12;
        tool.logs.drain(0..keep_from);
    }
}

fn cap_optional_string(value: Option<String>, max_chars: usize) -> Option<String> {
    value.map(|value| cap_string(value, max_chars))
}

fn cap_tool_raw_input(value: Option<String>) -> Option<String> {
    value.map(|value| cap_structured_tool_raw_input(value, MAX_TOOL_RAW_INPUT_CHARS))
}

fn cap_structured_tool_raw_input(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(&value) else {
        return cap_string(value, max_chars);
    };
    let Some(object) = parsed.as_object() else {
        return cap_string(value, max_chars);
    };

    let mut retained = Map::new();
    for key in TOOL_RAW_INPUT_PRIORITY_KEYS {
        if let Some(field) = object.get(*key) {
            retained.insert((*key).to_string(), cap_json_value(field.clone(), 2048));
        }
    }

    for (key, field) in object {
        if retained.contains_key(key) {
            continue;
        }
        if should_keep_tool_raw_input_field(field) {
            retained.insert(key.clone(), cap_json_value(field.clone(), 1024));
        }
    }

    retained.insert("_truncated".into(), Value::Bool(true));
    retained.insert(
        "_omittedChars".into(),
        Value::Number(serde_json::Number::from(
            value.chars().count().saturating_sub(max_chars),
        )),
    );

    let serialized = serde_json::to_string(&Value::Object(retained));
    let Ok(serialized) = serialized else {
        return cap_string(value, max_chars);
    };
    if serialized.chars().count() <= max_chars {
        return serialized;
    }

    let mut compact = Map::new();
    for key in TOOL_RAW_INPUT_PRIORITY_KEYS {
        if let Some(field) = object.get(*key) {
            compact.insert((*key).to_string(), cap_json_value(field.clone(), 256));
        }
    }
    compact.insert("_truncated".into(), Value::Bool(true));
    serde_json::to_string(&Value::Object(compact))
        .ok()
        .filter(|serialized| serialized.chars().count() <= max_chars)
        .unwrap_or_else(|| cap_string(value, max_chars))
}

const TOOL_RAW_INPUT_PRIORITY_KEYS: &[&str] = &[
    "description",
    "command",
    "cmd",
    "shell_command",
    "command_line",
    "args",
    "file_path",
    "filePath",
    "path",
    "pattern",
    "include",
    "url",
    "query",
    "prompt",
    "old_string",
    "oldString",
    "new_string",
    "newString",
    "before",
    "after",
    "oldText",
    "newText",
    "replacement",
    "parent_tool_call_id",
    "subagent_type",
];

fn should_keep_tool_raw_input_field(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => text.chars().count() <= 512,
        Value::Array(items) => {
            items.len() <= 16
                && items.iter().all(|item| {
                    matches!(item, Value::String(_) | Value::Number(_) | Value::Bool(_))
                })
        }
        Value::Object(_) => false,
    }
}

fn cap_json_value(value: Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => Value::String(cap_string(text, max_chars)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(16)
                .map(|item| cap_json_value(item, max_chars / 2))
                .collect(),
        ),
        other => other,
    }
}

fn cap_terminal_output(mut output: TerminalOutput) -> TerminalOutput {
    output.output = cap_string(output.output, MAX_TOOL_RAW_OUTPUT_CHARS);
    output
}

fn cap_string_in_place(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }

    let omitted = value.chars().count() - max_chars;
    let suffix = value
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    *value = format!("[... omitted {omitted} chars ...]\n{suffix}");
}

fn cap_string(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }

    let truncated = value.chars().take(max_chars).collect::<String>();
    let omitted = value.chars().count() - max_chars;
    format!("{truncated}\n[... omitted {omitted} chars ...]")
}

fn last_message_id(item: &TimelineItem) -> Option<uuid::Uuid> {
    match item {
        TimelineItem::Message(id) => Some(*id),
        TimelineItem::Tool(_) | TimelineItem::Thinking => None,
    }
}

fn chrono_now_iso() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO 8601 timestamp without chrono dependency
    format!("{now}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use workspace_model::{
        InspectorTab, MessageRole, PermissionOption, RepositorySnapshot, SessionStatus,
        SessionSummary, WorkspaceDescriptor, WorkspaceLocation,
    };

    fn empty_ui() -> UiSnapshot {
        let workspace_id = uuid::Uuid::new_v4();
        UiSnapshot {
            revision: 1,
            workspace: WorkspaceDescriptor {
                id: workspace_id,
                name: "test".into(),
                root: PathBuf::from("/test"),
                location: WorkspaceLocation::Local,
            },
            session: SessionSummary {
                id: uuid::Uuid::new_v4(),
                workspace_id,
                title: "test".into(),
                model: "test".into(),
                mode: None,
                agent_cli: None,
                status: SessionStatus::Idle,
            },
            session_config: Default::default(),
            prompt_capabilities: Default::default(),
            available_commands: Vec::new(),
            agent_plan: Vec::new(),
            messages: Vec::new(),
            timeline: Vec::new(),
            tools: Vec::new(),
            repository: RepositorySnapshot {
                branch: "main".into(),
                head: "abc".into(),
                changed_files: Vec::new(),
            },
            inspector_tab: InspectorTab::Activity,
            inspector_sections: Vec::new(),
            session_changes: Vec::new(),
            review_changes: Vec::new(),
            turn_changes: Vec::new(),
            thinking_status: None,
        }
    }

    #[test]
    fn ignores_blank_assistant_chunks_that_would_create_rows() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "checking".into(),
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "tool-1".into(),
                parent_id: None,
                name: "Read".into(),
                kind: "read".into(),
                summary: "Read file".into(),
                is_subagent: false,
                raw_input: None,
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "\n\n".into(),
            },
        );

        assert_eq!(ui.messages.len(), 1);
        assert!(matches!(
            ui.timeline.as_slice(),
            [TimelineItem::Message(_), TimelineItem::Tool(_)]
        ));

        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "\n\ndone".into(),
            },
        );

        assert_eq!(ui.messages.len(), 2);
        assert_eq!(ui.messages[1].body, "done");
    }

    #[test]
    fn preserves_blank_lines_inside_an_active_assistant_message() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "first".into(),
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "\n\nsecond".into(),
            },
        );

        assert_eq!(ui.messages.len(), 1);
        assert_eq!(ui.messages[0].body, "first\n\nsecond");
    }

    #[test]
    fn context_compaction_notice_is_a_standalone_system_message() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::System,
                content: "普通系统消息".into(),
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::ContextCompacted {
                message: "上下文已压缩".into(),
            },
        );

        assert_eq!(ui.messages.len(), 2);
        assert_eq!(ui.messages[1].role, MessageRole::System);
        assert_eq!(ui.messages[1].body, "上下文已压缩");
        assert!(matches!(
            ui.timeline.as_slice(),
            [TimelineItem::Message(_), TimelineItem::Message(_)]
        ));
    }

    #[test]
    fn context_compaction_started_message_is_replaced_when_completed() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::ContextCompactionStarted {
                message: "正在压缩上下文".into(),
            },
        );
        assert_eq!(ui.messages.len(), 1);
        assert_eq!(ui.messages[0].body, "正在压缩上下文");

        apply_event(
            &mut ui,
            ClientEvent::ContextCompacted {
                message: "上下文已自动压缩".into(),
            },
        );

        assert_eq!(ui.messages.len(), 1);
        assert_eq!(ui.messages[0].role, MessageRole::System);
        assert_eq!(ui.messages[0].body, "上下文已自动压缩");
        assert!(matches!(ui.timeline.as_slice(), [TimelineItem::Message(_)]));
    }

    #[test]
    fn cancelled_turn_resolves_pending_permission_tool() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::ToolPermissionRequest {
                id: "call_bash".into(),
                name: "Bash".into(),
                options: vec![
                    PermissionOption {
                        id: "approved".into(),
                        label: "Approved".into(),
                        kind: "AllowOnce".into(),
                    },
                    PermissionOption {
                        id: "abort".into(),
                        label: "Abort".into(),
                        kind: "RejectOnce".into(),
                    },
                ],
                details: Some("Command: python3 -c ...".into()),
                input: None,
            },
        );
        assert_eq!(ui.session.status, SessionStatus::WaitingForTool);

        apply_event(
            &mut ui,
            ClientEvent::TurnFinished {
                stop_reason: "cancelled".into(),
            },
        );

        let tool = ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "call_bash")
            .expect("permission tool should exist");
        assert_eq!(ui.session.status, SessionStatus::Idle);
        assert_eq!(tool.status, ToolStatus::Interrupted);
        assert_eq!(tool.summary, "已取消");
        assert!(tool.permission_options.is_empty());
        assert_eq!(tool.permission_decision.as_deref(), Some("已取消"));
        assert_eq!(tool.error.as_deref(), Some("轮次异常结束：cancelled"));
    }

    #[test]
    fn permission_resolved_allow_does_not_override_local_reject() {
        let mut ui = empty_ui();

        apply_event(
            &mut ui,
            ClientEvent::ToolPermissionRequest {
                id: "call_bash".into(),
                name: "Bash".into(),
                options: Vec::new(),
                details: None,
                input: None,
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::ToolPermissionResolved {
                id: "call_bash".into(),
                outcome: "Permission selected: Reject".into(),
            },
        );
        apply_event(
            &mut ui,
            ClientEvent::ToolPermissionResolved {
                id: "call_bash".into(),
                outcome: "Permission resolved: allow".into(),
            },
        );

        let tool = ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "call_bash")
            .expect("permission tool should exist");
        assert_eq!(tool.summary, "Permission selected: Reject");
        assert_eq!(
            tool.permission_decision.as_deref(),
            Some("Permission selected: Reject")
        );
        assert!(
            tool.logs
                .iter()
                .any(|entry| entry.body == "Permission resolved: allow")
        );
    }

    #[test]
    fn caps_tool_raw_input_without_losing_structured_title_fields() {
        let raw_input = serde_json::json!({
            "content": "x".repeat(MAX_TOOL_RAW_INPUT_CHARS + 2048),
            "file_path": "openspec/changes/accelerate-pipeline-execution/specs/pipeline-execution/spec.md",
            "command": "openspec status --change \"accelerate-pipeline-execution\" --json",
            "description": "Check OpenSpec status",
        })
        .to_string();

        let capped = cap_tool_raw_input(Some(raw_input)).expect("raw input should be retained");
        assert!(capped.len() < MAX_TOOL_RAW_INPUT_CHARS);

        let parsed: Value = serde_json::from_str(&capped).expect("capped raw input stays JSON");
        assert_eq!(
            parsed.get("file_path").and_then(Value::as_str),
            Some("openspec/changes/accelerate-pipeline-execution/specs/pipeline-execution/spec.md")
        );
        assert_eq!(
            parsed.get("command").and_then(Value::as_str),
            Some("openspec status --change \"accelerate-pipeline-execution\" --json")
        );
        assert_eq!(
            parsed.get("description").and_then(Value::as_str),
            Some("Check OpenSpec status")
        );
        assert_eq!(
            parsed.get("_truncated").and_then(Value::as_bool),
            Some(true)
        );
        assert!(parsed.get("content").is_none());
    }
}
