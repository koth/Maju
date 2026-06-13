use crate::bootstrap::{build_initial_remote_ui, build_initial_ui, update_initial_agent_notice};
use crate::file_tracker::FileChangeTracker;
use crate::paths::AppPaths;
use crate::reducer::apply_event;
use acp_core::{ClientEvent, PromptTask, RemoteSshSessionConfig, SessionConfig, SessionHandle};
use git_service::GitService;
use session_store::SessionStore;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};
use workspace_model::{
    AgentCliId, ChatMessage, MessageRole, RemoteLinuxWorkspace, SessionAttentionState,
    SessionConfigSource, SessionListItem, SessionRuntimeStatus, SessionStatus, TimelineItem,
    ToolInvocation, ToolLogEntry, ToolStatus, UserPromptContent,
};

mod bootstrap;
mod change_sets;
mod config;
pub(crate) mod diff_utils;
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
    normalize_diff_text_for_session_change, raw_input_has_write_payload, tool_event_hint_paths,
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
const BACKGROUND_RUNTIME_IDLE_GRACE: Duration = Duration::from_secs(10 * 60);
const PENDING_TOOL_WRITE_DETECTION_TTL: Duration = Duration::from_secs(5);
const PENDING_TOOL_WRITE_SETTLE_DELAY: Duration = Duration::from_secs(1);
const PENDING_TOOL_RETRY_INTERVAL: Duration = Duration::from_millis(250);

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

struct PendingToolWriteDetection {
    call_id: String,
    turn_user_message_id: Option<uuid::Uuid>,
    next_retry_at: Instant,
    expires_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelSelection {
    value: String,
    provider: Option<String>,
}

impl ModelSelection {
    fn new(value: impl Into<String>, provider: Option<String>) -> Self {
        Self {
            value: value.into(),
            provider,
        }
    }
}

struct SessionRuntime {
    local_session_id: uuid::Uuid,
    ui: workspace_model::UiSnapshot,
    session: SessionHandle,
    agent_command: String,
    in_flight_prompt: Option<InFlightPrompt>,
    seq_counter: i64,
    needs_title: bool,
    agent_title_received: bool,
    provisional_prompt_title: Option<String>,
    skip_replay: bool,
    pending_model_restore: Option<ModelSelection>,
    authoritative_model_selection: Option<ModelSelection>,
    file_tracker: FileChangeTracker,
    dirty_tool_call_ids: HashSet<String>,
    review_changes_started: bool,
    current_turn_user_message_id: Option<uuid::Uuid>,
    pending_tool_write_detections: Vec<PendingToolWriteDetection>,
    inline_think_filter: InlineThinkFilter,
    last_viewed: Instant,
    idle_since: Option<Instant>,
    runtime_status: SessionRuntimeStatus,
    attention_state: SessionAttentionState,
}

impl SessionRuntime {
    fn is_in_flight(&self) -> bool {
        self.in_flight_prompt.is_some()
    }
}

#[derive(Default)]
struct SessionRuntimeRegistry {
    entries: HashMap<String, SessionRuntime>,
    retained_attention: HashMap<String, SessionAttentionState>,
}

impl SessionRuntimeRegistry {
    fn insert(&mut self, runtime: SessionRuntime) {
        self.retained_attention
            .remove(&runtime.local_session_id.to_string());
        self.entries
            .insert(runtime.local_session_id.to_string(), runtime);
    }

    fn remove(&mut self, session_id: &str) -> Option<SessionRuntime> {
        self.entries.remove(session_id)
    }

    fn remove_all_state(&mut self, session_id: &str) -> Option<SessionRuntime> {
        self.retained_attention.remove(session_id);
        self.entries.remove(session_id)
    }

    fn clear_attention(&mut self, session_id: &str) {
        self.retained_attention.remove(session_id);
        if let Some(runtime) = self.entries.get_mut(session_id) {
            runtime.attention_state = SessionAttentionState::None;
        }
    }

    fn retain_attention_after_retirement(
        &mut self,
        session_id: String,
        attention: SessionAttentionState,
    ) {
        if !matches!(attention, SessionAttentionState::None) {
            self.retained_attention.insert(session_id, attention);
        }
    }

