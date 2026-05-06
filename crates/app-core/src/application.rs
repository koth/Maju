use crate::bootstrap::build_initial_ui;
use crate::file_tracker::FileChangeTracker;
use crate::paths::AppPaths;
use crate::reducer::apply_event;
use acp_core::{ClientEvent, PromptTask, SessionConfig, SessionHandle, diff_to_hunks};
use git_service::GitService;
use session_store::SessionStore;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use workspace_model::{
    AgentCliId, ChatMessage, DiffHunk, DiffLineKind, MessageRole, SessionConfigSource,
    SessionFileChange, SessionListItem, SessionStatus, TimelineItem, ToolDiffPreview,
    ToolInvocation, ToolLogEntry, ToolStatus, UserPromptContent,
};

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
    /// When true, discard replay events from session/load until user sends first prompt
    skip_replay: bool,
    pending_model_restore: Option<String>,
    file_tracker: FileChangeTracker,
}

pub fn normalize_tracked_path(path: &str) -> String {
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

pub fn normalize_path_for_storage(path: &str, workspace_root: &Path) -> String {
    let normalized = normalize_tracked_path(path);
    let ws_root = normalize_tracked_path(&workspace_root.display().to_string());
    let ws_prefix = if ws_root.ends_with('/') {
        ws_root
    } else {
        format!("{}/", ws_root)
    };
    normalized
        .strip_prefix(&ws_prefix)
        .unwrap_or(&normalized)
        .to_string()
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn tool_diff_hunks(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    diff_to_hunks(previous_session_new_text.or(tool_old_text), tool_new_text)
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

fn prompt_text(prompt: &[UserPromptContent]) -> Option<String> {
    let text = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => Some(text.trim()),
            UserPromptContent::Image { .. } | UserPromptContent::File { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if text.is_empty() { None } else { Some(text) }
}

fn prompt_has_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

fn prompt_has_file(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

fn markdown_image_alt(name: Option<&str>) -> String {
    name.unwrap_or("attached image")
        .replace(['\n', '\r', '[', ']'], " ")
        .trim()
        .to_string()
}

fn prompt_display_body(prompt: &[UserPromptContent]) -> String {
    let mut parts = Vec::new();
    if let Some(text) = prompt_text(prompt) {
        parts.push(text);
    }
    parts.extend(prompt.iter().filter_map(|content| match content {
        UserPromptContent::Image {
            name,
            thumbnail_data,
            thumbnail_mime_type,
            ..
        } => {
            let alt = markdown_image_alt(name.as_deref());
            thumbnail_data.as_ref().map_or_else(
                || Some(format!("[Image: {alt}]")),
                |data| {
                    let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                    Some(format!("![Image: {alt}](data:{mime_type};base64,{data})"))
                },
            )
        }
        UserPromptContent::File { name, .. } => Some(format!("[File: {name}]")),
        UserPromptContent::Text { .. } => None,
    }));
    parts.join("\n\n")
}

impl Application {
    pub fn bootstrap(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::bootstrap_with_app_paths(workspace_root, agent_command, AppPaths::resolve()?)
    }

    pub fn bootstrap_with_app_paths(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let workspace_root = workspace_root.as_ref();
        let agent_command = agent_command.into();
        app_paths.ensure_standard_dirs()?;
        let mut ui = build_initial_ui(workspace_root)?;

        let store = SessionStore::open(app_paths.root(), workspace_root)?;

        // Read ACP port from settings (used by opencode for TCP transport)
        let settings = crate::settings::load_app_settings(&app_paths);
        let acp_port = settings.acp_port;

        // Check for existing session and its ACP session ID for --resume
        let resume_session_id = store
            .list_sessions()
            .ok()
            .and_then(|sessions| sessions.first().and_then(|s| s.acp_session_id.clone()));

        // If resuming an existing session, skip replay events from session/load
        let skip_replay = resume_session_id.is_some();

        let session = SessionHandle::start(SessionConfig {
            workspace_root: ui.workspace.root.display().to_string(),
            app_data_root: app_paths.root().display().to_string(),
            model: ui.session.model.clone(),
            agent_command: agent_command.clone(),
            resume_session_id,
            log_id: make_log_id(),
            acp_port,
        })?;

        // Try to restore the most recent session, otherwise create a new one
        let (needs_title, seq_counter, pending_model_restore) = match store.list_sessions() {
            Ok(sessions) if !sessions.is_empty() => {
                let recent = &sessions[0]; // list_sessions orders by updated_at DESC
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) = store.load_session(session_id) {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    let mut tools = tools;
                    let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
                    for tool_id in &interrupted_tool_ids {
                        if let Some(tool) =
                            tools.iter().find(|tool| tool.id.to_string() == *tool_id)
                        {
                            let _ = store.update_tool(
                                tool_id,
                                "Interrupted",
                                tool.raw_output.as_deref(),
                                tool.error.as_deref(),
                            );
                        }
                    }
                    let mut pending_model_restore = None;
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        pending_model_restore = Some(model.clone());
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    // Restore file changes from SQLite
                    ui.session_changes = store.load_file_changes(session_id).unwrap_or_default();
                    let seq = store.next_seq(session_id).unwrap_or(1);
                    let needs_title = recent.title == "新会话";
                    (needs_title, seq, pending_model_restore)
                } else {
                    // Failed to load — create new session
                    let session_id = ui.session.id.to_string();
                    store.create_session(&session_id, &ui.session.model)?;
                    (true, 1, None)
                }
            }
            _ => {
                // No sessions exist — create a new one
                let session_id = ui.session.id.to_string();
                store.create_session(&session_id, &ui.session.model)?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        // Determine which agent CLI this session is using
        let agent_cli_label = crate::settings::agent_label_for_command(&agent_command);
        ui.session.agent_cli = Some(agent_cli_label);
        let _ = store.update_session_agent_cli(
            &ui.session.id.to_string(),
            ui.session.agent_cli.as_deref().unwrap_or("CodeBuddy"),
        );
        let _ = session.set_permission_mode(ui.session.mode.as_deref().unwrap_or("Build"));
        let _ = store.update_session_model_mode(
            &ui.session.id.to_string(),
            &ui.session.model,
            ui.session.mode.as_deref(),
        );

        Ok(Self {
            ui,
            session,
            store,
            app_paths,
            agent_command,
            acp_port,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            skip_replay,
            pending_model_restore,
            file_tracker: FileChangeTracker::new(workspace_root),
        })
    }

    pub fn send_prompt(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;
        let events = self.session.send_prompt(prompt)?;
        for event in events {
            apply_event(&mut self.ui, event.clone());
            self.persist_event(&event);
        }
        self.ui.session.status = SessionStatus::Idle;
        Ok(())
    }

    pub fn send_prompt_background(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.send_prompt_content_background(vec![UserPromptContent::text(prompt.into())])
    }

    pub fn send_prompt_content_background(
        &mut self,
        prompt: Vec<UserPromptContent>,
    ) -> anyhow::Result<()> {
        if self.in_flight_prompt.is_some() {
            let error = anyhow::anyhow!("提示请求已在运行中");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "图片提示".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("提示内容不能为空");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_image(&prompt) && !self.ui.prompt_capabilities.image {
            let error = anyhow::anyhow!("当前智能体不支持图片提示");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_file(&prompt) && !self.ui.prompt_capabilities.embedded_context {
            let error = anyhow::anyhow!("当前智能体不支持文件附件");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        if !self.session.is_alive() {
            if self.session.last_error().is_none() && self.should_auto_reconnect_after_clean_exit()
            {
                self.reconnect_session().map_err(anyhow::Error::msg)?;
            } else {
                let reason = self
                    .session
                    .last_error()
                    .unwrap_or_else(|| "ACP 子进程意外退出".to_string());
                let error = anyhow::anyhow!(reason);
                self.push_system_message(format!("会话已断开：{error}"));
                return Err(error);
            }
        }

        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::User,
            body: display_body,
        };

        // Persist user message to SQLite
        let seq = self.next_seq();
        let _ = self.store.insert_message(
            &self.ui.session.id.to_string(),
            &message.id.to_string(),
            "User",
            &message.body,
            seq,
        );

        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;

        // Step 1: Immediately set a truncated title from user prompt (no delay)
        if self.needs_title && self.ui.session.title == "新会话" {
            let title = extract_title_from_prompt(&title_source);
            self.ui.session.title = title.clone();
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }

        // User is sending a new prompt — drain any buffered replay events
        // from session/load before sending, so they don't mix with real responses.
        if self.skip_replay {
            self.session.drain_events();
            self.skip_replay = false;
        }

        let task = self.session.send_prompt_content_async(prompt)?;
        self.in_flight_prompt = Some(InFlightPrompt { task });
        Ok(())
    }

    pub fn poll_prompt_progress(&mut self) {
        // Detect subprocess crash even when no prompt is in flight
        if self.in_flight_prompt.is_none()
            && !self.session.is_alive()
            && self.ui.session.status != SessionStatus::Interrupted
        {
            let last_error = self.session.last_error();
            if last_error.is_none() && self.should_auto_reconnect_after_clean_exit() {
                if let Err(error) = self.reconnect_session() {
                    let reason = format!("ACP 子进程退出且重连失败：{error}");
                    apply_event(
                        &mut self.ui,
                        ClientEvent::Interrupted {
                            reason: reason.clone(),
                        },
                    );
                    self.push_system_message(format!("会话已断开：{}", reason));
                }
                return;
            }

            let reason = last_error.unwrap_or_else(|| "ACP 子进程意外退出".to_string());
            apply_event(
                &mut self.ui,
                ClientEvent::Interrupted {
                    reason: reason.clone(),
                },
            );
            self.push_system_message(format!("会话已断开：{}", reason));
            return;
        }

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            self.session.update_session_id(&events);
            for event in events {
                self.apply_event_and_restore_model(event);
            }
            return;
        };

        let events = match in_flight.task.collect_ready_events(&mut self.session) {
            Ok(events) => events,
            Err(error) => {
                self.ui.session.status = SessionStatus::Interrupted;
                self.ui.agent_plan.clear();
                self.push_system_message(format!(
                    "从 `{}` 读取 ACP 事件失败：{}",
                    self.agent_command, error
                ));
                self.in_flight_prompt = None;
                return;
            }
        };

        let is_finished = in_flight.task.is_finished();

        // If skip_replay is active, discard all events except SessionStarted and TurnFinished.
        // These are replay events from session/load that we already have in SQLite.
        if self.skip_replay {
            // Only keep SessionStarted (to update the ACP session ID) and check for TurnFinished
            for event in &events {
                if let ClientEvent::SessionStarted { .. } = event {
                    self.session.update_session_id(&[event.clone()]);
                    self.persist_event(event);
                }
            }
            if is_finished {
                self.skip_replay = false;
                self.in_flight_prompt = None;
                self.ui.session.status = SessionStatus::Idle;
            }
            return;
        }

        // Preprocess ToolDiff events: fill in old_text from the correct baseline.
        // For the tool card diff, old_text should be "what was on disk when the tool started"
        // so the card shows what THIS tool changed.
        // For session-level changes, the reducer's upsert_session_change preserves the
        // first-ever baseline separately.
        let workspace_root = self.ui.workspace.root.clone();
        let mut events = events;
        let mut had_file_changes = false;
        for event in events.iter_mut() {
            if let ClientEvent::ToolDiff { path, old_text, .. } = event {
                had_file_changes = true;
                // Normalize path to workspace-relative with forward slashes
                let normalized = normalize_path_for_storage(path, &workspace_root);
                let abs_path = workspace_root.join(&normalized);
                if old_text.is_none() {
                    // 1. file_tracker baseline: content at tool-start time (best for tool card)
                    if let Some(baseline) = self.file_tracker.get_any_baseline_text(&normalized) {
                        *old_text = Some(baseline.to_string());
                    }
                    // 2. session_changes new_text: the result of the previous edit = this edit's base
                    else if let Some(tracked) = self.ui.session_changes.iter().find(|c| {
                        normalize_tracked_path(&c.path) == normalize_tracked_path(&normalized)
                    }) {
                        *old_text = Some(tracked.new_text.clone());
                    }
                    // 3. last resort: read from disk (may be post-modification, but better than None)
                    else if let Ok(content) = std::fs::read_to_string(&abs_path) {
                        *old_text = Some(content);
                    }
                }
                *path = normalized;
            }
        }

        // Process events and track tool lifecycle for file change detection
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, .. } => {
                    self.file_tracker.start_recording(id, Vec::new());
                }
                ClientEvent::ToolCompleted { id, .. } | ClientEvent::ToolFailed { id, .. } => {
                    let changes = self.file_tracker.finish_recording(id);
                    had_file_changes |= self.apply_tracker_changes(id, changes);
                }
                _ => {}
            }
            apply_event(&mut self.ui, event.clone());
            self.persist_event(event);
        }
        self.session.update_session_id(&events);

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        had_file_changes |= self.detect_file_writes_from_tools();

        // Persist session_changes to SQLite after all file-change sources have run.
        if had_file_changes {
            self.persist_file_changes();
        }

        if is_finished {
            if self.ui.session.status == SessionStatus::Streaming {
                self.ui.session.status = SessionStatus::Idle;
            }

            // Step 2: After first turn, try to refine title from assistant's response
            if self.needs_title && !self.agent_title_received {
                self.needs_title = false;
                self.refine_session_title();
            }

            self.in_flight_prompt = None;
        }
    }

    pub fn has_in_flight_prompt(&self) -> bool {
        self.in_flight_prompt.is_some()
    }

    fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        if !self.agent_command.to_ascii_lowercase().contains("opencode") {
            return false;
        }

        !self.session.id.is_empty()
            || self
                .store
                .get_acp_session_id(&self.ui.session.id.to_string())
                .ok()
                .flatten()
                .is_some()
    }

    pub fn cancel_prompt(&mut self) -> Result<(), String> {
        if self.in_flight_prompt.is_none() {
            return Ok(());
        }
        self.session
            .cancel_prompt()
            .map_err(|error| error.to_string())?;
        self.mark_current_turn_cancelled();
        Ok(())
    }

    fn mark_current_turn_cancelled(&mut self) {
        let session_id = self.ui.session.id.to_string();
        let mut cancelled_tools = Vec::new();

        for tool in self
            .ui
            .tools
            .iter_mut()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
        {
            tool.status = ToolStatus::Interrupted;
            if tool.summary.trim().is_empty()
                || tool.summary == "等待活动"
                || tool.summary.starts_with("等待权限")
            {
                tool.summary = "已取消".into();
            }
            if tool.kind == "permission" && tool.permission_decision.is_none() {
                tool.permission_decision = Some("已取消".into());
            }
            tool.logs.push(ToolLogEntry {
                title: "已取消".into(),
                body: "客户端发送了 session/cancel 取消当前轮次".into(),
            });
            cancelled_tools.push(tool.clone());
        }

        for tool in cancelled_tools {
            let seq = self.next_seq();
            let _ = self.store.insert_tool(&session_id, &tool, seq);
        }
    }

    pub fn set_session_config_control(
        &mut self,
        control_id: &str,
        value_id: &str,
    ) -> Result<workspace_model::SessionConfigState, String> {
        if self.in_flight_prompt.is_some() || self.ui.session.status != SessionStatus::Idle {
            return Err("会话控件只能在会话空闲时更改".into());
        }

        let control = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.id == control_id)
            .cloned()
            .ok_or_else(|| format!("未知的会话控件：{control_id}"))?;

        if !control.enabled {
            return Err(format!("会话控件不可用：{}", control.label));
        }
        if !control.choices.iter().any(|choice| choice.id == value_id) {
            return Err(format!("{} 的值未知：{value_id}", control.label));
        }

        let events = match control.source {
            SessionConfigSource::ConfigOption => self
                .session
                .set_config_option(control.id, value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LegacyMode => self
                .session
                .set_mode(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LocalMode => {
                self.session
                    .set_permission_mode(value_id)
                    .map_err(|error| error.to_string())?;
                vec![ClientEvent::SessionConfigValueChanged {
                    control_id: control.id,
                    value_id: value_id.to_string(),
                    value_label: control
                        .choices
                        .iter()
                        .find(|choice| choice.id == value_id)
                        .map(|choice| choice.label.clone()),
                }]
            }
        };

        for event in events {
            apply_event(&mut self.ui, event.clone());
            self.persist_event(&event);
        }
        self.persist_session_model_mode();

        Ok(self.ui.session_config.clone())
    }

    pub fn resolve_tool_permission(
        &mut self,
        request_id: &str,
        option_id: Option<String>,
    ) -> Result<(), String> {
        self.session
            .resolve_permission(request_id, option_id)
            .map_err(|error| error.to_string())
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        self.store.list_sessions().map_err(|e| e.to_string())
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        // Load session data from SQLite
        let (messages, mut tools, timeline) =
            self.store.load_session(id).map_err(|e| e.to_string())?;
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = tools.iter().find(|tool| tool.id.to_string() == *tool_id) {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        let (model, mode) = self
            .store
            .get_session_model_mode(id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| (self.ui.session.model.clone(), self.ui.session.mode.clone()));
        let mode = mode.or_else(|| Some("Build".into()));

        // Get the stored ACP session ID for resume
        let resume_acp_id = self.store.get_acp_session_id(id).unwrap_or(None);

        // Start a new ACP session handle
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: self.agent_command.clone(),
            resume_session_id: resume_acp_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));

        // Update UI snapshot
        self.ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        self.ui.session.model = model;
        self.ui.session.mode = mode;
        self.ui.session.agent_cli = self
            .store
            .get_session_agent_cli(id)
            .unwrap_or(None)
            .or_else(|| {
                Some(crate::settings::agent_label_for_command(
                    &self.agent_command,
                ))
            });
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages = messages;
        self.ui.tools = tools;
        self.ui.timeline = timeline;
        self.ui.session.status = SessionStatus::Idle;
        self.session = session;
        self.in_flight_prompt = None;
        // Load file changes from SQLite for the switched session
        self.ui.session_changes = self.store.load_file_changes(id).unwrap_or_default();

        // Compute seq counter from loaded data
        self.seq_counter = self.store.next_seq(id).unwrap_or(1);

        // Load session title
        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            self.ui.session.title = s.title.clone();
        }

        self.needs_title = self.ui.session.title == "新会话";
        self.agent_title_received = false;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        Ok(())
    }

    pub fn session_create(&mut self, agent: Option<AgentCliId>) -> Result<(), String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let current_agent_command = match agent {
            Some(agent) => crate::settings::command_for_agent(agent).unwrap_or_else(|| {
                crate::settings::resolve_agent_command_with_settings(&self.app_paths)
            }),
            None => self.agent_command.clone(),
        };
        self.agent_command = current_agent_command;

        // Start a new ACP session handle (no resume for new session)
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: self.agent_command.clone(),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode("Build");

        self.ui.session.id = new_id;
        self.ui.session.title = "新会话".to_string();
        self.ui.session.model = initial_model;
        self.ui.session.mode = Some("Build".into());
        let agent_cli_label = crate::settings::agent_label_for_command(&self.agent_command);
        self.ui.session.agent_cli = Some(agent_cli_label);
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.session.status = SessionStatus::Idle;
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages.clear();
        self.ui.tools.clear();
        self.ui.timeline.clear();
        self.ui.session_changes.clear();
        self.session = session;
        self.in_flight_prompt = None;
        self.seq_counter = 1;
        self.needs_title = true;
        self.agent_title_received = false;
        self.pending_model_restore = None;
        self.persist_session_model_mode();
        let _ = self.store.update_session_agent_cli(
            &self.ui.session.id.to_string(),
            self.ui.session.agent_cli.as_deref().unwrap_or("CodeBuddy"),
        );

        Ok(())
    }

    pub fn session_delete(&mut self, id: &str) -> Result<(), String> {
        // Don't allow deleting the active session
        if self.ui.session.id.to_string() == id {
            return Err("无法删除当前活动的会话".to_string());
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        // Try to resume the current ACP session if we have its ID
        let resume_id = if !self.session.id.is_empty() {
            Some(self.session.id.clone())
        } else {
            self.store
                .get_acp_session_id(&self.ui.session.id.to_string())
                .unwrap_or(None)
        };

        let resume_id_for_handle = resume_id.clone();
        let has_resume_id = resume_id_for_handle.is_some();
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: self.agent_command.clone(),
            resume_session_id: resume_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        if let Some(acp_id) = resume_id_for_handle {
            session.id = acp_id;
        }

        self.session = session;
        self.ui.session.status = SessionStatus::Idle;
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut self.ui.tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = self
                .ui
                .tools
                .iter()
                .find(|tool| tool.id.to_string() == *tool_id)
            {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        self.in_flight_prompt = None;
        self.agent_title_received = false;
        self.skip_replay = has_resume_id;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        Ok(())
    }

    // ── Title refinement ──

    /// After the first turn completes, try to extract a better title from the
    /// assistant's response. The truncated user prompt is already set as Step 1.
    /// This Step 2 tries to improve it by looking at what the assistant actually did.
    fn refine_session_title(&mut self) {
        // Find first assistant message
        let assistant_body = match self
            .ui
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
        {
            Some(m) => m.body.clone(),
            None => return, // No assistant response yet, keep truncated title
        };

        // Try to extract a meaningful title from the assistant's first sentence.
        // Common patterns: "I'll help you X", "Let me X", "Here's how to X", etc.
        let refined = extract_title_from_response(&assistant_body);
        if let Some(title) = refined {
            self.ui.session.title = title.clone();
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }
        // If extraction fails, keep the truncated user prompt title from Step 1
    }

    // ── Internal helpers ──

    fn push_system_message(&mut self, body: impl Into<String>) {
        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::System,
            body: body.into(),
        };
        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
    }

    pub fn refresh_repository(&mut self) {
        if let Ok(snapshot) = GitService::open(&self.ui.workspace.root) {
            self.ui.repository = snapshot;
        }
    }

    pub fn stage_files(&mut self, paths: &[String]) -> Result<(), String> {
        GitService::stage(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    /// Persist current session_changes to SQLite.
    fn persist_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        for change in &self.ui.session_changes {
            let change_type = format!("{:?}", change.change_type);
            let _ = self.store.upsert_file_change(
                &session_id,
                &change.path,
                &change_type,
                change.old_text.as_deref(),
                &change.new_text,
                change.added_lines,
                change.removed_lines,
            );
        }
    }

    fn next_seq(&mut self) -> i64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    fn persist_event(&mut self, event: &ClientEvent) {
        let session_id = self.ui.session.id.to_string();
        match event {
            ClientEvent::SessionStarted { session_id: acp_id } => {
                // Persist the ACP session ID for --resume on next startup
                let _ = self.store.update_acp_session_id(&session_id, acp_id);
            }
            ClientEvent::MessageChunk { role, .. } if *role != MessageRole::User => {
                // Find the last assistant message and update its body
                if let Some(msg) = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::Assistant)
                {
                    let _ = self
                        .store
                        .update_message_body(&msg.id.to_string(), &msg.body);
                }
            }
            ClientEvent::TurnFinished { .. } => {
                // Persist the final assistant message if not already persisted
                let msg_data = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::Assistant)
                    .map(|m| (m.id.to_string(), m.body.clone()));

                if let Some((id_str, body)) = msg_data {
                    let seq = self.next_seq();
                    if self
                        .store
                        .insert_message(&session_id, &id_str, "Assistant", &body, seq)
                        .is_err()
                    {
                        let _ = self.store.update_message_body(&id_str, &body);
                    }
                }
                let _ = self.store.update_session_status(&session_id, "Idle");
            }
            ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. } => {
                self.persist_session_model_mode();
            }
            ClientEvent::SessionTitleUpdated { title } => {
                self.agent_title_received = true;
                let _ = self.store.update_session_title(&session_id, title);
            }
            ClientEvent::ToolStarted { id, .. }
            | ClientEvent::ToolUpdated { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. } => {
                // Find the tool in the UI snapshot and persist its latest display state
                let tool_clone = self
                    .ui
                    .tools
                    .iter()
                    .find(|t| t.id.to_string() == *id || t.call_id == *id)
                    .cloned();

                if let Some(tool) = tool_clone {
                    let seq = self.next_seq();
                    let _ = self.store.insert_tool(&session_id, &tool, seq);
                }
            }
            _ => {}
        }
    }

    fn persist_session_model_mode(&self) {
        let _ = self.store.update_session_model_mode(
            &self.ui.session.id.to_string(),
            &self.ui.session.model,
            self.ui.session.mode.as_deref(),
        );
    }

    fn apply_event_and_restore_model(&mut self, event: ClientEvent) {
        let should_restore_model = matches!(event, ClientEvent::SessionConfigUpdated { .. });
        apply_event(&mut self.ui, event.clone());
        if should_restore_model {
            self.restore_pending_model_selection();
        }
        self.persist_event(&event);
    }

    fn restore_pending_model_selection(&mut self) {
        let Some(saved_model) = self.pending_model_restore.clone() else {
            return;
        };
        let Some(model_control) = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == workspace_model::SessionConfigCategory::Model)
        else {
            return;
        };

        if model_control.current_value_id == saved_model
            || model_control.current_value_label == saved_model
        {
            self.pending_model_restore = None;
            return;
        }

        let Some(choice) = model_control
            .choices
            .iter()
            .find(|choice| choice.id == saved_model || choice.label == saved_model)
            .cloned()
        else {
            self.pending_model_restore = None;
            return;
        };

        self.pending_model_restore = None;
        let Ok(events) = self.session.set_model(choice.id) else {
            return;
        };
        for event in events {
            apply_event(&mut self.ui, event.clone());
            self.persist_event(&event);
        }
    }

    /// Detect file writes from completed tool calls by examining tool summaries/titles.
    /// Apply verified file changes from the tracker to session state and tool diff previews.
    fn apply_tracker_changes(
        &mut self,
        call_id: &str,
        changes: Vec<crate::file_tracker::VerifiedFileChange>,
    ) -> bool {
        use acp_core::diff_to_hunks;

        let mut changed = false;
        for change in changes {
            let normalized = normalize_tracked_path(&change.path);
            let existing_index = self
                .ui
                .session_changes
                .iter()
                .position(|c| normalize_tracked_path(&c.path) == normalized);
            let previous_session_new_text =
                existing_index.map(|index| self.ui.session_changes[index].new_text.clone());
            let effective_old_text = existing_index
                .and_then(|index| self.ui.session_changes[index].old_text.clone())
                .or_else(|| change.old_text.clone());
            let session_hunks = if change.skipped_diff {
                Vec::new()
            } else {
                diff_to_hunks(effective_old_text.as_deref(), &change.new_text)
            };
            let tool_hunks = if change.skipped_diff {
                Vec::new()
            } else if previous_session_new_text.is_none() && !change.hunks.is_empty() {
                change.hunks.clone()
            } else {
                tool_diff_hunks(
                    previous_session_new_text.as_deref(),
                    change.old_text.as_deref(),
                    &change.new_text,
                )
            };

            if !change.skipped_diff {
                if effective_old_text.as_deref().unwrap_or_default() == change.new_text {
                    if let Some(index) = existing_index {
                        self.ui.session_changes.remove(index);
                        changed = true;
                    }
                    self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
                    continue;
                }

                let added = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Added)
                    .count();
                let removed = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Removed)
                    .count();

                if added == 0 && removed == 0 {
                    continue;
                }

                if let Some(index) = existing_index {
                    let existing = &mut self.ui.session_changes[index];
                    if existing.old_text.is_none() {
                        existing.old_text = effective_old_text.clone();
                    }
                    existing.new_text = change.new_text.clone();
                    existing.change_type = change.change_type.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                } else {
                    self.ui.session_changes.push(SessionFileChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        old_text: effective_old_text.clone(),
                        new_text: change.new_text.clone(),
                        added_lines: added,
                        removed_lines: removed,
                        timestamp: current_timestamp(),
                    });
                }
                changed = true;
            }

            // Attach only this tool's diff preview, not the cumulative session diff.
            self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
        }
        changed
    }

    fn attach_tool_diff_preview(
        &mut self,
        call_id: &str,
        path: &str,
        normalized_path: &str,
        hunks: Vec<DiffHunk>,
    ) {
        if hunks.is_empty() {
            return;
        }
        let Some(tool) = self.ui.tools.iter_mut().find(|t| t.call_id == call_id) else {
            return;
        };

        let path_buf = PathBuf::from(path);
        if !tool
            .diff_paths
            .iter()
            .any(|p| normalize_tracked_path(&p.display().to_string()) == normalized_path)
        {
            tool.diff_paths.push(path_buf.clone());
        }
        if let Some(preview) = tool
            .diff_previews
            .iter_mut()
            .find(|p| normalize_tracked_path(&p.path.display().to_string()) == normalized_path)
        {
            preview.path = path_buf;
            preview.hunks = hunks;
        } else {
            tool.diff_previews.push(ToolDiffPreview {
                path: path_buf,
                hunks,
            });
        }
    }

    /// CodeBuddy agent uses terminal commands (cat > file << 'EOF') to write files,
    /// so we can't rely on ToolDiff events from the ACP protocol. Instead, we check
    /// completed tools for edit-related patterns and read the current file content.
    fn detect_file_writes_from_tools(&mut self) -> bool {
        // Normalize path for comparison: forward slashes, lowercase drive letter on Windows
        fn normalize_path(p: &str) -> String {
            normalize_tracked_path(p)
        }

        let workspace_root = self.ui.workspace.root.clone();
        let mut changed = false;

        // Collect normalized paths already tracked in session_changes
        let tracked_paths: HashSet<String> = self
            .ui
            .session_changes
            .iter()
            .map(|c| normalize_path(&c.path))
            .collect();

        // Look at recently completed tools for file write patterns
        let write_paths: Vec<(String, String)> = self
            .ui
            .tools
            .iter()
            .filter(|t| t.status == ToolStatus::Succeeded)
            .filter_map(|tool| {
                // Check diff_paths first (set by ToolDiff events from ACP WriteTextFileRequest).
                // Only treat it as a real write when the preview has actual changed lines;
                // a path-only or context-only preview is often just a no-op edit notification.
                if let Some(preview) = tool.diff_previews.iter().find(|preview| {
                    preview.hunks.iter().any(|hunk| {
                        hunk.lines.iter().any(|line| {
                            matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed)
                        })
                    })
                }) {
                    let path = preview.path.display().to_string();
                    if !tracked_paths.contains(&normalize_path(&path)) {
                        return Some((tool.call_id.clone(), path));
                    }
                }

                // Check summary for "Editing <path>" pattern
                if tool.summary.starts_with("Editing ") {
                    let path = tool.summary.trim_start_matches("Editing ").to_string();
                    if !tracked_paths.contains(&normalize_path(&path)) {
                        return Some((tool.call_id.clone(), path));
                    }
                }

                // Check raw_input JSON for file_path/filePath/path fields in edit/write tools
                if is_file_write_tool_identity(&tool.kind, &tool.name) {
                    if let Some(ref input) = tool.raw_input {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
                            let file_path = json
                                .get("file_path")
                                .or_else(|| json.get("filePath"))
                                .or_else(|| json.get("path"))
                                .and_then(|v| v.as_str());
                            if let Some(path) = file_path {
                                if !tracked_paths.contains(&normalize_path(path)) {
                                    return Some((tool.call_id.clone(), path.to_string()));
                                }
                            }
                        }
                    }
                }

                None
            })
            .collect();

        // For each detected file write, read current content and update only
        // already-tracked changes. Creating a new change without a baseline makes
        // the UI render the whole file as added, so the file tracker must be the
        // source of new session_changes.
        for (call_id, path) in write_paths {
            let normalized = normalize_path_for_storage(&path, &workspace_root);
            let abs_path = workspace_root.join(&normalized);
            if let Ok(new_text) = std::fs::read_to_string(&abs_path)
                && let Some(index) = self
                    .ui
                    .session_changes
                    .iter()
                    .position(|c| normalize_path(&c.path) == normalized)
            {
                let old_text = self.ui.session_changes[index].old_text.clone();
                let previous_session_new_text = self.ui.session_changes[index].new_text.clone();
                let tool_hunks = tool_diff_hunks(Some(&previous_session_new_text), None, &new_text);
                if old_text.as_deref().unwrap_or_default() == new_text {
                    self.ui.session_changes.remove(index);
                    changed = true;
                    self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
                    continue;
                }

                let session_hunks = diff_to_hunks(old_text.as_deref(), &new_text);
                let added = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Added)
                    .count();
                let removed = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Removed)
                    .count();
                if added == 0 && removed == 0 {
                    continue;
                }

                let existing = &mut self.ui.session_changes[index];
                existing.new_text = new_text;
                existing.added_lines = added;
                existing.removed_lines = removed;
                existing.timestamp = current_timestamp();
                changed = true;

                self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
            }
        }

        changed
    }
}

