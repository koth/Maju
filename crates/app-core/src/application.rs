use crate::bootstrap::{build_initial_ui, update_initial_agent_notice};
use crate::file_tracker::FileChangeTracker;
use crate::paths::AppPaths;
use crate::reducer::apply_event;
use acp_core::{ClientEvent, PromptTask, SessionConfig, SessionHandle};
use git_service::GitService;
use session_store::SessionStore;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use workspace_model::{
    AgentCliId, ChatMessage, MessageRole, SessionConfigSource, SessionListItem, SessionStatus,
    TimelineItem, ToolInvocation, ToolLogEntry, ToolStatus, UserPromptContent,
};

mod bootstrap;
mod change_sets;
mod config;
mod diff_utils;
mod events;
mod inline_think;
mod path_utils;
mod prompt_content;
mod prompting;
mod repository;
mod sessions;
mod shell_bridge;
#[cfg(test)]
mod tests;
mod titles;
mod tool_diffs;
mod ui_snapshot;
use diff_utils::{
    expand_tool_diff_fragment_from_disk, looks_like_fragment_to_full_file_text,
    normalize_diff_text_for_session_change, tool_event_hint_paths,
};
use inline_think::InlineThinkFilter;
pub use path_utils::{normalize_path_for_storage, normalize_tracked_path};
use prompt_content::{prompt_display_body, prompt_has_file, prompt_has_image, prompt_text};
use titles::{
    extract_title_from_prompt, extract_title_from_response, is_placeholder_session_title,
};
pub use ui_snapshot::{UiPatchCursor, UiSnapshotUpdate};

const AGENT_DEFAULT_MODEL_LABEL: &str = "Agent default";
const RESTORED_INCOMPLETE_TOOL_REASON: &str = "上次会话结束前未完成";

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
    authoritative_model_selection: Option<String>,
    file_tracker: FileChangeTracker,
    dirty_tool_call_ids: HashSet<String>,
    review_changes_started: bool,
    current_turn_user_message_id: Option<uuid::Uuid>,
    inline_think_filter: InlineThinkFilter,
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
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

fn humanize_acp_disconnect_reason(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("requested token count exceeds")
        || lower.contains("maximum context length")
        || lower.contains("context_length_exceeded")
    {
        return "模型上下文超限：本轮携带的历史消息或工具输出太多，超过了上游模型窗口。请新建会话或压缩上下文后重试。".into();
    }

    reason.to_string()
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

fn is_codex_agent_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    normalized == "codex" || normalized == "codex-acp" || normalized == "kodex-acp"
}

fn is_claude_agent_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "claude" | "claude-acp" | "claude-agent-acp" | "claude agent"
    )
}

fn normalize_title_for_prompt_compare(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn display_codex_provider(provider: &str) -> &str {
    match provider {
        "default" => "默认",
        "venus" => "Venus",
        "deepseek" => "DeepSeek",
        other => other,
    }
}

impl Application {
    pub(super) fn bump_revision(&mut self) {
        self.ui.revision = self.ui.revision.saturating_add(1);
    }
}
