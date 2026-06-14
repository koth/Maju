use super::*;

mod diff_preview;

use diff_preview::emit_codebuddy_diff_content;
pub(super) use diff_preview::emit_tool_diff_previews_from_raw_output;
#[cfg(test)]
pub(super) use diff_preview::{edit_preview_new_text_from_raw_input, normalize_unix_drive_prefix};

pub(super) fn emit_codebuddy_notification(
    tx: &mpsc::Sender<ClientEvent>,
    workspace_root: &str,
    payload: &Value,
) -> anyhow::Result<bool> {
    let Some(update) = payload.get("update") else {
        return Ok(false);
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return Ok(false);
    };

    match kind {
        "tool_call" => {
            emit_codebuddy_tool_call(tx, workspace_root, update)?;
            Ok(true)
        }
        "tool_call_update" => {
            emit_codebuddy_tool_call_update(tx, workspace_root, update)?;
            Ok(true)
        }
        "current_mode_update" => {
            emit_codebuddy_current_mode_update(tx, update)?;
            Ok(true)
        }
        "agent_message_chunk" => {
            emit_codebuddy_agent_chunk(tx, update)?;
            Ok(true)
        }
        "session_info_update" => {
            emit_codebuddy_session_info(tx, update)?;
            Ok(true)
        }
        "available_commands_update" => {
            emit_codebuddy_available_commands(tx, update)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(super) fn emit_kodex_notification(
    tx: &mpsc::Sender<ClientEvent>,
    payload: &Value,
) -> anyhow::Result<bool> {
    if let Some(context_compaction) = payload
        .get("_meta")
        .and_then(|meta| meta.get(KODEX_CONTEXT_COMPACTION_META_KEY))
        .or_else(|| {
            payload
                .get("update")
                .and_then(|update| update.get("_meta"))
                .and_then(|meta| meta.get(KODEX_CONTEXT_COMPACTION_META_KEY))
        })
    {
        let phase = context_compaction
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        let default_message = if phase == "started" {
            "正在压缩上下文"
        } else {
            "上下文已自动压缩"
        };
        let message = context_compaction
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| context_compaction.as_str())
            .filter(|message| !message.trim().is_empty())
            .unwrap_or(default_message)
            .to_string();

        if phase == "started" {
            tx.send(ClientEvent::ContextCompactionStarted { message })
                .map_err(|_| anyhow!("failed to emit context compaction start notice"))?;
        } else {
            tx.send(ClientEvent::ContextCompacted { message })
                .map_err(|_| anyhow!("failed to emit context compaction notice"))?;
        }
        return Ok(true);
    }

    let Some(context_compacted) = payload
        .get("_meta")
        .and_then(|meta| meta.get(KODEX_CONTEXT_COMPACTED_META_KEY))
        .or_else(|| {
            payload
                .get("update")
                .and_then(|update| update.get("_meta"))
                .and_then(|meta| meta.get(KODEX_CONTEXT_COMPACTED_META_KEY))
        })
    else {
        return Ok(false);
    };

    let message = context_compacted
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| context_compacted.as_str())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("上下文已压缩")
        .to_string();

    tx.send(ClientEvent::ContextCompacted { message })
        .map_err(|_| anyhow!("failed to emit context compaction notice"))?;
    Ok(true)
}

fn emit_codebuddy_tool_call(
    tx: &mpsc::Sender<ClientEvent>,
    workspace_root: &str,
    update: &Value,
) -> anyhow::Result<()> {
    let id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let parent_id = codebuddy_parent_tool_call_id(update);
    let is_subagent = codebuddy_is_subagent(update);
    let status = tool_call_status(update);
    let name = tool_display_name(update);
    let kind = tool_kind_label(update);
    let summary = tool_summary(update, &name);
    let raw_input = tool_raw_input_for_ui(update);
    let raw_output_fallback = codebuddy_raw_output_fallback(update);
    let raw_output = update.get("rawOutput").or(raw_output_fallback.as_ref());
    let effective_status = effective_codebuddy_tool_status(update, status.as_deref(), raw_output);

    tx.send(ClientEvent::ToolStarted {
        id: id.clone(),
        parent_id,
        name: name.clone(),
        kind,
        summary: summary.clone(),
        is_subagent,
        raw_input: raw_input.as_ref().map(format_json),
    })
    .map_err(|_| anyhow!("failed to emit CodeBuddy tool start"))?;

    emit_codebuddy_diff_content(tx, workspace_root, &id, update)?;
    emit_codebuddy_text_content(tx, &id, update)?;
    emit_tool_diff_previews_from_raw_output(tx, &id, raw_output)?;
    emit_codebuddy_plan_from_raw_response(tx, update)?;

    match effective_status.as_deref() {
        Some("completed") => {
            let terminal_output = raw_output.and_then(parse_terminal_output);
            tx.send(ClientEvent::ToolCompleted {
                id,
                name: Some(name),
                outcome: summary,
                raw_output: raw_output.map(format_value_for_ui),
                terminal_output,
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy inline tool completion"))?;
        }
        Some("failed") => {
            let terminal_output = raw_output.and_then(parse_terminal_output);
            let error = codebuddy_hard_error_text(update, raw_output)
                .or_else(|| raw_output.map(format_value_for_ui))
                .unwrap_or_else(|| "Tool call failed".into());
            tx.send(ClientEvent::ToolFailed {
                id,
                name: Some(name),
                error,
                raw_output: raw_output.map(format_value_for_ui),
                terminal_output,
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy inline tool failure"))?;
        }
        _ => {}
    }

    if let Some(mode_id) = codebuddy_tool_policy_mode(update, effective_status.as_deref(), false) {
        emit_policy_mode_change(tx, mode_id)?;
    }

    Ok(())
}

fn emit_codebuddy_tool_call_update(
    tx: &mpsc::Sender<ClientEvent>,
    workspace_root: &str,
    update: &Value,
) -> anyhow::Result<()> {
    let id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let parent_id = codebuddy_parent_tool_call_id(update);
    let is_subagent = codebuddy_is_subagent(update);
    let status = tool_call_status(update);
    let is_partial = status.is_none() && !is_stable_claude_tool_metadata_update(update);

    // For partial (streaming) updates, rawInput is incomplete (e.g. {"file_path": "d:/wor"}).
    // Claude Code also sends complete metadata-only updates without a status field; keep those
    // so later file paths and titles can replace the generic initial tool card.
    let name = if is_partial {
        None
    } else {
        tool_update_display_name(update)
    };
    let kind = if is_partial {
        None
    } else {
        Some(tool_kind_label(update))
    };
    let summary = if is_partial {
        None
    } else {
        let n = name.as_deref().unwrap_or("tool");
        let s = tool_summary(update, n);
        (!s.is_empty()).then_some(s)
    };
    let raw_output_fallback = codebuddy_raw_output_fallback(update);
    let raw_output = update
        .get("rawOutput")
        .or_else(|| {
            update
                .get("fields")
                .and_then(|fields| fields.get("rawOutput"))
        })
        .or(raw_output_fallback.as_ref());
    let effective_status = effective_codebuddy_tool_status(update, status.as_deref(), raw_output);
    let terminal_output = raw_output.and_then(parse_terminal_output);
    // Only send rawInput on final updates to avoid sending incomplete JSON fragments
    let raw_input = if is_partial {
        None
    } else {
        tool_raw_input_for_ui(update)
    };

    if !is_partial {
        emit_codebuddy_diff_content(tx, workspace_root, &id, update)?;
        emit_codebuddy_text_content(tx, &id, update)?;
        emit_tool_diff_previews_from_raw_output(tx, &id, raw_output)?;
    }

    tx.send(ClientEvent::ToolUpdated {
        id: id.clone(),
        parent_id,
        name: name.clone(),
        kind,
        summary: summary.clone(),
        is_subagent,
        raw_input: raw_input.as_ref().map(format_json),
        raw_output: raw_output.map(format_value_for_ui),
        terminal_output: terminal_output.clone(),
        is_partial,
    })
    .map_err(|_| anyhow!("failed to emit CodeBuddy tool update"))?;

    match effective_status.as_deref() {
        Some("completed") => tx
            .send(ClientEvent::ToolCompleted {
                id,
                name: name.clone(),
                outcome: raw_output
                    .map(format_value_for_ui)
                    .or(summary)
                    .unwrap_or_else(|| "Completed".to_string()),
                raw_output: raw_output.map(format_value_for_ui),
                terminal_output,
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy tool completion")),
        Some("failed") => {
            let error_msg = codebuddy_hard_error_text(update, raw_output)
                .or_else(|| raw_output.map(format_value_for_ui))
                .unwrap_or_else(|| "Tool call failed".to_string());
            let name_for_error = name.clone().unwrap_or_else(|| "tool".to_string());
            let error = if is_vague_error(&error_msg) {
                format!("{error_msg} (tool: {name_for_error})")
            } else {
                error_msg
            };
            tx.send(ClientEvent::ToolFailed {
                id,
                name,
                error,
                raw_output: raw_output.map(format_value_for_ui),
                terminal_output,
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy tool failure"))
        }
        Some(_) => Ok(()),
        None => Ok(()),
    }?;

    if let Some(mode_id) = codebuddy_tool_policy_mode(update, effective_status.as_deref(), true) {
        emit_policy_mode_change(tx, mode_id)?;
    }
    emit_codebuddy_plan_from_raw_response(tx, update)?;

    Ok(())
}

fn emit_codebuddy_plan_from_raw_response(
    tx: &mpsc::Sender<ClientEvent>,
    update: &Value,
) -> anyhow::Result<()> {
    let Some(entries) = codebuddy_plan_entries_from_raw_response(update) else {
        return Ok(());
    };

    tx.send(ClientEvent::PlanUpdated { entries })
        .map_err(|_| anyhow!("failed to emit CodeBuddy todo plan update"))
}

fn codebuddy_plan_entries_from_raw_response(update: &Value) -> Option<Vec<AgentPlanEntry>> {
    let todos = update
        .get("_meta")
        .and_then(|meta| meta.get("codebuddy.ai/rawResponse"))
        .and_then(|raw_response| raw_response.get("todos"))
        .and_then(Value::as_array)?;

    let entries = todos
        .iter()
        .filter_map(codebuddy_todo_plan_entry)
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(entries)
}

fn codebuddy_todo_plan_entry(todo: &Value) -> Option<AgentPlanEntry> {
    let content = string_field(
        todo,
        &["content", "activeForm", "subject", "title", "description"],
    )?;
    Some(AgentPlanEntry {
        id: string_field(todo, &["id"]).map(|id| format!("codebuddy-todo-{id}")),
        content,
        priority: AgentPlanEntryPriority::Medium,
        status: codebuddy_todo_status(todo),
    })
}

fn codebuddy_todo_status(todo: &Value) -> AgentPlanEntryStatus {
    let Some(status) = string_field(todo, &["status"]) else {
        return AgentPlanEntryStatus::Pending;
    };
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "done" => AgentPlanEntryStatus::Completed,
        "in_progress" | "running" | "active" => AgentPlanEntryStatus::InProgress,
        "cancelled" | "canceled" => AgentPlanEntryStatus::Cancelled,
        _ => AgentPlanEntryStatus::Pending,
    }
}

fn emit_codebuddy_current_mode_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: &Value,
) -> anyhow::Result<()> {
    let Some(mode) = string_field(
        update,
        &["currentModeId", "current_mode_id", "modeId", "mode"],
    ) else {
        return Ok(());
    };
    let Some(mode_id) = policy_mode_id(&mode, &mode) else {
        return Ok(());
    };
    emit_policy_mode_change(tx, mode_id)
}

fn emit_policy_mode_change(
    tx: &mpsc::Sender<ClientEvent>,
    mode_id: &'static str,
) -> anyhow::Result<()> {
    tx.send(ClientEvent::SessionConfigValueChanged {
        control_id: "mode".into(),
        value_id: mode_id.into(),
        value_label: Some(policy_mode_label(mode_id).into()),
    })
    .map_err(|_| anyhow!("failed to emit session mode update"))
}

fn emit_codebuddy_text_content(
    tx: &mpsc::Sender<ClientEvent>,
    id: &str,
    update: &Value,
) -> anyhow::Result<()> {
    let text = extract_text(update.get("content"));
    if text.trim().is_empty() {
        return Ok(());
    }

    tx.send(ClientEvent::ToolMessageChunk {
        id: id.to_string(),
        content: text,
    })
    .map_err(|_| anyhow!("failed to emit CodeBuddy tool text content"))
}

fn emit_codebuddy_agent_chunk(
    tx: &mpsc::Sender<ClientEvent>,
    update: &Value,
) -> anyhow::Result<()> {
    let text = extract_text(update.get("content"));
    if text.is_empty() {
        return Ok(());
    }

    if let Some(parent_id) = codebuddy_parent_tool_call_id(update) {
        tx.send(ClientEvent::ToolMessageChunk {
            id: parent_id,
            content: text,
        })
        .map_err(|_| anyhow!("failed to emit CodeBuddy tool message chunk"))
    } else {
        tx.send(ClientEvent::MessageChunk {
            role: MessageRole::Assistant,
            content: text,
        })
        .map_err(|_| anyhow!("failed to emit CodeBuddy assistant chunk"))
    }
}

fn emit_codebuddy_session_info(
    tx: &mpsc::Sender<ClientEvent>,
    update: &Value,
) -> anyhow::Result<()> {
    // Extract agent-provided title if present
    if let Some(title) = update.get("title").and_then(Value::as_str) {
        if !title.is_empty() {
            tx.send(ClientEvent::SessionTitleUpdated {
                title: title.to_string(),
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy session title update"))?;
        }
    }

    let Some(meta) = update.get("_meta") else {
        return Ok(());
    };

    if let Some(interruption_request) = meta.get("codebuddy.ai/interruptionRequest") {
        let tool_call_id = interruption_request
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let tool_name = interruption_request
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("Tool")
            .to_string();
        let options = interruption_request
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|label| PermissionOption {
                        id: label.to_string(),
                        label: label.to_string(),
                        kind: "CodeBuddy".into(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        tx.send(ClientEvent::ToolPermissionRequest {
            id: tool_call_id.to_string(),
            name: tool_name.clone(),
            options,
            details: None,
            input: None,
        })
        .map_err(|_| anyhow!("failed to emit CodeBuddy interruption request"))?;

        return Ok(());
    }

    if meta
        .get("codebuddy.ai/permissionResolved")
        .and_then(Value::as_bool)
        == Some(true)
    {
        let tool_call_id = meta
            .get("codebuddy.ai/toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let decision = meta
            .get("codebuddy.ai/decision")
            .and_then(Value::as_str)
            .unwrap_or("allow");

        tx.send(ClientEvent::ToolPermissionResolved {
            id: tool_call_id.to_string(),
            outcome: format!("Permission resolved: {decision}"),
        })
        .map_err(|_| anyhow!("failed to emit CodeBuddy permission resolution"))?;

        return Ok(());
    }

    if let Some(member_event) = meta.get("codebuddy.ai/memberEvent") {
        let text = match member_event {
            Value::String(name) => format!("{name} is working..."),
            other => format!("Member event: {}", format_json(other)),
        };

        return tx
            .send(ClientEvent::MessageChunk {
                role: MessageRole::System,
                content: text,
            })
            .map_err(|_| anyhow!("failed to emit CodeBuddy member event"));
    }

    Ok(())
}

fn emit_codebuddy_available_commands(
    tx: &mpsc::Sender<ClientEvent>,
    update: &Value,
) -> anyhow::Result<()> {
    let Some(items) = update.get("availableCommands").and_then(Value::as_array) else {
        // Marker notification with no actual commands — ignore
        return Ok(());
    };

    let commands = items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let description = item.get("description")?.as_str()?.to_string();
            let input_hint = item
                .get("input")
                .and_then(|input| input.get("hint"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(AvailableCommand {
                name,
                description,
                input_hint,
            })
        })
        .collect();

    tx.send(ClientEvent::AvailableCommandsUpdated { commands })
        .map_err(|_| anyhow!("failed to emit CodeBuddy available commands"))
}

fn tool_display_name(update: &Value) -> String {
    explicit_tool_display_name(update).unwrap_or_else(|| "Tool".into())
}

fn tool_update_display_name(update: &Value) -> Option<String> {
    update
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            let tool_name = claude_code_tool_name(update)?;
            let path = tool_file_path(update)?;
            Some(format!("{tool_name} {path}"))
        })
        .or_else(|| explicit_tool_display_name(update))
}

fn explicit_tool_display_name(update: &Value) -> Option<String> {
    update
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            update
                .get("_meta")
                .and_then(Value::as_object)
                .and_then(|meta| meta.get("codebuddy.ai/toolName"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| claude_code_tool_name(update))
        .or_else(|| {
            update
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn tool_kind_label(update: &Value) -> String {
    codebuddy_subagent_type(update)
        .or_else(|| {
            update
                .get("_meta")
                .and_then(Value::as_object)
                .and_then(|meta| meta.get("codebuddy.ai/toolName"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            update
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| claude_code_tool_name(update))
        .unwrap_or_else(|| "tool".into())
}

fn tool_summary(update: &Value, fallback: &str) -> String {
    update
        .get("rawInput")
        .and_then(|input| input.get("description"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            update
                .get("rawInput")
                .and_then(|input| input.get("prompt"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| tool_file_path(update))
        .or_else(|| {
            let text = extract_text(update.get("content"));
            (!text.trim().is_empty()).then_some(text)
        })
        .or_else(|| {
            update
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn is_stable_claude_tool_metadata_update(update: &Value) -> bool {
    if claude_code_meta(update).is_none() {
        return false;
    }

    update
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(|title| !title.trim().is_empty())
        || tool_file_path(update).is_some()
        || update
            .get("rawInput")
            .and_then(Value::as_object)
            .is_some_and(|input| !input.is_empty())
}

fn tool_raw_input_for_ui(update: &Value) -> Option<Value> {
    let mut raw_input = update.get("rawInput").cloned();
    if let Some(plan) = codebuddy_plan_content(update) {
        raw_input = Some(insert_raw_input_string(raw_input, "plan", plan));
    }
    let Some(path) = tool_file_path(update) else {
        return raw_input;
    };

    match raw_input {
        Some(Value::Object(ref mut map)) => {
            if !map_has_string_field(map, &["file_path", "filePath", "path"]) {
                map.insert("file_path".into(), Value::String(path));
            }
            raw_input
        }
        Some(value) => Some(value),
        None => Some(serde_json::json!({ "file_path": path })),
    }
}

fn codebuddy_plan_content(update: &Value) -> Option<String> {
    update
        .get("_meta")
        .and_then(|meta| meta.get("codebuddy.ai/planContent"))
        .and_then(Value::as_str)
        .or_else(|| {
            update
                .get("_meta")
                .and_then(|meta| meta.get("codebuddy.ai/rawResponse"))
                .and_then(|raw_response| raw_response.get("plan"))
                .and_then(Value::as_str)
        })
        .filter(|plan| !plan.trim().is_empty())
        .map(str::to_string)
}

fn codebuddy_tool_policy_mode(
    update: &Value,
    status: Option<&str>,
    allow_content_confirmation: bool,
) -> Option<&'static str> {
    let tool_name = codebuddy_mode_tool_name(update)?;
    if tool_name.eq_ignore_ascii_case("EnterPlanMode")
        && (status == Some("completed")
            || (status != Some("failed")
                && allow_content_confirmation
                && codebuddy_update_mentions(update, "entered plan mode")))
    {
        return Some(PLAN_MODE_ID);
    }
    if tool_name.eq_ignore_ascii_case("ExitPlanMode")
        && (status == Some("completed") || codebuddy_update_mentions(update, "exited plan mode"))
    {
        return Some(BUILD_MODE_ID);
    }
    None
}

fn codebuddy_mode_tool_name(update: &Value) -> Option<String> {
    update
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("codebuddy.ai/toolName"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            update
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| {
                    title.eq_ignore_ascii_case("EnterPlanMode")
                        || title.eq_ignore_ascii_case("ExitPlanMode")
                })
                .map(str::to_string)
        })
}

fn codebuddy_update_mentions(update: &Value, needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    [
        update.get("title").and_then(Value::as_str),
        update.get("rawOutput").and_then(Value::as_str),
        update
            .get("rawOutput")
            .and_then(|output| output.get("output"))
            .and_then(Value::as_str),
        update
            .get("fields")
            .and_then(|fields| fields.get("rawOutput"))
            .and_then(Value::as_str),
        update
            .get("fields")
            .and_then(|fields| fields.get("rawOutput"))
            .and_then(|output| output.get("output"))
            .and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_ascii_lowercase().contains(&needle))
        || extract_text(update.get("content"))
            .to_ascii_lowercase()
            .contains(&needle)
}

fn insert_raw_input_string(raw_input: Option<Value>, key: &str, value: String) -> Value {
    match raw_input {
        Some(Value::Object(mut map)) => {
            map.entry(key.to_string()).or_insert(Value::String(value));
            Value::Object(map)
        }
        Some(existing) => {
            let mut map = serde_json::Map::new();
            map.insert("input".into(), existing);
            map.insert(key.to_string(), Value::String(value));
            Value::Object(map)
        }
        None => serde_json::json!({ key: value }),
    }
}

fn tool_file_path(update: &Value) -> Option<String> {
    update
        .get("rawInput")
        .and_then(|input| string_field(input, &["file_path", "filePath", "path"]))
        .or_else(|| {
            update
                .get("locations")
                .and_then(Value::as_array)
                .and_then(|locations| {
                    locations.iter().find_map(|location| {
                        location
                            .get("path")
                            .and_then(Value::as_str)
                            .filter(|path| !path.trim().is_empty())
                            .map(str::to_string)
                    })
                })
        })
        .or_else(|| {
            claude_code_meta(update)
                .and_then(|meta| meta.get("toolResponse"))
                .and_then(|response| response.get("file"))
                .and_then(|file| string_field(file, &["filePath", "file_path", "path"]))
        })
}

fn claude_code_tool_name(update: &Value) -> Option<String> {
    claude_code_meta(update)
        .and_then(|meta| meta.get("toolName"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
}

fn claude_code_meta(update: &Value) -> Option<&Value> {
    update.get("_meta").and_then(|meta| meta.get("claudeCode"))
}

fn codebuddy_raw_output_fallback(update: &Value) -> Option<Value> {
    claude_code_tool_response_text(update).map(Value::String)
}

fn effective_codebuddy_tool_status(
    update: &Value,
    status: Option<&str>,
    raw_output: Option<&Value>,
) -> Option<String> {
    if status == Some("completed") && codebuddy_hard_error_text(update, raw_output).is_some() {
        return Some("failed".into());
    }

    status.map(str::to_string)
}

fn codebuddy_hard_error_text(update: &Value, raw_output: Option<&Value>) -> Option<String> {
    raw_output
        .map(|value| extract_text(Some(value)))
        .filter(|text| looks_like_hard_tool_error(text))
        .or_else(|| {
            claude_code_tool_response_text(update).filter(|text| looks_like_hard_tool_error(text))
        })
}

fn claude_code_tool_response_text(update: &Value) -> Option<String> {
    let response = claude_code_meta(update).and_then(|meta| meta.get("toolResponse"))?;
    let text = response
        .get("content")
        .map(|content| extract_text(Some(content)))
        .or_else(|| {
            response
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| response.as_str().map(str::to_string))?;
    (!text.trim().is_empty()).then_some(text)
}

fn looks_like_hard_tool_error(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("API Error:") || trimmed.contains("指定模型不存在")
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    })
}

fn map_has_string_field(map: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        map.get(*key)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    })
}

fn codebuddy_parent_tool_call_id(update: &Value) -> Option<String> {
    update
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("codebuddy.ai/parentToolCallId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            update
                .get("rawInput")
                .and_then(|input| input.get("parent_tool_call_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn codebuddy_subagent_type(update: &Value) -> Option<String> {
    update
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("codebuddy.ai/subagentType"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            update
                .get("rawInput")
                .and_then(|input| input.get("subagent_type"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn codebuddy_is_subagent(update: &Value) -> bool {
    update
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("codebuddy.ai/isSubagent"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || codebuddy_subagent_type(update).is_some()
        || codebuddy_parent_tool_call_id(update).is_some()
}

fn tool_call_status(update: &Value) -> Option<String> {
    update
        .get("status")
        .and_then(Value::as_str)
        .map(|status| status.to_ascii_lowercase())
        .or_else(|| {
            update
                .get("fields")
                .and_then(|fields| fields.get("status"))
                .and_then(Value::as_str)
                .map(|status| status.to_ascii_lowercase())
        })
}

fn extract_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| extract_text(Some(item)))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(map)) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(content) = map.get("content") {
                let nested = extract_text(Some(content));
                if !nested.is_empty() {
                    return nested;
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}
