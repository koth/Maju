use acp_core::{ClientEvent, diff_to_hunks};
use serde_json::Value;
use workspace_model::{
    AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, ChatMessage, DiffHunk,
    DiffLineKind, FileChangeType, SessionFileChange, SessionStatus, SidebarSection, TerminalOutput,
    ThinkingStatus, TimelineItem, ToolDiffPreview, ToolInvocation, ToolLogEntry, ToolStatus,
    UiSnapshot,
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
            ui.inspector_sections.push(SidebarSection {
                title: "轮次结果".into(),
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
            let tool = ensure_tool(ui, &id, parent_id, &name, &kind, is_subagent);
            tool.name = name;
            tool.kind = kind;
            tool.summary = summary.clone();
            tool.status = ToolStatus::Running;
            tool.is_subagent = is_subagent;
            tool.raw_input = cap_optional_string(raw_input, MAX_TOOL_RAW_INPUT_CHARS);
            tool.raw_output = None;
            tool.terminal_output = None;
            tool.error = None;
            push_tool_log(tool, "Requested", summary);
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
                if let Some(summary) = summary {
                    tool.summary = summary.clone();
                    if !is_partial {
                        push_tool_log(tool, "Update", summary);
                    }
                }
                if let Some(raw_input) = cap_optional_string(raw_input, MAX_TOOL_RAW_INPUT_CHARS) {
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
        ClientEvent::ToolPermissionRequest { id, name, options } => {
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
            tool.permission_decision = None;
            push_tool_log(tool, "Permission", summary);
            ui.session.status = SessionStatus::WaitingForTool;
        }
        ClientEvent::ToolPermissionResolved { id, outcome } => {
            let tool = ensure_tool(ui, &id, None, "Permission", "permission", false);
            tool.summary = outcome.clone();
            tool.status = ToolStatus::Succeeded;
            tool.permission_options.clear();
            tool.permission_decision = Some(outcome.clone());
            tool.error = None;
            push_tool_log(tool, "Decision", outcome);
            refresh_session_status(ui);
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
            let (completed_tool_name, completed_raw_input) = {
                let tool = ensure_tool(ui, &id, None, fallback_name, "tool", false);
                if let Some(name) = name {
                    tool.name = name;
                }
                tool.summary = summarize_completion(&outcome);
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
            let has_trustworthy_old_text = old_text.as_deref().map_or(false, |text| {
                !text.is_empty() && !looks_like_fragment_old_text(text, &new_text)
            });
            let diff_hunks = if is_synthetic_write || has_trustworthy_old_text {
                diff_to_hunks(old_text.as_deref(), &new_text)
            } else {
                Vec::new()
            };
            let has_existing_preview_for_path = ui_has_tool_preview_for_path(ui, &normalized_path);
            let is_bogus_whole_file_diff = looks_like_full_file_or_fragment_expansion(&diff_hunks);
            let is_synthetic_full_file_fallback = is_synthetic_write
                && old_text.as_deref().map_or(true, |text| {
                    text.is_empty() || looks_like_fragment_old_text(text, &new_text)
                })
                && has_existing_preview_for_path
                && is_bogus_whole_file_diff;

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
                        preview.hunks = diff_hunks.clone();
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
                changed_file.hunks = diff_hunks.clone();
            }

            if is_synthetic_write && !is_synthetic_full_file_fallback {
                upsert_session_change(ui, path, old_text, new_text);
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

    let message = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role,
        body: content,
    };
    ui.timeline.push(TimelineItem::Message(message.id));
    ui.messages.push(message);
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
                .map(|path| normalize_change_path(path) == normalized_path)
                .unwrap_or(false)
    })
}

fn tool_has_preview_for_path(tool: &ToolInvocation, normalized_path: &str) -> bool {
    tool.diff_previews.iter().any(|preview| {
        normalize_change_path(&preview.path.display().to_string()) == normalized_path
            && preview.hunks.iter().any(|hunk| {
                hunk.lines
                    .iter()
                    .any(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
            })
    })
}

fn ui_has_tool_preview_for_path(ui: &UiSnapshot, normalized_path: &str) -> bool {
    ui.tools
        .iter()
        .any(|tool| tool_has_preview_for_path(tool, normalized_path))
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

fn upsert_session_change(
    ui: &mut UiSnapshot,
    path: String,
    old_text: Option<String>,
    new_text: String,
) {
    let normalized_path = normalize_change_path(&path);
    let normalized_old_text = old_text
        .as_deref()
        .map(normalize_diff_text_for_session_change);
    let normalized_new_text = normalize_diff_text_for_session_change(&new_text);
    let incoming_change_type = if old_text.is_none() {
        FileChangeType::Created
    } else {
        FileChangeType::Modified
    };

    if let Some(index) = ui
        .session_changes
        .iter()
        .position(|change| normalize_change_path(&change.path) == normalized_path)
    {
        let baseline = ui.session_changes[index]
            .old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change)
            .or_else(|| normalized_old_text.clone())
            .unwrap_or_default();
        if baseline == normalized_new_text {
            ui.session_changes.remove(index);
            return;
        }

        let existing = &mut ui.session_changes[index];
        if existing.old_text.is_none() && normalized_old_text.is_some() {
            existing.old_text = normalized_old_text;
            existing.change_type = incoming_change_type;
        }
        existing.new_text = normalized_new_text;
        existing.path = normalized_path;
        existing.timestamp = chrono_now_iso();
        refresh_change_stats(existing);
        if existing.added_lines == 0 && existing.removed_lines == 0 {
            ui.session_changes.remove(index);
        }
        return;
    }

    if normalized_old_text.as_deref().unwrap_or_default() == normalized_new_text {
        return;
    }

    let mut change = SessionFileChange {
        path: normalized_path,
        change_type: incoming_change_type,
        old_text: normalized_old_text,
        new_text: normalized_new_text,
        added_lines: 0,
        removed_lines: 0,
        timestamp: chrono_now_iso(),
    };
    refresh_change_stats(&mut change);
    if change.added_lines > 0 || change.removed_lines > 0 {
        ui.session_changes.push(change);
    }
}

fn refresh_change_stats(change: &mut SessionFileChange) {
    let hunks = diff_to_hunks(change.old_text.as_deref(), &change.new_text);
    change.added_lines = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    change.removed_lines = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count();
}

fn normalize_diff_text_for_session_change(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn normalize_change_path(path: &str) -> String {
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
    for tool in ui
        .tools
        .iter_mut()
        .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        if tool.error.is_some() {
            tool.status = ToolStatus::Failed;
            continue;
        }

        tool.status = if stop_reason == "cancelled" {
            ToolStatus::Interrupted
        } else {
            ToolStatus::Succeeded
        };
        if tool.summary.trim().is_empty() || tool.summary == "等待活动" {
            tool.summary = if stop_reason == "cancelled" {
                "已取消".into()
            } else {
                "已完成".into()
            };
        }
        let label = if stop_reason == "cancelled" {
            "已取消"
        } else {
            "已完成"
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
