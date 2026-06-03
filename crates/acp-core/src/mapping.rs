use crate::events::{ClientEvent, SessionConfig};
use agent_client_protocol::schema::{
    AvailableCommandInput, ConfigOptionUpdate, ContentBlock, CurrentModeUpdate, Plan,
    PlanEntryPriority as AcpPlanEntryPriority, PlanEntryStatus as AcpPlanEntryStatus,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionInfoUpdate, SessionModeState, SessionModelState,
    SessionNotification, SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate,
};
use anyhow::{Context, anyhow};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{OnceLock, mpsc};
use workspace_model::{
    AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, AvailableCommand, DiffHunk,
    DiffLine, DiffLineKind, MessageRole, PermissionOption, SessionConfigCategory,
    SessionConfigChoice, SessionConfigControl, SessionConfigSource, SessionConfigState,
    TerminalOutput,
};

const PLAN_MODE_ID: &str = "plan";
const BUILD_MODE_ID: &str = "build";
const KODEX_CONTEXT_COMPACTED_META_KEY: &str = "kodex.ai/contextCompacted";
const NOTIFICATION_LOG_CHANNEL_SIZE: usize = 1024;

struct NotificationLogRecord {
    log_path: PathBuf,
    method: String,
    payload: Value,
}

static NOTIFICATION_LOG_TX: OnceLock<mpsc::SyncSender<NotificationLogRecord>> = OnceLock::new();

pub(crate) fn emit_notification(
    tx: &mpsc::Sender<ClientEvent>,
    workspace_root: &str,
    notification: SessionNotification,
) -> anyhow::Result<()> {
    let raw_notification =
        serde_json::to_value(&notification).map_err(|err| anyhow!(err.to_string()))?;
    if emit_kodex_notification(tx, &raw_notification)? {
        return Ok(());
    }
    if emit_codebuddy_notification(tx, workspace_root, &raw_notification)? {
        return Ok(());
    }

    match notification.update {
        SessionUpdate::UserMessageChunk(chunk) => {
            emit_content(tx, MessageRole::User, chunk.content)
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            let _ = tx.send(ClientEvent::ThinkingActivity { active: false });
            emit_content(tx, MessageRole::Assistant, chunk.content)
        }
        SessionUpdate::AgentThoughtChunk(_chunk) => tx
            .send(ClientEvent::ThinkingActivity { active: true })
            .map_err(|_| anyhow!("failed to emit thinking activity")),
        SessionUpdate::ToolCall(tool) => emit_tool_call(tx, tool),
        SessionUpdate::ToolCallUpdate(update) => emit_tool_update(tx, update),
        SessionUpdate::ConfigOptionUpdate(update) => emit_config_option_update(tx, update),
        SessionUpdate::CurrentModeUpdate(update) => emit_current_mode_update(tx, update),
        SessionUpdate::Plan(plan) => emit_plan_update(tx, plan),
        SessionUpdate::AvailableCommandsUpdate(update) => emit_available_commands(tx, update),
        SessionUpdate::SessionInfoUpdate(update) => {
            // Emit title update if present
            if let Some(title) = update.title.value() {
                if !title.is_empty() {
                    tx.send(ClientEvent::SessionTitleUpdated {
                        title: title.clone(),
                    })
                    .map_err(|_| anyhow!("failed to emit session title update"))?;
                }
            }
            let content = format_session_info_update(&update);
            if content.is_empty() {
                Ok(())
            } else {
                tx.send(ClientEvent::MessageChunk {
                    role: MessageRole::System,
                    content,
                })
                .map_err(|_| anyhow!("failed to emit session info"))
            }
        }
        _ => Ok(()),
    }
}

pub(crate) fn is_session_state_update(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::AvailableCommandsUpdate(_)
            | SessionUpdate::ConfigOptionUpdate(_)
            | SessionUpdate::CurrentModeUpdate(_)
    )
}

fn emit_plan_update(tx: &mpsc::Sender<ClientEvent>, plan: Plan) -> anyhow::Result<()> {
    let entries = plan
        .entries
        .into_iter()
        .map(|entry| AgentPlanEntry {
            id: None,
            content: entry.content,
            priority: normalize_plan_priority(entry.priority),
            status: normalize_plan_status(entry.status),
        })
        .collect();

    tx.send(ClientEvent::PlanUpdated { entries })
        .map_err(|_| anyhow!("failed to emit plan update"))
}

fn emit_available_commands(
    tx: &mpsc::Sender<ClientEvent>,
    update: agent_client_protocol::schema::AvailableCommandsUpdate,
) -> anyhow::Result<()> {
    let commands = update
        .available_commands
        .into_iter()
        .map(|cmd| {
            let input_hint = cmd.input.and_then(|input| match input {
                AvailableCommandInput::Unstructured(u) => Some(u.hint),
                _ => None,
            });
            AvailableCommand {
                name: cmd.name,
                description: cmd.description,
                input_hint,
            }
        })
        .collect();

    tx.send(ClientEvent::AvailableCommandsUpdated { commands })
        .map_err(|_| anyhow!("failed to emit available commands update"))
}

fn normalize_plan_priority(priority: AcpPlanEntryPriority) -> AgentPlanEntryPriority {
    match priority {
        AcpPlanEntryPriority::High => AgentPlanEntryPriority::High,
        AcpPlanEntryPriority::Medium => AgentPlanEntryPriority::Medium,
        AcpPlanEntryPriority::Low => AgentPlanEntryPriority::Low,
        _ => AgentPlanEntryPriority::Medium,
    }
}

fn normalize_plan_status(status: AcpPlanEntryStatus) -> AgentPlanEntryStatus {
    match status {
        AcpPlanEntryStatus::Pending => AgentPlanEntryStatus::Pending,
        AcpPlanEntryStatus::InProgress => AgentPlanEntryStatus::InProgress,
        AcpPlanEntryStatus::Completed => AgentPlanEntryStatus::Completed,
        _ => AgentPlanEntryStatus::Pending,
    }
}

