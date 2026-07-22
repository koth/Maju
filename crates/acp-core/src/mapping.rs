use crate::events::{ClientEvent, SessionConfig};
use agent_client_protocol::schema::{
    AvailableCommandInput, ConfigOptionUpdate, ContentBlock, CurrentModeUpdate, Plan,
    PlanEntryPriority as AcpPlanEntryPriority, PlanEntryStatus as AcpPlanEntryStatus,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionInfoUpdate, SessionModeState, SessionModelState,
    SessionNotification, SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
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
    TerminalOutput, UsageContextSnapshot, UsageEvent, UsageEventScope, UsageTokenBreakdown,
};

mod codebuddy;
mod session_config;

#[cfg(test)]
use codebuddy::{edit_preview_new_text_from_raw_input, normalize_unix_drive_prefix};
use codebuddy::{
    emit_codebuddy_notification, emit_kodex_notification, emit_tool_diff_previews_from_raw_output,
};
use session_config::{
    emit_available_commands, emit_config_option_update, emit_current_mode_update, emit_plan_update,
    policy_mode_id, policy_mode_label,
};
pub(crate) use session_config::{session_config_from_options, session_config_from_parts};

const PLAN_MODE_ID: &str = "plan";
const BUILD_MODE_ID: &str = "build";
const FULL_ACCESS_MODE_ID: &str = "full-access";
const KODEX_CONTEXT_COMPACTION_META_KEY: &str = "kodex.ai/contextCompaction";
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
        SessionUpdate::UsageUpdate(update) => emit_usage_update(tx, update, &raw_notification),
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

pub(crate) fn append_notification_log(
    config: &SessionConfig,
    method: &str,
    payload: &Value,
) -> anyhow::Result<()> {
    append_notification_log_owned(config, method, payload.clone())
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

fn emit_usage_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: UsageUpdate,
    raw_notification: &Value,
) -> anyhow::Result<()> {
    let update_meta = update
        .meta
        .as_ref()
        .and_then(|meta| meta.get("kodex.ai/usage"));
    let notification_meta = raw_notification
        .get("_meta")
        .and_then(|meta| meta.get("kodex.ai/usage"));
    let meta = update_meta.or(notification_meta);
    let context = UsageContextSnapshot {
        used_tokens: Some(update.used),
        window_tokens: Some(update.size),
        updated_at: None,
    };

    // When meta is absent we still want to surface context occupancy from the
    // standard ACP `used`/`size` fields, so emit a single context_snapshot
    // event with empty token breakdown.
    let Some(meta) = meta else {
        let usage = UsageEvent {
            scope: UsageEventScope::ContextSnapshot,
            model: None,
            provider: None,
            agent_cli: None,
            tokens: UsageTokenBreakdown::default(),
            context,
            timestamp: None,
            raw_json: None,
        };
        return tx
            .send(ClientEvent::UsageUpdated { usage })
            .map_err(|_| anyhow!("failed to emit usage update"));
    };

    let model = usage_string_field(Some(meta), "model");
    let provider = usage_string_field(Some(meta), "provider");
    let agent_cli = usage_string_field(Some(meta), "agent_cli");
    let raw_json = serde_json::to_string(meta).ok();

    // Top-level token fields describe the session cumulative usage
    // (scope: session_total). codex-acp emits both scopes from a single
    // token-count event by attaching a nested `turn_delta` object.
    let total_tokens = usage_tokens_from_meta(Some(meta));
    let session_event = UsageEvent {
        scope: UsageEventScope::SessionTotal,
        model: model.clone(),
        provider: provider.clone(),
        agent_cli: agent_cli.clone(),
        tokens: total_tokens,
        context: context.clone(),
        timestamp: None,
        raw_json: raw_json.clone(),
    };
    tx.send(ClientEvent::UsageUpdated {
        usage: session_event,
    })
    .map_err(|_| anyhow!("failed to emit usage update"))?;

    // Nested `turn_delta` object describes the most recent request.
    // Skip emission when missing or when all token fields are zero/null so we
    // do not write meaningless rows to the persisted usage stream.
    if let Some(turn_meta) = meta.get("turn_delta") {
        let turn_tokens = usage_tokens_from_meta(Some(turn_meta));
        if has_any_token_value(&turn_tokens) {
            let turn_event = UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model,
                provider,
                agent_cli,
                tokens: turn_tokens,
                context,
                timestamp: None,
                raw_json,
            };
            tx.send(ClientEvent::UsageUpdated { usage: turn_event })
                .map_err(|_| anyhow!("failed to emit usage update"))?;
        }
    }

    Ok(())
}

/// Returns `true` if any field of the breakdown holds a non-zero numeric value.
/// Used to skip `turn_delta` events that carry only nulls/zeroes (e.g. the
/// initial token-count frame for a new turn before any real consumption).
fn has_any_token_value(tokens: &UsageTokenBreakdown) -> bool {
    [
        tokens.input_tokens,
        tokens.output_tokens,
        tokens.cache_read_tokens,
        tokens.cache_write_tokens,
        tokens.reasoning_tokens,
        tokens.total_tokens,
    ]
    .into_iter()
    .flatten()
    .any(|value| value > 0)
}