fn is_file_write_tool_identity(kind: &str, name: &str) -> bool {
    kind_and_name_tokens(kind, name).any(|token| {
        matches!(
            token.as_str(),
            "edit" | "write" | "patch" | "applypatch" | "apply_patch" | "apply-patch"
        )
    })
}

fn kind_and_name_tokens<'a>(kind: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    kind.split(|ch: char| !ch.is_ascii_alphanumeric())
        .chain(name.split(|ch: char| !ch.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
}

/// Extract a concise session title from the user's first prompt.
/// Takes the first line, strips common prefixes, and truncates to 60 chars.
fn extract_title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();

    // Strip common conversational prefixes
    let stripped = first_line
        .strip_prefix("Please ")
        .or_else(|| first_line.strip_prefix("please "))
        .or_else(|| first_line.strip_prefix("Help me "))
        .or_else(|| first_line.strip_prefix("help me "))
        .or_else(|| first_line.strip_prefix("Can you "))
        .or_else(|| first_line.strip_prefix("can you "))
        .or_else(|| first_line.strip_prefix("I want to "))
        .or_else(|| first_line.strip_prefix("I need to "))
        .unwrap_or(first_line)
        .trim();

    let text = if stripped.is_empty() {
        first_line
    } else {
        stripped
    };

    if text.chars().count() <= 60 {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::{is_file_write_tool_identity, tool_diff_hunks};
    use workspace_model::DiffLineKind;

    #[test]
    fn tool_diff_uses_previous_session_new_text_for_repeated_file_edits() {
        let hunks = tool_diff_hunks(Some("one\ntwo\n"), Some("one\n"), "one\nthree\n");
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

        assert_eq!(added, vec!["three"]);
        assert_eq!(removed, vec!["two"]);
    }

    #[test]
    fn write_tool_detection_does_not_match_editor_paths() {
        assert!(!is_file_write_tool_identity(
            "read",
            "docs\\editor-subsystem-design.md"
        ));
        assert!(!is_file_write_tool_identity(
            "read",
            "D:/work/kodex/docs/editor-subsystem-design.md"
        ));
        assert!(is_file_write_tool_identity("edit", "docs/architecture.md"));
        assert!(is_file_write_tool_identity("tool", "mcp__opencode__write"));
    }
}

/// Try to extract a refined title from the assistant's first response.
/// Returns None if no good title can be extracted (keeps the prompt-based title).
fn extract_title_from_response(response: &str) -> Option<String> {
    // Get the first meaningful line (skip empty lines and markdown headers)
    let first_line = response
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))?;

    // Strip common assistant prefixes to get the action description
    let prefixes = [
        "I'll help you ",
        "I'll ",
        "I will ",
        "Let me ",
        "Sure, I'll ",
        "Sure! I'll ",
        "OK, I'll ",
        "Alright, I'll ",
        "Here's how to ",
        "I can help with ",
        "I can help you ",
        // Chinese prefixes
        "我来帮你",
        "让我",
        "好的，我来",
        "好的，让我",
        "我会",
        "我将",
    ];

    let mut text = first_line;
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
            break;
        }
    }

    let text = text.trim_end_matches('.');
    let text = text.trim();

    // If too short or same as just a function word, not useful
    if text.len() < 5 {
        return None;
    }

    // Capitalize first letter
    let title = if text.starts_with(|c: char| c.is_lowercase()) {
        let mut chars = text.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => return None,
        }
    } else {
        text.to_string()
    };

    // Truncate to 60 chars
    if title.chars().count() <= 60 {
        Some(title)
    } else {
        let truncated: String = title.chars().take(57).collect();
        Some(format!("{truncated}..."))
    }
}