pub(crate) fn session_config_from_options(options: Vec<SessionConfigOption>) -> SessionConfigState {
    let controls = options
        .into_iter()
        .filter_map(normalize_config_option)
        .collect::<Vec<_>>();

    SessionConfigState {
        hydrated: true,
        controls: with_policy_mode_control(controls, None),
    }
}

pub(crate) fn session_config_from_parts(
    options: Option<Vec<SessionConfigOption>>,
    modes: Option<&SessionModeState>,
    models: Option<&SessionModelState>,
) -> SessionConfigState {
    let mut controls = options
        .unwrap_or_default()
        .into_iter()
        .filter_map(normalize_config_option)
        .collect::<Vec<_>>();

    if let Some(model_control) = models.map(session_config_control_from_models)
        && !controls
            .iter()
            .any(|control| control.category == SessionConfigCategory::Model)
    {
        controls.insert(0, model_control);
    }

    SessionConfigState {
        hydrated: true,
        controls: with_policy_mode_control(controls, modes),
    }
}

fn with_policy_mode_control(
    mut controls: Vec<SessionConfigControl>,
    modes: Option<&SessionModeState>,
) -> Vec<SessionConfigControl> {
    let current_mode = controls
        .iter()
        .filter(|control| control.category == SessionConfigCategory::Mode)
        .find_map(|control| policy_mode_id(&control.current_value_id, &control.current_value_label))
        .or_else(|| modes.and_then(policy_mode_from_modes))
        .unwrap_or(BUILD_MODE_ID);

    controls.retain(|control| control.category != SessionConfigCategory::Mode);
    controls.push(policy_mode_control(current_mode));
    controls
}

fn policy_mode_control(current_mode: &str) -> SessionConfigControl {
    let current_value_id = if current_mode == BUILD_MODE_ID {
        BUILD_MODE_ID
    } else {
        PLAN_MODE_ID
    };
    SessionConfigControl {
        id: "mode".into(),
        label: "Mode".into(),
        description: None,
        category: SessionConfigCategory::Mode,
        source: SessionConfigSource::LocalMode,
        current_value_id: current_value_id.into(),
        current_value_label: policy_mode_label(current_value_id).into(),
        choices: vec![
            SessionConfigChoice {
                id: PLAN_MODE_ID.into(),
                label: "Plan".into(),
                description: Some(
                    "Allow workspace reads and markdown writes; reject shell execution".into(),
                ),
                provider: None,
            },
            SessionConfigChoice {
                id: BUILD_MODE_ID.into(),
                label: "Build".into(),
                description: Some(
                    "Allow workspace work; ask before reading or writing outside the workspace"
                        .into(),
                ),
                provider: None,
            },
        ],
        enabled: true,
    }
}

fn policy_mode_from_modes(modes: &SessionModeState) -> Option<&'static str> {
    let current_mode_id = modes.current_mode_id.0.as_ref();
    modes
        .available_modes
        .iter()
        .find(|mode| mode.id.0.as_ref() == current_mode_id)
        .and_then(|mode| policy_mode_id(mode.id.0.as_ref(), &mode.name))
        .or_else(|| policy_mode_id(current_mode_id, current_mode_id))
}

fn policy_mode_id(id: &str, label: &str) -> Option<&'static str> {
    let id = id.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    if id == PLAN_MODE_ID || label == PLAN_MODE_ID || label.contains("plan") {
        return Some(PLAN_MODE_ID);
    }
    if id == BUILD_MODE_ID
        || label == BUILD_MODE_ID
        || label.contains("build")
        || matches!(
            id.as_str(),
            "default" | "acceptedits" | "auto" | "bypasspermissions"
        )
        || label.contains("manual")
        || label.contains("accept")
        || label.contains("auto")
        || label.contains("bypass")
    {
        return Some(BUILD_MODE_ID);
    }
    None
}

fn policy_mode_label(id: &str) -> &'static str {
    if id == BUILD_MODE_ID { "Build" } else { "Plan" }
}

fn session_config_control_from_models(models: &SessionModelState) -> SessionConfigControl {
    let choices = models
        .available_models
        .iter()
        .map(|model| SessionConfigChoice {
            id: model.model_id.0.to_string(),
            label: model.name.clone(),
            description: model.description.clone(),
            provider: model
                .meta
                .as_ref()
                .and_then(|meta| meta.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect::<Vec<_>>();
    let current_value_id = models.current_model_id.0.to_string();
    let current_value_label = choices
        .iter()
        .find(|choice| choice.id == current_value_id)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| current_value_id.clone());

    SessionConfigControl {
        id: "model".into(),
        label: "Model".into(),
        description: None,
        category: SessionConfigCategory::Model,
        source: SessionConfigSource::SessionModel,
        current_value_id,
        current_value_label,
        choices,
        enabled: true,
    }
}

fn emit_config_option_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: ConfigOptionUpdate,
) -> anyhow::Result<()> {
    tx.send(ClientEvent::SessionConfigUpdated {
        state: session_config_from_options(update.config_options),
    })
    .map_err(|_| anyhow!("failed to emit session config update"))
}

fn emit_current_mode_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: CurrentModeUpdate,
) -> anyhow::Result<()> {
    let Some(mode_id) = policy_mode_id(
        update.current_mode_id.0.as_ref(),
        update.current_mode_id.0.as_ref(),
    ) else {
        return Ok(());
    };

    tx.send(ClientEvent::SessionConfigValueChanged {
        control_id: "mode".into(),
        value_id: mode_id.into(),
        value_label: Some(policy_mode_label(mode_id).into()),
    })
    .map_err(|_| anyhow!("failed to emit session mode update"))
}

