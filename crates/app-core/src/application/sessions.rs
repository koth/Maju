use super::*;

impl Application {
    pub(super) fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        false
    }

    pub fn has_running_codex_acp_session(&self) -> bool {
        let visible_running = self.is_codex_acp_session() && self.session.is_alive();
        let background_running = self.runtime_registry.entries.values().any(|runtime| {
            runtime
                .ui
                .session
                .agent_cli
                .as_deref()
                .map(is_codex_agent_label)
                .unwrap_or_else(|| {
                    let command = runtime.agent_command.to_ascii_lowercase();
                    command.contains("codex-acp") || command.contains("kodex-acp")
                })
                && runtime.session.is_alive()
        });
        visible_running || background_running
    }

    pub(super) fn is_codex_acp_session(&self) -> bool {
        self.ui
            .session
            .agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or_else(|| {
                let command = self.agent_command.to_ascii_lowercase();
                command.contains("codex-acp") || command.contains("kodex-acp")
            })
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        let mut sessions = self.store.list_sessions().map_err(|e| e.to_string())?;
        self.runtime_registry
            .annotate_sessions(&mut sessions, &self.ui.session.id.to_string());
        Ok(sessions)
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            self.runtime_registry.clear_attention(id);
            return Ok(());
        }

        self.ensure_codex_provider_matches_for_resume(id)?;
        let target_runtime = if let Some(runtime) = self.runtime_registry.remove(id) {
            runtime
        } else {
            self.runtime_for_stored_session(id)?
        };

        let background_runtime = self.install_runtime_as_visible(target_runtime);
        self.runtime_registry.insert(background_runtime);
        self.ui.session.status = if self.in_flight_prompt.is_some() {
            SessionStatus::Streaming
        } else {
            self.ui.session.status.clone()
        };
        self.poll_current_runtime_progress();
        self.bump_revision();
        Ok(())
    }

    pub fn session_create(&mut self, agent: Option<AgentCliId>) -> Result<(), String> {
        let runtime = self.runtime_for_new_session(agent)?;
        let background_runtime = self.install_runtime_as_visible(runtime);
        self.runtime_registry.insert(background_runtime);
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

        if let Some(mut runtime) = self.runtime_registry.remove_all_state(id) {
            runtime.session.shutdown();
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(&self.ui.session.id.to_string())?;

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
            remote_ssh: self.remote_ssh_session_config(),
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
        self.authoritative_model_selection = None;
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

    fn runtime_for_stored_session(&mut self, id: &str) -> Result<SessionRuntime, String> {
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
        let current_agent_label = self.ui.session.agent_cli.as_deref();
        let session_agent_command = if stored_agent_cli.as_deref() == current_agent_label {
            self.agent_command.clone()
        } else {
            stored_agent_cli
                .as_deref()
                .and_then(|label| {
                    crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
                })
                .unwrap_or_else(|| self.agent_command.clone())
        };
        crate::settings::ensure_agent_ready_for_command(&session_agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;

        let resume_acp_id = self.resume_acp_session_id_for_stored_session(id);
        let has_resume_id = resume_acp_id.is_some();
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
            remote_ssh: self.remote_ssh_session_config(),
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));

        let mut ui = self.ui.clone();
        ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        ui.session.model = model;
        ui.session.mode = mode;
        ui.session.agent_cli = stored_agent_cli.or_else(|| {
            Some(crate::settings::agent_label_for_command(
                &session_agent_command,
            ))
        });
        ui.session_config = Default::default();
        ui.prompt_capabilities = Default::default();
        ui.available_commands.clear();
        ui.agent_plan.clear();
        ui.messages = messages;
        ui.tools = tools;
        ui.timeline = timeline;
        ui.session.status = SessionStatus::Idle;
        ui.session_changes.clear();
        ui.review_changes.clear();
        ui.turn_changes.clear();

        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            ui.session.title = s.title.clone();
        }
        if let Some(agent_label) = ui.session.agent_cli.clone() {
            update_initial_agent_notice(&mut ui, &agent_label);
            if is_codex_agent_label(&agent_label) {
                let provider = crate::settings::codex_current_provider(&self.app_paths);
                let _ = self.store.update_session_codex_provider(id, &provider);
            }
        }
        let _ =
            self.store
                .update_session_model_mode(id, &ui.session.model, ui.session.mode.as_deref());

        let seq_counter = self.store.next_seq(id).unwrap_or(1);
        let needs_title = is_placeholder_session_title(&ui.session.title);
        Ok(SessionRuntime {
            local_session_id: ui.session.id,
            ui,
            session,
            agent_command: session_agent_command,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay: has_resume_id,
            pending_model_restore: Some(
                self.store
                    .get_session_model_mode(id)
                    .ok()
                    .flatten()
                    .map(|(model, _)| model)
                    .unwrap_or_else(|| AGENT_DEFAULT_MODEL_LABEL.to_string()),
            ),
            authoritative_model_selection: None,
            file_tracker: FileChangeTracker::new(&self.ui.workspace.root),
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            pending_tool_diff_previews: Vec::new(),
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            last_viewed: self.runtime_now(),
            idle_since: None,
            runtime_status: SessionRuntimeStatus::Active,
            attention_state: SessionAttentionState::None,
        })
    }

    fn runtime_for_new_session(
        &mut self,
        agent: Option<AgentCliId>,
    ) -> Result<SessionRuntime, String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let agent_command = match agent {
            Some(agent) => crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        };
        crate::settings::ensure_agent_ready_for_command(&agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;

        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&agent_command, &self.app_paths),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
            remote_ssh: self.remote_ssh_session_config(),
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode("Build");

        let agent_cli_label = crate::settings::agent_label_for_command(&agent_command);
        let mut ui = self.ui.clone();
        ui.session.id = new_id;
        ui.session.title = "新会话".to_string();
        ui.session.model = initial_model;
        ui.session.mode = Some("Build".into());
        ui.session.agent_cli = Some(agent_cli_label.clone());
        ui.session_config = Default::default();
        ui.prompt_capabilities = Default::default();
        ui.session.status = SessionStatus::Idle;
        ui.available_commands.clear();
        ui.agent_plan.clear();
        ui.messages.clear();
        ui.tools.clear();
        ui.timeline.clear();
        ui.session_changes.clear();
        ui.review_changes.clear();
        ui.turn_changes.clear();

        let _ = self.store.update_session_model_mode(
            &new_id.to_string(),
            &ui.session.model,
            ui.session.mode.as_deref(),
        );
        let _ = self
            .store
            .update_session_agent_cli(&new_id.to_string(), &agent_cli_label);
        if is_codex_agent_label(&agent_cli_label) {
            let provider = crate::settings::codex_current_provider(&self.app_paths);
            let _ = self
                .store
                .update_session_codex_provider(&new_id.to_string(), &provider);
        }

        Ok(SessionRuntime {
            local_session_id: new_id,
            ui,
            session,
            agent_command,
            in_flight_prompt: None,
            seq_counter: 1,
            needs_title: true,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay: false,
            pending_model_restore: None,
            authoritative_model_selection: None,
            file_tracker: FileChangeTracker::new(&self.ui.workspace.root),
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            pending_tool_diff_previews: Vec::new(),
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            last_viewed: self.runtime_now(),
            idle_since: None,
            runtime_status: SessionRuntimeStatus::Active,
            attention_state: SessionAttentionState::None,
        })
    }
}