    fn annotate_sessions(&self, sessions: &mut [SessionListItem], visible_session_id: &str) {
        for item in sessions {
            if item.id == visible_session_id {
                item.runtime_status = SessionRuntimeStatus::Active;
                item.attention_state = SessionAttentionState::None;
                continue;
            }

            if let Some(runtime) = self.entries.get(&item.id) {
                item.runtime_status = runtime.runtime_status.clone();
                item.attention_state = runtime.attention_state.clone();
            } else if let Some(attention) = self.retained_attention.get(&item.id) {
                item.runtime_status = SessionRuntimeStatus::None;
                item.attention_state = attention.clone();
            } else {
                item.runtime_status = SessionRuntimeStatus::None;
                item.attention_state = SessionAttentionState::None;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct RuntimeClock {
    fixed_now: Option<Instant>,
}

impl RuntimeClock {
    fn now(&self) -> Instant {
        self.fixed_now.unwrap_or_else(Instant::now)
    }
}

impl Default for RuntimeClock {
    fn default() -> Self {
        Self { fixed_now: None }
    }
}

pub struct Application {
    pub ui: workspace_model::UiSnapshot,
    session: SessionHandle,
    runtime_registry: SessionRuntimeRegistry,
    runtime_clock: RuntimeClock,
    store: SessionStore,
    app_paths: AppPaths,
    pub agent_command: String,
    acp_port: u16,
    remote_ssh: Option<RemoteSshSessionConfig>,
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
    pending_model_restore: Option<ModelSelection>,
    authoritative_model_selection: Option<ModelSelection>,
    file_tracker: FileChangeTracker,
    dirty_tool_call_ids: HashSet<String>,
    review_changes_started: bool,
    current_turn_user_message_id: Option<uuid::Uuid>,
    pending_tool_write_detections: Vec<PendingToolWriteDetection>,
    inline_think_filter: InlineThinkFilter,
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{millis}")
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
    let reason = unpack_acp_internal_error(reason).unwrap_or_else(|| reason.trim().to_string());
    let lower = reason.to_ascii_lowercase();
    if lower.contains("requested token count exceeds")
        || lower.contains("maximum context length")
        || lower.contains("context_length_exceeded")
    {
        return "模型上下文超限：本轮携带的历史消息或工具输出太多，超过了上游模型窗口。请新建会话或压缩上下文后重试。".into();
    }

    if lower.contains("streamable-http acp request failed with status 409")
        && lower.contains("connection not found")
    {
        return "CodeBuddy ACP 连接状态已失效：远程服务端没有找到当前连接。请重新打开远程目录；如果仍然复现，请重启远程 CodeBuddy Agent 后重试。".into();
    }

    if lower.contains("ssh remote agent process ended before readiness was reported") {
        return remote_agent_readiness_notice(&reason, "远程 ACP Agent 启动后在报告就绪前退出");
    }
    if lower.contains("timed out waiting for remote acp agent readiness") {
        return remote_agent_readiness_notice(&reason, "等待远程 ACP Agent 就绪超时");
    }
    if lower.contains("ssh remote agent process exited before acp tcp became reachable") {
        return "远程 ACP Agent 启动后退出，尚未建立可用的 ACP TCP 连接。请检查远程 Agent 配置、模型凭据和远程目录后重试。".into();
    }

    reason
}

fn unpack_acp_internal_error(reason: &str) -> Option<String> {
    let rest = reason.trim().strip_prefix("Internal error:")?.trim();
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(rest)
        && let Some(data) = value.get("data").and_then(|data| data.as_str())
    {
        let data = data.trim();
        if !data.is_empty() {
            return Some(data.to_string());
        }
    }
    Some(rest.to_string())
}

fn remote_agent_readiness_notice(reason: &str, summary: &str) -> String {
    let diagnostic = reason
        .split_once(':')
        .map(|(_, diagnostic)| diagnostic.trim())
        .filter(|diagnostic| !diagnostic.is_empty());
    match diagnostic {
        Some(diagnostic) => format!("{summary}：{diagnostic}"),
        None => {
            format!(
                "{summary}。请检查远程 Agent 是否能在该目录启动，以及远程机器上的模型凭据/配置。"
            )
        }
    }
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

fn is_codebuddy_agent_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    normalized == "codebuddy" || normalized == "codebuddy-acp"
}

fn normalize_title_for_prompt_compare(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn display_codex_provider(provider: &str) -> &str {
    match provider {
        "default" => "默认",
        "byok" => "BYOK",
        "timiai" => "TimiAI",
        "deepseek" => "DeepSeek",
        other => other,
    }
}

impl Application {
    pub(super) fn bump_revision(&mut self) {
        self.ui.revision = self.ui.revision.saturating_add(1);
    }

    pub(super) fn runtime_now(&self) -> Instant {
        self.runtime_clock.now()
    }

    #[cfg(test)]
    pub(super) fn set_runtime_clock_now(&mut self, now: Instant) {
        self.runtime_clock.fixed_now = Some(now);
    }

    #[cfg(test)]
    pub(super) fn advance_runtime_clock(&mut self, duration: Duration) {
        let now = self.runtime_now() + duration;
        self.runtime_clock.fixed_now = Some(now);
    }

    fn swap_visible_state_with_runtime(&mut self, runtime: &mut SessionRuntime) {
        std::mem::swap(&mut self.ui, &mut runtime.ui);
        std::mem::swap(&mut self.session, &mut runtime.session);
        std::mem::swap(&mut self.agent_command, &mut runtime.agent_command);
        std::mem::swap(&mut self.in_flight_prompt, &mut runtime.in_flight_prompt);
        std::mem::swap(&mut self.seq_counter, &mut runtime.seq_counter);
        std::mem::swap(&mut self.needs_title, &mut runtime.needs_title);
        std::mem::swap(
            &mut self.agent_title_received,
            &mut runtime.agent_title_received,
        );
        std::mem::swap(
            &mut self.provisional_prompt_title,
            &mut runtime.provisional_prompt_title,
        );
        std::mem::swap(&mut self.skip_replay, &mut runtime.skip_replay);
        std::mem::swap(
            &mut self.pending_model_restore,
            &mut runtime.pending_model_restore,
        );
        std::mem::swap(
            &mut self.authoritative_model_selection,
            &mut runtime.authoritative_model_selection,
        );
        std::mem::swap(&mut self.file_tracker, &mut runtime.file_tracker);
        std::mem::swap(
            &mut self.dirty_tool_call_ids,
            &mut runtime.dirty_tool_call_ids,
        );
        std::mem::swap(
            &mut self.review_changes_started,
            &mut runtime.review_changes_started,
        );
        std::mem::swap(
            &mut self.current_turn_user_message_id,
            &mut runtime.current_turn_user_message_id,
        );
        std::mem::swap(
            &mut self.pending_tool_write_detections,
            &mut runtime.pending_tool_write_detections,
        );
        std::mem::swap(
            &mut self.inline_think_filter,
            &mut runtime.inline_think_filter,
        );
    }

    fn install_runtime_as_visible(&mut self, mut runtime: SessionRuntime) -> SessionRuntime {
        let now = self.runtime_now();
        let old_visible_session_id = self.ui.session.id;
        let old_was_in_flight = self.in_flight_prompt.is_some();
        self.swap_visible_state_with_runtime(&mut runtime);

        runtime.local_session_id = old_visible_session_id;
        runtime.last_viewed = now;
        runtime.attention_state = SessionAttentionState::None;
        if old_was_in_flight {
            runtime.runtime_status = SessionRuntimeStatus::BackgroundRunning;
            runtime.idle_since = None;
        } else {
            runtime.runtime_status = SessionRuntimeStatus::BackgroundIdle;
            runtime.idle_since = Some(now);
        }

        self.runtime_registry
            .clear_attention(&self.ui.session.id.to_string());
        runtime
    }

    fn runtime_has_pending_permission(&self) -> bool {
        self.ui.tools.iter().any(|tool| {
            tool.kind == "permission"
                && tool.permission_decision.is_none()
                && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
        })
    }

    fn runtime_needs_attention(&self) -> bool {
        self.runtime_has_pending_permission()
            || matches!(self.ui.session.status, SessionStatus::Interrupted)
    }

    pub fn remote_ssh_session_config(&self) -> Option<RemoteSshSessionConfig> {
        self.remote_ssh.clone()
    }

    pub fn is_remote_workspace(&self) -> bool {
        matches!(
            self.ui.workspace.location,
            workspace_model::WorkspaceLocation::RemoteLinux(_)
        )
    }

    pub(crate) fn ensure_local_workspace_for(&self, operation: &str) -> Result<(), String> {
        if self.is_remote_workspace() {
            Err(format!("Remote workspaces do not support {operation} yet"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        self.session.shutdown();
        for runtime in self.runtime_registry.entries.values_mut() {
            runtime.session.shutdown();
        }
    }
}