fn normalize_config_option(option: SessionConfigOption) -> Option<SessionConfigControl> {
    let select = match option.kind {
        SessionConfigKind::Select(select) => select,
        _ => return None,
    };
    let choices = flatten_select_options(select.options);
    let current_value_id = select.current_value.0.to_string();
    let current_value_label = choices
        .iter()
        .find(|choice| choice.id == current_value_id)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| current_value_id.clone());

    Some(SessionConfigControl {
        id: option.id.0.to_string(),
        label: option.name,
        description: option.description,
        category: normalize_category(option.category),
        source: SessionConfigSource::ConfigOption,
        current_value_id,
        current_value_label,
        choices,
        enabled: true,
    })
}

fn flatten_select_options(options: SessionConfigSelectOptions) -> Vec<SessionConfigChoice> {
    match options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .into_iter()
            .map(|option| SessionConfigChoice {
                id: option.value.0.to_string(),
                label: option.name,
                description: option.description,
                provider: option
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get("provider"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .into_iter()
            .flat_map(|group| group.options)
            .map(|option| SessionConfigChoice {
                id: option.value.0.to_string(),
                label: option.name,
                description: option.description,
                provider: option
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get("provider"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn normalize_category(category: Option<SessionConfigOptionCategory>) -> SessionConfigCategory {
    match category {
        Some(SessionConfigOptionCategory::Model) => SessionConfigCategory::Model,
        Some(SessionConfigOptionCategory::Mode) => SessionConfigCategory::Mode,
        Some(SessionConfigOptionCategory::ThoughtLevel) => SessionConfigCategory::ThoughtLevel,
        Some(SessionConfigOptionCategory::Other(_)) | Some(_) | None => {
            SessionConfigCategory::Other
        }
    }
}

pub(crate) fn append_notification_log(
    config: &SessionConfig,
    method: &str,
    payload: &Value,
) -> anyhow::Result<()> {
    append_notification_log_owned(config, method, payload.clone())
}

pub(crate) fn append_typed_notification_log(
    config: &SessionConfig,
    notification: &SessionNotification,
) -> anyhow::Result<()> {
    let payload = serde_json::to_value(notification).map_err(|err| anyhow!(err.to_string()))?;
    append_notification_log_owned(config, "session/update", payload)
}

pub(crate) fn append_runtime_event_log(
    config: &SessionConfig,
    label: &str,
    payload: &Value,
) -> anyhow::Result<()> {
    append_notification_log(config, label, payload)
}

pub fn notification_log_path(config: &SessionConfig) -> PathBuf {
    PathBuf::from(&config.app_data_root)
        .join("logs")
        .join(format!("acp-notifications-{}.log", config.log_id))
}

fn append_notification_log_owned(
    config: &SessionConfig,
    method: &str,
    payload: Value,
) -> anyhow::Result<()> {
    let record = NotificationLogRecord {
        log_path: notification_log_path(config),
        method: method.to_string(),
        payload,
    };

    match notification_log_tx().try_send(record) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => Ok(()),
        Err(mpsc::TrySendError::Disconnected(record)) => write_notification_log_record(record),
    }
}

fn notification_log_tx() -> &'static mpsc::SyncSender<NotificationLogRecord> {
    NOTIFICATION_LOG_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<NotificationLogRecord>(NOTIFICATION_LOG_CHANNEL_SIZE);
        let _ = std::thread::Builder::new()
            .name("kodex-acp-log-writer".into())
            .spawn(move || {
                while let Ok(record) = rx.recv() {
                    let _ = write_notification_log_record(record);
                }
            });
        tx
    })
}

fn write_notification_log_record(record: NotificationLogRecord) -> anyhow::Result<()> {
    if let Some(parent) = record.log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record.log_path)
        .with_context(|| format!("failed to open log file {}", record.log_path.display()))?;

    writeln!(file, "=== {} ===", record.method)?;
    writeln!(file, "{}", format_json(&record.payload))?;
    writeln!(file)?;
    Ok(())
}

pub(crate) fn format_stop_reason(reason: StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::MaxTurnRequests => "max_turn_requests".into(),
        StopReason::Refusal => "refusal".into(),
        StopReason::Cancelled => "cancelled".into(),
        _ => "unknown".into(),
    }
}

pub fn diff_to_hunks(old_text: Option<&str>, new_text: &str) -> Vec<DiffHunk> {
    use similar::{ChangeTag, TextDiff};

    let old = normalize_diff_line_endings(old_text.unwrap_or_default());
    let new = normalize_diff_line_endings(new_text);
    let diff = TextDiff::from_lines(&old, &new);
    let mut lines = Vec::new();

    let mut has_changes = false;
    for change in diff.iter_all_changes() {
        let content = change.as_str().unwrap_or_default();
        // Strip trailing newline added by line-based diffing
        let content = content.strip_suffix('\n').unwrap_or(content).to_string();
        let kind = match change.tag() {
            ChangeTag::Equal => DiffLineKind::Context,
            ChangeTag::Delete => DiffLineKind::Removed,
            ChangeTag::Insert => DiffLineKind::Added,
        };
        has_changes |= matches!(kind, DiffLineKind::Added | DiffLineKind::Removed);
        lines.push(DiffLine { kind, content });
    }

    if !has_changes {
        return Vec::new();
    }

    vec![DiffHunk {
        heading: "ACP diff".into(),
        lines,
    }]
}

fn normalize_diff_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn emit_content(
    tx: &mpsc::Sender<ClientEvent>,
    role: MessageRole,
    content: ContentBlock,
) -> anyhow::Result<()> {
    let text = match content {
        ContentBlock::Text(text) => text.text,
        other => format!("{:?}", other),
    };

    tx.send(ClientEvent::MessageChunk {
        role,
        content: text,
    })
    .map_err(|_| anyhow!("failed to emit message chunk"))
}

fn emit_tool_call(tx: &mpsc::Sender<ClientEvent>, tool: ToolCall) -> anyhow::Result<()> {
    let tool_id = tool.tool_call_id.0.to_string();
    let tool_title = tool.title.clone();
    let completion_summary = format_tool_completion(&tool);
    let terminal_output = tool.raw_output.as_ref().and_then(parse_terminal_output);
    let raw_output = tool.raw_output.as_ref().map(format_value_for_ui);
    tx.send(ClientEvent::ToolStarted {
        id: tool_id.clone(),
        parent_id: None,
        name: tool_title.clone(),
        kind: format!("{:?}", tool.kind),
        summary: format_tool_call_summary(&tool),
        is_subagent: false,
        raw_input: tool.raw_input.as_ref().map(format_json),
    })
    .map_err(|_| anyhow!("failed to emit tool start"))?;

    for content in tool.content {
        emit_tool_content(tx, &tool_id, content)?;
    }
    emit_tool_diff_previews_from_raw_output(tx, &tool_id, tool.raw_output.as_ref())?;

    if tool.status == ToolCallStatus::Completed {
        tx.send(ClientEvent::ToolCompleted {
            id: tool_id,
            name: Some(tool_title),
            outcome: completion_summary,
            raw_output,
            terminal_output,
        })
        .map_err(|_| anyhow!("failed to emit tool completion"))?;
    }

    Ok(())
}

fn emit_codebuddy_notification(
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

fn emit_kodex_notification(
    tx: &mpsc::Sender<ClientEvent>,
    payload: &Value,
) -> anyhow::Result<bool> {
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
    }
}

fn emit_codebuddy_diff_content(
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

fn emit_tool_diff_previews_from_raw_output(
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

fn edit_preview_new_text_from_raw_input(raw_input: Option<&Value>) -> Option<String> {
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

fn emit_tool_update(tx: &mpsc::Sender<ClientEvent>, update: ToolCallUpdate) -> anyhow::Result<()> {
    let id = update.tool_call_id.0.to_string();
    let title = update.fields.title.clone().unwrap_or_else(|| "tool".into());

    if let Some(content) = update.fields.content {
        for item in content {
            emit_tool_content(tx, &id, item)?;
        }
    }
    emit_tool_diff_previews_from_raw_output(tx, &id, update.fields.raw_output.as_ref())?;

    if let Some(status) = update.fields.status {
        match status {
            ToolCallStatus::Completed => tx
                .send(ClientEvent::ToolCompleted {
                    id,
                    name: Some(title),
                    outcome: format_tool_update_summary(status, update.fields.raw_output.as_ref()),
                    raw_output: update.fields.raw_output.as_ref().map(format_value_for_ui),
                    terminal_output: update
                        .fields
                        .raw_output
                        .as_ref()
                        .and_then(parse_terminal_output),
                })
                .map_err(|_| anyhow!("failed to emit completed tool update"))?,
            ToolCallStatus::Failed => tx
                .send(ClientEvent::ToolFailed {
                    id,
                    name: Some(title.clone()),
                    error: format_tool_failure(update.fields.raw_output.as_ref(), &title),
                    raw_output: update.fields.raw_output.as_ref().map(format_value_for_ui),
                    terminal_output: update
                        .fields
                        .raw_output
                        .as_ref()
                        .and_then(parse_terminal_output),
                })
                .map_err(|_| anyhow!("failed to emit failed tool update"))?,
            ToolCallStatus::InProgress | ToolCallStatus::Pending => tx
                .send(ClientEvent::ToolProgress {
                    id,
                    content: format_tool_update_summary(status, update.fields.raw_output.as_ref()),
                })
                .map_err(|_| anyhow!("failed to emit tool progress"))?,
            _ => tx
                .send(ClientEvent::ToolProgress {
                    id,
                    content: format_tool_update_summary(status, update.fields.raw_output.as_ref()),
                })
                .map_err(|_| anyhow!("failed to emit tool status"))?,
        }
    }

    Ok(())
}

pub(crate) fn format_permission_options(options: &[String]) -> String {
    if options.is_empty() {
        return "Permission requested".into();
    }

    format!("Permission required: {}", options.join(" / "))
}

fn format_tool_call_summary(tool: &ToolCall) -> String {
    let mut parts = vec![tool.title.clone()];

    if !tool.locations.is_empty() {
        parts.push(format!("{} location(s)", tool.locations.len()));
    }

    parts.join(" | ")
}

fn format_tool_completion(tool: &ToolCall) -> String {
    if let Some(raw_output) = &tool.raw_output {
        return format!("Completed | {}", format_value_for_summary(raw_output));
    }

    if !tool.locations.is_empty() {
        return format!("Completed | {} location(s)", tool.locations.len());
    }

    "Completed".into()
}

fn format_tool_update_summary(status: ToolCallStatus, raw_output: Option<&Value>) -> String {
    match raw_output {
        Some(raw_output) => format_value_for_summary(raw_output),
        None => match status {
            ToolCallStatus::Pending => "Awaiting approval".into(),
            ToolCallStatus::InProgress => "Executing".into(),
            ToolCallStatus::Completed => "Completed".into(),
            ToolCallStatus::Failed => "Failed".into(),
            _ => "Working".into(),
        },
    }
}

fn format_tool_failure(raw_output: Option<&Value>, tool_name: &str) -> String {
    let message = match raw_output {
        Some(raw_output) => format_value_for_ui(raw_output),
        None => "Tool call failed".into(),
    };

    // If the server returned a vague error, include the tool name for context
    if is_vague_error(&message) {
        format!("{message} (tool: {tool_name})")
    } else {
        message
    }
}

/// Returns true if the error message is too vague to be useful on its own.
fn is_vague_error(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "internal error"
            | "internal server error"
            | "error"
            | "unknown error"
            | "tool call failed"
            | "failed"
    )
}

fn format_value_for_summary(value: &Value) -> String {
    let formatted = format_value_for_ui(value);
    let compact = formatted.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() > 160 {
        let truncated: String = compact.chars().take(157).collect();
        format!("{truncated}...")
    } else {
        compact
    }
}

fn format_value_for_ui(value: &Value) -> String {
    if let Some(text) = extract_tool_text_payload(value) {
        return normalize_tool_text_payload(&text);
    }

    if let Some(command) = value.get("command").and_then(Value::as_str) {
        return command.to_string();
    }

    if let Some(path) = value.get("file_path").and_then(Value::as_str) {
        return path.to_string();
    }

    format_json(value)
}

fn extract_tool_text_payload(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.to_string()),
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("text") {
                return map.get("text").and_then(Value::as_str).map(str::to_string);
            }

            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }

            None
        }
        _ => None,
    }
}

