use super::*;

impl Application {
    pub(super) fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        false
    }

    pub fn has_running_codex_acp_session(&self) -> bool {
        self.is_codex_acp_session() && self.session.is_alive()
    }

    pub(super) fn is_codex_acp_session(&self) -> bool {
        self.ui
            .session
            .agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or_else(|| {
                self.agent_command
                    .to_ascii_lowercase()
                    .contains("codex-acp")
            })
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        self.store.list_sessions().map_err(|e| e.to_string())
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(id)?;

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
        let stored_agent_cli = self.store.get_session_agent_cli(id).unwrap_or(None);
        let session_agent_command = stored_agent_cli
            .as_deref()
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
            })
            .unwrap_or_else(|| self.agent_command.clone());
        crate::settings::ensure_agent_ready_for_command(&session_agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;

        // Empty local sessions may have a transient ACP id from session/new, but no durable
        // agent-side resource yet. Resume only after a prompt/tool has created persisted activity.
        let resume_acp_id = self.resume_acp_session_id_for_stored_session(id);

        // Start a new ACP session handle
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: session_agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(
                &session_agent_command,
                &self.app_paths,
            ),
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
        self.agent_command = session_agent_command;
        self.ui.session.agent_cli = stored_agent_cli.or_else(|| {
            Some(crate::settings::agent_label_for_command(
                &self.agent_command,
            ))
        });
        self.persist_current_codex_provider_if_needed();
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages = messages;
        self.ui.tools = tools;
        self.ui.timeline = timeline;
        if let Some(agent_label) = self.ui.session.agent_cli.clone() {
            update_initial_agent_notice(&mut self.ui, &agent_label);
        }
        self.ui.session.status = SessionStatus::Idle;
        self.session = session;
        self.in_flight_prompt = None;
        // Historical diffs are loaded through scoped change-set APIs. The old
        // arrays remain runtime staging buffers for compatibility, but they are
        // not restored as primary session/review/timeline state on switch.
        self.ui.session_changes.clear();
        self.ui.review_changes.clear();
        self.ui.turn_changes.clear();
        self.review_changes_started = false;
        self.current_turn_user_message_id = None;

        // Compute seq counter from loaded data
        self.seq_counter = self.store.next_seq(id).unwrap_or(1);

        // Load session title
        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            self.ui.session.title = s.title.clone();
        }

        self.needs_title = is_placeholder_session_title(&self.ui.session.title);
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    pub fn session_create(&mut self, agent: Option<AgentCliId>) -> Result<(), String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let current_agent_command = match agent {
            Some(agent) => crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        };
        crate::settings::ensure_agent_ready_for_command(&current_agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;
        self.agent_command = current_agent_command;

        // Start a new ACP session handle (no resume for new session)
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
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
        self.ui.review_changes.clear();
        self.ui.turn_changes.clear();
        self.session = session;
        self.in_flight_prompt = None;
        self.review_changes_started = false;
        self.current_turn_user_message_id = None;
        self.seq_counter = 1;
        self.needs_title = true;
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.pending_model_restore = None;
        self.persist_session_model_mode();
        let _ = self.store.update_session_agent_cli(
            &self.ui.session.id.to_string(),
            self.ui.session.agent_cli.as_deref().unwrap_or("CodeBuddy"),
        );
        self.persist_current_codex_provider_if_needed();

        self.bump_revision();
        Ok(())
    }

    pub fn session_delete(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            let replacement_id = self
                .store
                .list_sessions()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|session| session.id != id)
                .map(|session| session.id);

            if let Some(replacement_id) = replacement_id {
                self.session_switch(&replacement_id)?;
            } else {
                self.session_create(None)?;
            }
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(&self.ui.session.id.to_string())?;

        // Try to resume the current ACP session if we have its ID
        let session_id = self.ui.session.id.to_string();
        let has_activity = self
            .store
            .session_has_activity(&session_id)
            .unwrap_or(false);
        let resume_id = if has_activity && !self.session.id.is_empty() {
            Some(self.session.id.clone())
        } else {
            self.resume_acp_session_id_for_stored_session(&session_id)
        };

        let resume_id_for_handle = resume_id.clone();
        let has_resume_id = resume_id_for_handle.is_some();
        crate::settings::ensure_agent_ready_for_command(&self.agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
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
        self.current_turn_user_message_id = None;
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.skip_replay = has_resume_id;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    pub(super) fn resume_acp_session_id_for_stored_session(&self, id: &str) -> Option<String> {
        if self.store.session_has_activity(id).unwrap_or(false) {
            self.store.get_acp_session_id(id).unwrap_or(None)
        } else {
            let _ = self.store.clear_acp_session_id(id);
            None
        }
    }
}
