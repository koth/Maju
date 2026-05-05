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
    ChatMessage, DiffLineKind, FileChangeType, MessageRole, SessionConfigSource, SessionFileChange,
    SessionListItem, SessionStatus, TimelineItem, ToolDiffPreview, ToolLogEntry, ToolStatus,
    UserPromptContent,
};

const AGENT_DEFAULT_MODEL_LABEL: &str = "Agent default";

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
    file_tracker: FileChangeTracker,
}

fn normalize_tracked_path(path: &str) -> String {
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
        let (needs_title, seq_counter) = match store.list_sessions() {
            Ok(sessions) if !sessions.is_empty() => {
                let recent = &sessions[0]; // list_sessions orders by updated_at DESC
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) = store.load_session(session_id) {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    // Restore file changes from SQLite
                    ui.session_changes = store.load_file_changes(session_id).unwrap_or_default();
                    let seq = store.next_seq(session_id).unwrap_or(1);
                    let needs_title = recent.title == "New Session";
                    (needs_title, seq)
                } else {
                    // Failed to load — create new session
                    let session_id = ui.session.id.to_string();
                    store.create_session(&session_id, &ui.session.model)?;
                    (true, 1)
                }
            }
            _ => {
                // No sessions exist — create a new one
                let session_id = ui.session.id.to_string();
                store.create_session(&session_id, &ui.session.model)?;
                (true, 1)
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
            let error = anyhow::anyhow!("a prompt is already running");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "Image prompt".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("prompt cannot be empty");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_image(&prompt) && !self.ui.prompt_capabilities.image {
            let error = anyhow::anyhow!("active agent does not support image prompts");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_file(&prompt) && !self.ui.prompt_capabilities.embedded_context {
            let error = anyhow::anyhow!("active agent does not support file attachments");
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
                    .unwrap_or_else(|| "ACP subprocess exited unexpectedly".to_string());
                let error = anyhow::anyhow!(reason);
                self.push_system_message(format!("Session disconnected: {error}"));
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
        if self.needs_title && self.ui.session.title == "New Session" {
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
                    let reason = format!("ACP subprocess exited and reconnect failed: {error}");
                    apply_event(
                        &mut self.ui,
                        ClientEvent::Interrupted {
                            reason: reason.clone(),
                        },
                    );
                    self.push_system_message(format!("Session disconnected: {}", reason));
                }
                return;
            }

            let reason =
                last_error.unwrap_or_else(|| "ACP subprocess exited unexpectedly".to_string());
            apply_event(
                &mut self.ui,
                ClientEvent::Interrupted {
                    reason: reason.clone(),
                },
            );
            self.push_system_message(format!("Session disconnected: {}", reason));
            return;
        }

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            self.session.update_session_id(&events);
            for event in events {
                apply_event(&mut self.ui, event.clone());
                self.persist_event(&event);
            }
            return;
        };

        let events = match in_flight.task.collect_ready_events(&mut self.session) {
            Ok(events) => events,
            Err(error) => {
                self.ui.session.status = SessionStatus::Interrupted;
                self.ui.agent_plan.clear();
                self.push_system_message(format!(
                    "Failed to read ACP events from `{}`: {}",
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

        // Preprocess ToolDiff events: capture base text from disk before apply_event
        let mut events = events;
        let mut had_tool_diff = false;
        for event in events.iter_mut() {
            if let ClientEvent::ToolDiff { path, old_text, .. } = event {
                had_tool_diff = true;
                // If old_text is None and we don't already have a base for this path,
                // read the file from disk to capture the pre-modification content
                if old_text.is_none() {
                    let already_tracked =
                        self.ui.session_changes.iter().any(|c| {
                            normalize_tracked_path(&c.path) == normalize_tracked_path(path)
                        });
                    if !already_tracked {
                        // Try to read from disk — if file doesn't exist, it's a new file (leave None)
                        if let Ok(content) = std::fs::read_to_string(path.as_str()) {
                            *old_text = Some(content);
                        }
                    }
                }
            }
        }

        // Process events and track tool lifecycle for file change detection
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, .. } => {
                    self.file_tracker.start_recording(id, Vec::new());
                }
                ClientEvent::ToolCompleted { id, .. }
                | ClientEvent::ToolFailed { id, .. } => {
                    let changes = self.file_tracker.finish_recording(id);
                    self.apply_tracker_changes(id, changes);
                }
                _ => {}
            }
            apply_event(&mut self.ui, event.clone());
            self.persist_event(event);
        }
        self.session.update_session_id(&events);

        // Persist session_changes to SQLite after event processing
        if had_tool_diff {
            self.persist_file_changes();
        }

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        self.detect_file_writes_from_tools();

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
                || tool.summary == "Waiting for activity"
                || tool.summary.starts_with("Awaiting permission")
            {
                tool.summary = "Cancelled".into();
            }
            if tool.kind == "permission" && tool.permission_decision.is_none() {
                tool.permission_decision = Some("cancelled".into());
            }
            tool.logs.push(ToolLogEntry {
                title: "Cancelled".into(),
                body: "Client sent session/cancel for the active turn".into(),
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
            return Err("Session controls can only be changed while the session is idle".into());
        }

        let control = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.id == control_id)
            .cloned()
            .ok_or_else(|| format!("Unknown session control: {control_id}"))?;

        if !control.enabled {
            return Err(format!("Session control is unavailable: {}", control.label));
        }
        if !control.choices.iter().any(|choice| choice.id == value_id) {
            return Err(format!("Unknown value for {}: {value_id}", control.label));
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
        let (messages, tools, timeline) = self.store.load_session(id).map_err(|e| e.to_string())?;
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

        self.needs_title = self.ui.session.title == "New Session";
        self.agent_title_received = false;
        Ok(())
    }

    pub fn session_create(&mut self) -> Result<(), String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        // Re-read current agent selection from persisted settings
        let current_agent_command =
            crate::settings::resolve_agent_command_with_settings(&self.app_paths);
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
        self.ui.session.title = "New Session".to_string();
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
            return Err("Cannot delete the active session".to_string());
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
        self.in_flight_prompt = None;
        self.agent_title_received = false;
        self.skip_replay = has_resume_id;
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

    /// Detect file writes from completed tool calls by examining tool summaries/titles.
    /// Apply verified file changes from the tracker to session state and tool diff previews.
    fn apply_tracker_changes(
        &mut self,
        call_id: &str,
        changes: Vec<crate::file_tracker::VerifiedFileChange>,
    ) {
        use acp_core::diff_to_hunks;

        for change in changes {
            let normalized = normalize_tracked_path(&change.path);
            let is_new = !self.ui.session_changes.iter().any(|c| normalize_tracked_path(&c.path) == normalized);
            let hunks = if change.skipped_diff {
                Vec::new()
            } else {
                diff_to_hunks(change.old_text.as_deref(), &change.new_text)
            };

            if is_new {
                let added = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == DiffLineKind::Added).count();
                let removed = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == DiffLineKind::Removed).count();

                self.ui.session_changes.push(SessionFileChange {
                    path: change.path.clone(),
                    change_type: change.change_type,
                    old_text: change.old_text.clone(),
                    new_text: change.new_text.clone(),
                    added_lines: added,
                    removed_lines: removed,
                    timestamp: {
                        use std::time::SystemTime;
                        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
                        format!("{secs}")
                    },
                });
            }

            // Attach diff preview to the tool that caused the change
            if let Some(tool) = self.ui.tools.iter_mut().find(|t| t.call_id == call_id) {
                let path_buf = PathBuf::from(&change.path);
                if !tool.diff_paths.iter().any(|p| normalize_tracked_path(&p.display().to_string()) == normalized) {
                    tool.diff_paths.push(path_buf.clone());
                }
                if !change.skipped_diff {
                    if let Some(preview) = tool.diff_previews.iter_mut().find(|p| normalize_tracked_path(&p.path.display().to_string()) == normalized) {
                        preview.hunks = hunks;
                    } else {
                        tool.diff_previews.push(ToolDiffPreview { path: path_buf, hunks });
                    }
                }
            }
        }
    }

    /// CodeBuddy agent uses terminal commands (cat > file << 'EOF') to write files,
    /// so we can't rely on ToolDiff events from the ACP protocol. Instead, we check
    /// completed tools for edit-related patterns and read the current file content.
    fn detect_file_writes_from_tools(&mut self) {
        // Normalize path for comparison: forward slashes, lowercase drive letter on Windows
        fn normalize_path(p: &str) -> String {
            normalize_tracked_path(p)
        }

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
                // Check diff_paths first (set by ToolDiff events from ACP WriteTextFileRequest)
                if !tool.diff_paths.is_empty() {
                    let path = tool.diff_paths[0].display().to_string();
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

        // For each detected file write, read current content and upsert into SessionFileChange
        for (call_id, path) in write_paths {
            let display_path = path.clone();
            let file_path = PathBuf::from(&path);
            if let Ok(new_text) = std::fs::read_to_string(&file_path) {
                let normalized = normalize_path(&path);
                // Check if we already have this path (with normalized comparison)
                if let Some(existing) = self
                    .ui
                    .session_changes
                    .iter_mut()
                    .find(|c| normalize_path(&c.path) == normalized)
                {
                    // Update existing entry
                    let hunks = diff_to_hunks(existing.old_text.as_deref(), &new_text);
                    existing.new_text = new_text;
                    existing.added_lines = hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .filter(|l| l.kind == DiffLineKind::Added)
                        .count();
                    existing.removed_lines = hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .filter(|l| l.kind == DiffLineKind::Removed)
                        .count();
                } else {
                    // New entry
                    let hunks = diff_to_hunks(None, &new_text);
                    self.ui.session_changes.push(SessionFileChange {
                        path: display_path.clone(),
                        change_type: FileChangeType::Modified,
                        old_text: None,
                        new_text,
                        added_lines: hunks
                            .iter()
                            .flat_map(|h| &h.lines)
                            .filter(|l| l.kind == DiffLineKind::Added)
                            .count(),
                        removed_lines: hunks
                            .iter()
                            .flat_map(|h| &h.lines)
                            .filter(|l| l.kind == DiffLineKind::Removed)
                            .count(),
                        timestamp: {
                            use std::time::SystemTime;
                            let secs = SystemTime::now()
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            format!("{secs}")
                        },
                    });
                }

                let preview_hunks = self
                    .ui
                    .session_changes
                    .iter()
                    .find(|c| normalize_path(&c.path) == normalized)
                    .map(|change| diff_to_hunks(change.old_text.as_deref(), &change.new_text));
                if let Some(hunks) = preview_hunks
                    && let Some(tool) = self
                        .ui
                        .tools
                        .iter_mut()
                        .find(|tool| tool.call_id == call_id)
                {
                    let path_buf = PathBuf::from(&display_path);
                    if !tool.diff_paths.iter().any(|existing| {
                        normalize_path(&existing.display().to_string())
                            == normalize_path(&display_path)
                    }) {
                        tool.diff_paths.push(path_buf.clone());
                    }
                    if let Some(preview) = tool.diff_previews.iter_mut().find(|preview| {
                        normalize_path(&preview.path.display().to_string())
                            == normalize_path(&display_path)
                    }) {
                        preview.hunks = hunks;
                    } else {
                        tool.diff_previews.push(ToolDiffPreview {
                            path: path_buf,
                            hunks,
                        });
                    }
                }
            }
        }
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
    use super::is_file_write_tool_identity;

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