fn usage_string_field(meta: Option<&Value>, key: &str) -> Option<String> {
    meta.and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn usage_tokens_from_meta(meta: Option<&Value>) -> UsageTokenBreakdown {
    let Some(meta) = meta else {
        return UsageTokenBreakdown::default();
    };
    let input_tokens = usage_u64_field(meta, &["input_tokens", "inputTokens", "prompt_tokens"]);
    let output_tokens = usage_u64_field(
        meta,
        &["output_tokens", "outputTokens", "completion_tokens"],
    );
    let cache_read_tokens = usage_u64_field(
        meta,
        &[
            "cache_read_tokens",
            "cacheReadTokens",
            "cached_read_tokens",
            "cachedReadTokens",
            "cache_read_input_tokens",
            // Codex core reports cache hits under `cached_input_tokens`; map it
            // to the Kodex `cache_read_tokens` slot.
            "cached_input_tokens",
            "cachedInputTokens",
        ],
    );
    let cache_write_tokens = usage_u64_field(
        meta,
        &[
            "cache_write_tokens",
            "cacheWriteTokens",
            "cached_write_tokens",
            "cachedWriteTokens",
            "cache_creation_input_tokens",
        ],
    );
    let reasoning_tokens = usage_u64_field(
        meta,
        &[
            "reasoning_tokens",
            "reasoningTokens",
            // Codex core's per-token-type field is `reasoning_output_tokens`.
            "reasoning_output_tokens",
            "reasoningOutputTokens",
        ],
    );
    // Fallback when the agent did not report an authoritative total:
    // `input_tokens` already includes cache hits (`cache_read_tokens` is the
    // cached subset of the prompt) and `output_tokens` already includes
    // reasoning tokens, so the total is simply input + output. Including the
    // cache/reasoning breakdown fields would double-count them.
    let total_tokens =
        usage_u64_field(meta, &["total_tokens", "totalTokens", "total"]).or_else(|| {
            if input_tokens.is_some() || output_tokens.is_some() {
                Some(input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0))
            } else {
                None
            }
        });

    let latency_ms = usage_u64_field(meta, &["latency_ms", "latencyMs"]);
    let ttft_ms = usage_u64_field(meta, &["ttft_ms", "ttftMs"]);
    let tokens_per_second =
        usage_f64_field(meta, &["tokens_per_second", "tokensPerSecond"]);

    UsageTokenBreakdown {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
        latency_ms,
        ttft_ms,
        tokens_per_second,
    }
}

fn usage_f64_field(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|field| {
            field
                .as_f64()
                .or_else(|| field.as_i64().map(|v| v as f64))
                .or_else(|| field.as_u64().map(|v| v as f64))
                .or_else(|| field.as_str().and_then(|v| v.trim().parse().ok()))
        })
    })
}

fn usage_u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|field| {
            field
                .as_u64()
                .or_else(|| field.as_i64().and_then(|value| u64::try_from(value).ok()))
                .or_else(|| field.as_str().and_then(|value| value.trim().parse().ok()))
        })
    })
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

    if text.is_empty() {
        return Ok(());
    }

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

fn emit_tool_update(tx: &mpsc::Sender<ClientEvent>, update: ToolCallUpdate) -> anyhow::Result<()> {
    let id = update.tool_call_id.0.to_string();
    let title = update.fields.title.clone().unwrap_or_else(|| "tool".into());

    if let Some(content) = update.fields.content.clone() {
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
            ToolCallStatus::InProgress | ToolCallStatus::Pending => {
                emit_non_terminal_tool_update(tx, id, update.fields, Some(status))?
            }
            _ => emit_non_terminal_tool_update(tx, id, update.fields, Some(status))?,
        }
    } else {
        emit_non_terminal_tool_update(tx, id, update.fields, None)?;
    }

    Ok(())
}

fn emit_non_terminal_tool_update(
    tx: &mpsc::Sender<ClientEvent>,
    id: String,
    fields: ToolCallUpdateFields,
    status: Option<ToolCallStatus>,
) -> anyhow::Result<()> {
    let summary = status
        .map(|status| format_tool_update_summary(status, fields.raw_output.as_ref()))
        .or_else(|| fields.raw_output.as_ref().map(format_value_for_summary));
    if fields.raw_output.is_none()
        && fields.raw_input.is_none()
        && fields.title.is_none()
        && fields.kind.is_none()
    {
        if let Some(summary) = summary {
            tx.send(ClientEvent::ToolProgress {
                id,
                content: summary,
            })
            .map_err(|_| anyhow!("failed to emit tool status"))?;
        }
        return Ok(());
    }

    let terminal_output = fields.raw_output.as_ref().and_then(parse_terminal_output);
    tx.send(ClientEvent::ToolUpdated {
        id,
        parent_id: None,
        name: fields.title,
        kind: fields.kind.map(|kind| format!("{kind:?}")),
        summary,
        is_subagent: false,
        raw_input: fields.raw_input.as_ref().map(format_json),
        raw_output: fields.raw_output.as_ref().map(format_value_for_ui),
        terminal_output,
        is_partial: false,
    })
    .map_err(|_| anyhow!("failed to emit tool update"))
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
mod tests;