fn normalize_tool_text_payload(text: &str) -> String {
    if let Some(normalized) = normalize_terminal_completion_text(text) {
        return normalized;
    }

    text.to_string()
}

fn normalize_terminal_completion_text(text: &str) -> Option<String> {
    let rest = text.strip_prefix("Exited with code ")?;
    let (code, remaining) = rest.split_once('.')?;
    let code = code.trim();
    let remaining = remaining.trim_start();

    if let Some(output) = remaining.strip_prefix("Final output:") {
        let output = output.trim_start_matches(['\r', '\n']).trim_end();
        return Some(if output.is_empty() {
            format!("Exit code: {code}")
        } else {
            format!("Exit code: {code}\n\n{output}")
        });
    }

    if remaining.starts_with("No output") {
        return Some(format!("Exit code: {code}"));
    }

    Some(format!("Exit code: {code}\n\n{remaining}"))
}

fn parse_terminal_output(value: &Value) -> Option<TerminalOutput> {
    let text = extract_tool_text_payload(value)?;
    let rest = text.strip_prefix("Exited with code ")?;
    let (code_text, remaining) = rest.split_once('.')?;
    let exit_code = code_text.trim().parse::<i32>().ok();
    let remaining = remaining.trim_start();

    let output = if let Some(output) = remaining.strip_prefix("Final output:") {
        output
            .trim_start_matches(['\r', '\n'])
            .trim_end()
            .to_string()
    } else if remaining.starts_with("No output") {
        String::new()
    } else {
        remaining.to_string()
    };

    Some(TerminalOutput { exit_code, output })
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn format_session_info_update(update: &SessionInfoUpdate) -> String {
    if let Some(meta) = &update.meta {
        if let Some(member_event) = meta.get("codebuddy.ai/memberEvent") {
            return format!("Member event: {}", format_json(member_event));
        }

        if let Some(team_update) = meta.get("codebuddy.ai/teamUpdate") {
            return format!("Team update: {}", format_json(team_update));
        }
    }

    String::new()
}

fn emit_tool_content(
    tx: &mpsc::Sender<ClientEvent>,
    tool_call_id: &str,
    content: ToolCallContent,
) -> anyhow::Result<()> {
    match content {
        ToolCallContent::Content(content) => emit_content(tx, MessageRole::System, content.content),
        ToolCallContent::Diff(diff) => tx
            .send(ClientEvent::ToolDiff {
                id: tool_call_id.to_string(),
                path: diff.path.display().to_string(),
                old_text: diff.old_text,
                new_text: diff.new_text,
            })
            .map_err(|_| anyhow!("failed to emit tool diff")),
        ToolCallContent::Terminal(terminal) => tx
            .send(ClientEvent::ToolProgress {
                id: tool_call_id.to_string(),
                content: format!("Terminal attached: {}", terminal.terminal_id.0),
            })
            .map_err(|_| anyhow!("failed to emit terminal content")),
        _ => tx
            .send(ClientEvent::ToolProgress {
                id: tool_call_id.to_string(),
                content: "Received unsupported ACP tool content".into(),
            })
            .map_err(|_| anyhow!("failed to emit generic tool content")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{PlanEntry, SessionNotification};
    use std::path::PathBuf;

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("kodex-acp-core-mapping-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn write(&self, relative_path: &str, contents: &str) {
            let path = self.root.join(relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }

        fn root_str(&self) -> &str {
            self.root.to_str().unwrap()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn diff_conversion_marks_added_and_removed_lines() {
        let hunks = diff_to_hunks(Some("alpha\nbeta"), "alpha\ngamma");
        assert_eq!(hunks.len(), 1);
        assert!(
            hunks[0]
                .lines
                .iter()
                .any(|line| matches!(line.kind, DiffLineKind::Removed) && line.content == "beta")
        );
        assert!(
            hunks[0]
                .lines
                .iter()
                .any(|line| matches!(line.kind, DiffLineKind::Added) && line.content == "gamma")
        );
    }

    #[test]
    fn diff_conversion_returns_empty_for_unchanged_content() {
        let hunks = diff_to_hunks(Some("alpha\nbeta"), "alpha\nbeta");
        assert!(hunks.is_empty());
    }

    #[test]
    fn diff_conversion_ignores_line_ending_only_changes() {
        let hunks = diff_to_hunks(Some("alpha\r\nbeta\r\n"), "alpha\nbeta\n");
        assert!(hunks.is_empty());
    }

    #[test]
    fn kodex_context_compacted_meta_emits_structured_event() {
        let (tx, rx) = mpsc::channel();
        let notification = SessionNotification::new(
            "session-1",
            SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new()),
        )
        .meta(serde_json::Map::from_iter([(
            KODEX_CONTEXT_COMPACTED_META_KEY.to_string(),
            serde_json::json!({
                "message": "上下文已压缩"
            }),
        )]));

        emit_notification(&tx, "", notification).unwrap();

        match rx.try_recv().unwrap() {
            ClientEvent::ContextCompacted { message } => {
                assert_eq!(message, "上下文已压缩");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn codebuddy_diff_content_preserves_old_text_for_tool_card_stats() {
        let (tx, rx) = mpsc::channel();
        let old_text = "# Kodex\n\n## Project Structure\n\nbody\n";
        let new_text = "# Kodex\n\n## 项目结构\n\nbody\n";

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "claudeCode": {
                            "toolName": "Write"
                        }
                    },
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-edit-1",
                    "status": "completed",
                    "content": [{
                        "type": "diff",
                        "path": "/workspace/README.md",
                        "oldText": old_text,
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                id,
                path,
                old_text: emitted_old_text,
                new_text: emitted_new_text,
            } => {
                assert_eq!(id, "call-edit-1");
                assert_eq!(path, "/workspace/README.md");
                assert_eq!(emitted_old_text.as_deref(), Some(old_text));
                assert_eq!(emitted_new_text, new_text);

                let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
                assert_eq!(added, vec!["## 项目结构"]);
                assert_eq!(removed, vec!["## Project Structure"]);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_path_only_content_reads_workspace_files_for_diff() {
        let workspace = TestWorkspace::new();
        workspace.write(
            "openspec/changes/support-import-without-filename/design.md",
            "## Design\n\nUse offline CSV maps.\n",
        );
        workspace.write(
            "openspec/changes/support-import-without-filename/tasks.md",
            "## Tasks\n\n- [ ] Add parser\n",
        );

        let (tx, rx) = mpsc::channel();
        let handled = emit_codebuddy_notification(
            &tx,
            workspace.root_str(),
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-write-openspec",
                    "status": "completed",
                    "content": [
                        {
                            "type": "diff",
                            "path": "openspec/changes/support-import-without-filename/design.md"
                        },
                        {
                            "type": "content",
                            "content": {
                                "type": "diff",
                                "filePath": "openspec/changes/support-import-without-filename/tasks.md"
                            }
                        }
                    ]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let diffs = rx
            .try_iter()
            .filter_map(|event| match event {
                ClientEvent::ToolDiff {
                    id,
                    path,
                    old_text,
                    new_text,
                } => Some((id, path, old_text, new_text)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].0, "call-write-openspec");
        assert_eq!(
            diffs[0].1,
            "openspec/changes/support-import-without-filename/design.md"
        );
        assert_eq!(diffs[0].2, None);
        assert_eq!(diffs[0].3, "## Design\n\nUse offline CSV maps.\n");
        assert_eq!(
            diffs[1].1,
            "openspec/changes/support-import-without-filename/tasks.md"
        );
        assert_eq!(diffs[1].2, None);
        assert_eq!(diffs[1].3, "## Tasks\n\n- [ ] Add parser\n");
    }

    #[test]
    fn codebuddy_raw_output_changes_emit_tool_diff_preview() {
        let (tx, rx) = mpsc::channel();
        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-raw-diff",
                    "status": "completed",
                    "title": "Edit D:\\work\\App\\src\\main.rs",
                    "rawOutput": {
                        "changes": {
                            "D:\\work\\App\\src\\main.rs": {
                                "move_path": null,
                                "type": "update",
                                "unified_diff": "@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"done\");\n }\n"
                            }
                        },
                        "success": true
                    }
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let previews = rx
            .try_iter()
            .filter_map(|event| match event {
                ClientEvent::ToolDiffPreview { id, path, hunks } => Some((id, path, hunks)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].0, "call-raw-diff");
        assert_eq!(previews[0].1, "D:\\work\\App\\src\\main.rs");
        assert_eq!(previews[0].2.len(), 1);
        assert_eq!(previews[0].2[0].heading, "@@ -1,3 +1,4 @@");
        let added = previews[0].2[0]
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let removed = previews[0].2[0]
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Removed)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            added,
            vec!["    println!(\"new\");", "    println!(\"done\");"]
        );
        assert_eq!(removed, vec!["    println!(\"old\");"]);
    }

    #[test]
    fn claude_write_create_raw_input_content_is_not_treated_as_old_text() {
        let (tx, rx) = mpsc::channel();
        let new_text = "export const value = 1;\n";

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "claudeCode": {
                            "toolName": "Write"
                        }
                    },
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-write-create",
                    "title": "Write packages/backend/scripts/migrate-vision-tags-to-structured.ts",
                    "rawInput": {
                        "file_path": "/d/work/ArtAssets/src/new.ts",
                        "content": new_text
                    },
                    "content": [{
                        "type": "diff",
                        "path": "/d/work/ArtAssets/src/new.ts",
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text: emitted_new_text,
            } => {
                assert_eq!(id, "call-write-create");
                assert_eq!(path, "/d/work/ArtAssets/src/new.ts");
                assert_eq!(old_text, None);
                assert_eq!(emitted_new_text, new_text);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn unix_drive_prefix_paths_are_normalized_for_workspace_file_reads() {
        assert_eq!(
            normalize_unix_drive_prefix("/d/work/ArtAssets/docs/tags.md"),
            "D:/work/ArtAssets/docs/tags.md"
        );
        assert_eq!(
            normalize_unix_drive_prefix("/mnt/d/work/ArtAssets/docs/tags.md"),
            "D:/work/ArtAssets/docs/tags.md"
        );
    }

    #[test]
    fn edit_preview_new_text_can_be_rebuilt_from_raw_input_content() {
        let raw_input = serde_json::json!({
            "path": "/workspace/README.md",
            "before": "## Project Structure\n",
            "after": "## 项目结构\n",
            "content": "# Kodex\n\n## Project Structure\n\nbody\n"
        });

        assert_eq!(
            edit_preview_new_text_from_raw_input(Some(&raw_input)).as_deref(),
            Some("# Kodex\n\n## 项目结构\n\nbody\n")
        );
    }

    #[test]
    fn codebuddy_empty_old_text_uses_raw_input_content_for_tool_card_stats() {
        let (tx, rx) = mpsc::channel();
        let old_text = "# Kodex\n\n## Project Structure\n\nbody\n";
        let new_text = "# Kodex\n\n## 项目结构\n\nbody\n";

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-edit-1",
                    "status": "completed",
                    "rawInput": {
                        "path": "/workspace/README.md",
                        "before": "## Project Structure\n",
                        "after": "## 项目结构\n",
                        "content": old_text
                    },
                    "content": [{
                        "type": "diff",
                        "path": "/workspace/README.md",
                        "oldText": "",
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                old_text: emitted_old_text,
                new_text: emitted_new_text,
                ..
            } => {
                assert_eq!(emitted_old_text.as_deref(), Some(old_text));

                let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
                assert_eq!((added, removed), (1, 1));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_empty_old_text_can_be_reversed_from_raw_input_replacement() {
        let (tx, rx) = mpsc::channel();
        let old_text = "const value = 'old';\nconsole.log(value);\n";
        let new_text = "const value = 'new';\nconsole.log(value);\n";

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-edit-2",
                    "status": "completed",
                    "rawInput": {
                        "path": "/workspace/storyboard.ts",
                        "before": "const value = 'old';",
                        "after": "const value = 'new';"
                    },
                    "content": [{
                        "type": "diff",
                        "path": "/workspace/storyboard.ts",
                        "oldText": "",
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                old_text: emitted_old_text,
                new_text: emitted_new_text,
                ..
            } => {
                assert_eq!(emitted_old_text.as_deref(), Some(old_text));

                let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
                assert_eq!((added, removed), (1, 1));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_old_new_string_can_reconstruct_full_old_text() {
        let (tx, rx) = mpsc::channel();
        let old_fragment = "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.locator('body')).toBeVisible();\n  await page.evaluate(buildTestIdInjector());\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}";
        let new_fragment = "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.locator('body')).toBeVisible();\n  await page.evaluate(buildTestIdInjector());\n  await page.waitForFunction(() => {\n    const win = window as Window & { __smokeTagElements?: () => void };\n    win.__smokeTagElements?.();\n    return Boolean(document.querySelector('[data-testid=\"prompt-shell\"]'));\n  }, undefined, { timeout: 10_000 });\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}";
        let old_text = format!("header\n\n{old_fragment}\n\nfooter\n");
        let new_text = format!("header\n\n{new_fragment}\n\nfooter\n");

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-edit-old-string",
                    "status": "completed",
                    "rawInput": {
                        "file_path": "/workspace/smokeTest/tests/app-smoke.spec.ts",
                        "old_string": old_fragment,
                        "new_string": new_fragment
                    },
                    "content": [{
                        "type": "diff",
                        "path": "/workspace/smokeTest/tests/app-smoke.spec.ts",
                        "oldText": "",
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                old_text: emitted_old_text,
                new_text: emitted_new_text,
                ..
            } => {
                assert_eq!(emitted_old_text.as_deref(), Some(old_text.as_str()));

                let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
                let added = hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .filter(|line| line.kind == DiffLineKind::Added)
                    .count();
                assert!(added < 20, "should not render the whole file as added");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_fragment_old_text_prefers_reconstructed_full_old_text() {
        let (tx, rx) = mpsc::channel();
        let old_text = "def helper():\n    return _UA\n\nprint(helper())\n";
        let new_text = "def helper():\n    return _ua()\n\nprint(helper())\n";

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call-edit-3",
                    "status": "completed",
                    "rawInput": {
                        "path": "/workspace/inspiration.py",
                        "before": "_UA",
                        "after": "_ua()"
                    },
                    "content": [{
                        "type": "diff",
                        "path": "/workspace/inspiration.py",
                        "oldText": "_UA",
                        "newText": new_text
                    }]
                }
            }),
        )
        .unwrap();

        assert!(handled);
        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolDiff {
                old_text: emitted_old_text,
                new_text: emitted_new_text,
                ..
            } => {
                assert_eq!(emitted_old_text.as_deref(), Some(old_text));

                let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
                assert_eq!((added, removed), (1, 1));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn plan_update_emits_normalized_plan_event() {
        let (tx, rx) = mpsc::channel();

        emit_notification(
            &tx,
            "",
            SessionNotification::new(
                "session-1",
                SessionUpdate::Plan(Plan::new(vec![
                    PlanEntry::new(
                        "Read the code",
                        AcpPlanEntryPriority::High,
                        AcpPlanEntryStatus::Pending,
                    ),
                    PlanEntry::new(
                        "Apply the fix",
                        AcpPlanEntryPriority::Medium,
                        AcpPlanEntryStatus::InProgress,
                    ),
                    PlanEntry::new(
                        "Verify behavior",
                        AcpPlanEntryPriority::Low,
                        AcpPlanEntryStatus::Completed,
                    ),
                ])),
            ),
        )
        .unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(
            event,
            ClientEvent::PlanUpdated {
                entries: vec![
                    AgentPlanEntry {
                        id: None,
                        content: "Read the code".into(),
                        priority: AgentPlanEntryPriority::High,
                        status: AgentPlanEntryStatus::Pending,
                    },
                    AgentPlanEntry {
                        id: None,
                        content: "Apply the fix".into(),
                        priority: AgentPlanEntryPriority::Medium,
                        status: AgentPlanEntryStatus::InProgress,
                    },
                    AgentPlanEntry {
                        id: None,
                        content: "Verify behavior".into(),
                        priority: AgentPlanEntryPriority::Low,
                        status: AgentPlanEntryStatus::Completed,
                    },
                ],
            }
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn codebuddy_exit_plan_mode_preserves_plan_content_in_raw_input() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "codebuddy.ai/toolName": "ExitPlanMode",
                        "codebuddy.ai/planContent": "# Plan\n\nShip the fix."
                    },
                    "rawInput": {
                        "allowedPrompts": []
                    },
                    "sessionUpdate": "tool_call",
                    "title": "ExitPlanMode",
                    "toolCallId": "call-exit-plan"
                }
            }),
        )
        .unwrap();

        assert!(handled);
        match rx.try_recv().unwrap() {
            ClientEvent::ToolStarted { raw_input, .. } => {
                let raw_input = raw_input.unwrap();
                assert!(raw_input.contains("# Plan"));
                assert!(raw_input.contains("allowedPrompts"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_task_tool_call_marks_latest_subagent_format_as_subagent() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call",
                    "title": "task",
                    "toolCallId": "chatcmpl-tool-1",
                    "rawInput": {
                        "description": "探索项目结构和状态",
                        "prompt": "探索 D:/work/kodex",
                        "subagent_type": "explore"
                    }
                }
            }),
        )
        .unwrap();

        assert!(handled);

        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolStarted {
                id,
                parent_id,
                name,
                kind,
                summary,
                is_subagent,
                raw_input,
            } => {
                assert_eq!(id, "chatcmpl-tool-1");
                assert_eq!(parent_id, None);
                assert_eq!(name, "task");
                assert_eq!(kind, "explore");
                assert_eq!(summary, "探索项目结构和状态");
                assert!(is_subagent);
                assert!(raw_input.unwrap_or_default().contains("subagent_type"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn codebuddy_task_update_emits_tool_message_chunk_from_content_text() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "chatcmpl-tool-1",
                    "title": "task",
                    "status": "completed",
                    "rawInput": {
                        "description": "探索项目结构和状态",
                        "subagent_type": "explore"
                    },
                    "content": [
                        {
                            "type": "content",
                            "content": {
                                "type": "text",
                                "text": "task_id: ses_123\n\n<task_result>done</task_result>"
                            }
                        }
                    ],
                    "rawOutput": {
                        "output": "task_id: ses_123\n\n<task_result>done</task_result>",
                        "metadata": {
                            "sessionId": "ses_123",
                            "truncated": false
                        }
                    }
                }
            }),
        )
        .unwrap();

        assert!(handled);

        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::ToolMessageChunk { id, content }
                if id == "chatcmpl-tool-1" && content.contains("<task_result>done</task_result>")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::ToolCompleted { id, .. } if id == "chatcmpl-tool-1"
        )));
    }

    #[test]
    fn claude_metadata_update_without_status_updates_tool_path() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "claudeCode": {
                            "toolName": "Read"
                        }
                    },
                    "kind": "read",
                    "locations": [
                        {
                            "line": 1,
                            "path": "D:/work/ArtAssets/packages/frontend/src/utils/display.ts"
                        }
                    ],
                    "rawInput": {
                        "file_path": "D:/work/ArtAssets/packages/frontend/src/utils/display.ts"
                    },
                    "sessionUpdate": "tool_call_update",
                    "title": "Read packages/frontend/src/utils/display.ts",
                    "toolCallId": "tooluse_read_1"
                }
            }),
        )
        .unwrap();

        assert!(handled);

        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolUpdated {
                id,
                name,
                kind,
                summary,
                raw_input,
                is_partial,
                ..
            } => {
                assert_eq!(id, "tooluse_read_1");
                assert_eq!(
                    name.as_deref(),
                    Some("Read packages/frontend/src/utils/display.ts")
                );
                assert_eq!(kind.as_deref(), Some("read"));
                assert_eq!(
                    summary.as_deref(),
                    Some("D:/work/ArtAssets/packages/frontend/src/utils/display.ts")
                );
                assert!(
                    raw_input
                        .as_deref()
                        .is_some_and(|input| input.contains("\"file_path\"")
                            && input.contains("packages/frontend/src/utils/display.ts"))
                );
                assert!(!is_partial);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn claude_tool_response_file_path_is_synthesized_as_raw_input() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "claudeCode": {
                            "toolName": "Read",
                            "toolResponse": {
                                "file": {
                                    "filePath": "D:/work/ArtAssets/packages/frontend/src/pages/AssetDetailPage.tsx",
                                    "numLines": 618,
                                    "startLine": 1,
                                    "totalLines": 618
                                },
                                "type": "text"
                            }
                        }
                    },
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tooluse_read_2"
                }
            }),
        )
        .unwrap();

        assert!(handled);

        let event = rx.try_recv().unwrap();
        match event {
            ClientEvent::ToolUpdated {
                name,
                raw_input,
                is_partial,
                ..
            } => {
                assert_eq!(
                    name.as_deref(),
                    Some("Read D:/work/ArtAssets/packages/frontend/src/pages/AssetDetailPage.tsx")
                );
                assert!(
                    raw_input
                        .as_deref()
                        .is_some_and(|input| input.contains("\"file_path\"")
                            && input.contains("AssetDetailPage.tsx"))
                );
                assert!(!is_partial);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn claude_tool_response_api_error_is_mapped_as_tool_failure() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "_meta": {
                        "claudeCode": {
                            "toolName": "Agent",
                            "toolResponse": {
                                "content": [
                                    {
                                        "text": "API Error: 400 指定模型不存在，请重启 claude-internal 后重试",
                                        "type": "text"
                                    }
                                ],
                                "status": "completed"
                            }
                        }
                    },
                    "sessionUpdate": "tool_call_update",
                    "status": "completed",
                    "toolCallId": "tooluse_agent_error"
                }
            }),
        )
        .unwrap();

        assert!(handled);

        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::ToolUpdated { id, raw_output, .. }
                if id == "tooluse_agent_error"
                    && raw_output.as_deref().is_some_and(|output| output.contains("API Error: 400"))
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::ToolFailed { id, name, error, raw_output, .. }
                if id == "tooluse_agent_error"
                    && name.as_deref() == Some("Agent")
                    && error.contains("指定模型不存在")
                    && raw_output.as_deref().is_some_and(|output| output.contains("API Error: 400"))
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            ClientEvent::ToolCompleted { id, .. } if id == "tooluse_agent_error"
        )));
    }

    #[test]
    fn codebuddy_whitespace_agent_chunks_are_preserved() {
        let (tx, rx) = mpsc::channel();

        let handled = emit_codebuddy_notification(
            &tx,
            "",
            &serde_json::json!({
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "\n\n"
                    }
                }
            }),
        )
        .unwrap();

        assert!(handled);
        match rx.try_recv().expect("whitespace chunk should be emitted") {
            ClientEvent::MessageChunk { role, content } => {
                assert_eq!(role, MessageRole::Assistant);
                assert_eq!(content, "\n\n");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }
}
