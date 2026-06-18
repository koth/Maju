use super::*;

struct PreparedSessionRuntime {
    workspace_root: String,
    agent_env: Vec<(String, String)>,
    acp_port: u16,
    remote_ssh: Option<RemoteSshSessionConfig>,
}

fn remote_machine_profile_from_workspace(
    remote: &RemoteLinuxWorkspace,
) -> workspace_model::RemoteMachineProfile {
    workspace_model::RemoteMachineProfile {
        id: remote.profile_id.unwrap_or_else(uuid::Uuid::new_v4),
        display_name: remote.display_name(),
        ssh_target: remote.ssh_target.clone(),
        ssh_port: remote.ssh_port,
        created_at_ms: 0,
        updated_at_ms: 0,
        last_validation: None,
    }
}

fn session_status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Streaming => "Streaming",
        SessionStatus::WaitingForTool => "WaitingForTool",
        SessionStatus::Interrupted => "Interrupted",
    }
}

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

    #[cfg(test)]
    pub(super) fn agent_command_for_new_session(&self, agent: Option<AgentCliId>) -> String {
        match agent {
            Some(agent) if self.remote_agent_selection_matches_current(agent) => {
                self.agent_command.clone()
            }
            Some(agent) => self
                .command_for_agent_in_current_workspace(agent)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        }
    }

    fn prepare_agent_command_for_new_session(
        &self,
        agent: Option<AgentCliId>,
    ) -> Result<String, String> {
        match agent {
            Some(agent) if self.remote_agent_selection_matches_current(agent) => {
                Ok(self.agent_command.clone())
            }
            Some(agent) if self.is_remote_workspace() => self.bootstrap_remote_agent_command(agent),
            Some(agent) => Ok(self
                .command_for_agent_in_current_workspace(agent)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                })),
            None => Ok(self.agent_command.clone()),
        }
    }

    fn prepare_agent_command_for_stored_label(
        &self,
        label: &str,
    ) -> Result<Option<String>, String> {
        if !self.is_remote_workspace() {
            return Ok(self.command_for_agent_label_in_current_workspace(label));
        }
        let Some(agent) = crate::settings::agent_id_for_label(label) else {
            return Ok(None);
        };
        if self.remote_agent_selection_matches_current(agent) {
            return Ok(Some(self.agent_command.clone()));
        }
        self.bootstrap_remote_agent_command(agent).map(Some)
    }

    fn bootstrap_remote_agent_command(&self, agent: AgentCliId) -> Result<String, String> {
        let remote = self.current_remote_workspace().ok_or_else(|| {
            "Remote workspace is missing metadata; reopen the remote directory first".to_string()
        })?;
        let profile = remote
            .profile_id
            .and_then(|profile_id| {
                crate::remote_profiles::get_remote_machine_profile(&self.app_paths, profile_id).ok()
            })
            .unwrap_or_else(|| remote_machine_profile_from_workspace(&remote));
        let ssh_password = remote.ssh_password.as_deref().or_else(|| {
            self.remote_ssh
                .as_ref()
                .and_then(|ssh| ssh.ssh_password.as_deref())
        });

        crate::remote_bootstrap::bootstrap_remote_agent(
            crate::remote_bootstrap::RemoteAgentBootstrapRequest {
                request_id: uuid::Uuid::new_v4(),
                profile: &profile,
                remote_path: &remote.remote_path,
                ssh_password,
                agent_cli: agent,
            },
            &crate::remote_ssh::SystemRemoteSshCommandRunner,
            |_| {},
        )
        .map(|bootstrap| bootstrap.agent_command)
        .map_err(|e| e.to_string())
    }

    fn current_remote_workspace(&self) -> Option<RemoteLinuxWorkspace> {
        match &self.ui.workspace.location {
            workspace_model::WorkspaceLocation::RemoteLinux(remote) => Some(remote.clone()),
            workspace_model::WorkspaceLocation::Local => None,
        }
    }

    fn remote_agent_selection_matches_current(&self, agent: AgentCliId) -> bool {
        if !self.is_remote_workspace() {
            return false;
        }
        if matches!(
            &self.ui.workspace.location,
            workspace_model::WorkspaceLocation::RemoteLinux(remote)
                if remote.agent_cli == Some(agent)
        ) {
            return true;
        }
        crate::settings::agent_label_for_id(agent)
            .is_some_and(|label| self.ui.session.agent_cli.as_deref() == Some(label))
    }

    pub(super) fn command_for_agent_in_current_workspace(
        &self,
        agent: AgentCliId,
    ) -> Option<String> {
        if self.is_remote_workspace() {
            crate::settings::remote_linux_command_for_agent(agent)
        } else {
            crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
        }
    }

    pub(super) fn command_for_agent_label_in_current_workspace(
        &self,
        label: &str,
    ) -> Option<String> {
        if self.is_remote_workspace() {
            crate::settings::remote_linux_command_for_agent_label(label)
        } else {
            crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
        }
    }

    fn prepare_session_runtime(
        &self,
        agent_command: &str,
    ) -> Result<PreparedSessionRuntime, String> {
        if self.is_remote_workspace() {
            return self.prepare_remote_session_runtime(agent_command);
        }

        crate::settings::ensure_agent_ready_for_command(agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;
        Ok(PreparedSessionRuntime {
            workspace_root: self.session_config_workspace_root(None),
            agent_env: crate::settings::agent_env_for_command(agent_command, &self.app_paths),
            acp_port: self.acp_port,
            remote_ssh: None,
        })
    }

    fn prepare_remote_session_runtime(
        &self,
        agent_command: &str,
    ) -> Result<PreparedSessionRuntime, String> {
        let mut remote_ssh = self.remote_ssh.clone().ok_or_else(|| {
            "Remote workspace is not connected; reopen the remote directory first".to_string()
        })?;
        let local_port =
            super::bootstrap::find_available_loopback_port().map_err(|e| e.to_string())?;
        let mut agent_ports = std::collections::BTreeSet::from([local_port]);
        let port_map = super::bootstrap::remote_proxy_port_map(&remote_ssh, &mut agent_ports)
            .map_err(|e| e.to_string())?;
        let remote_port = port_map.get(&local_port).copied().unwrap_or(local_port);
        remote_ssh.local_port = local_port;
        remote_ssh.remote_port = remote_port;
        remote_ssh.reverse_forwards.clear();
        let workspace_root = self.session_config_workspace_root(Some(&remote_ssh));

        let remote_runtime = super::bootstrap::prepare_remote_agent_runtime(
            agent_command,
            &self.app_paths,
            &remote_ssh,
        )
        .map_err(|e| e.to_string())?;
        remote_ssh.reverse_forwards = remote_runtime.reverse_forwards;

        Ok(PreparedSessionRuntime {
            workspace_root,
            agent_env: remote_runtime.agent_env,
            acp_port: local_port,
            remote_ssh: Some(remote_ssh),
        })
    }

    pub(super) fn session_config_workspace_root(
        &self,
        remote_ssh: Option<&RemoteSshSessionConfig>,
    ) -> String {
        remote_ssh
            .map(|config| config.remote_workspace_root.clone())
            .unwrap_or_else(|| self.ui.workspace.root.display().to_string())
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        let mut sessions = self.store.list_sessions().map_err(|e| e.to_string())?;
        self.runtime_registry
            .annotate_sessions(&mut sessions, &self.ui.session.id.to_string());
        self.annotate_visible_session_summary(&mut sessions);
        Ok(sessions)
    }

    pub fn session_list_after_poll(&mut self) -> Result<Vec<SessionListItem>, String> {
        self.poll_prompt_progress();
        self.session_list()
    }

    pub fn session_list_for_visibility(
        &self,
        workspace_visible: bool,
    ) -> Result<Vec<SessionListItem>, String> {
        let mut sessions = self.session_list()?;
        if !workspace_visible {
            self.annotate_visible_session_as_background(&mut sessions);
        }
        Ok(sessions)
    }

    pub fn session_list_after_poll_for_visibility(
        &mut self,
        workspace_visible: bool,
    ) -> Result<Vec<SessionListItem>, String> {
        self.poll_prompt_progress();
        self.session_list_for_visibility(workspace_visible)
    }

    fn annotate_visible_session_summary(&self, sessions: &mut [SessionListItem]) {
        let visible_session_id = self.ui.session.id.to_string();
        let Some(item) = sessions
            .iter_mut()
            .find(|session| session.id == visible_session_id)
        else {
            return;
        };

        item.title = self.ui.session.title.clone();
        item.status = session_status_label(&self.ui.session.status).to_string();
        if self.ui.session.agent_cli.is_some() {
            item.agent_cli = self.ui.session.agent_cli.clone();
        }
    }

    fn annotate_visible_session_as_background(&self, sessions: &mut [SessionListItem]) {
        let visible_session_id = self.ui.session.id.to_string();
        let Some(item) = sessions
            .iter_mut()
            .find(|session| session.id == visible_session_id)
        else {
            return;
        };

        if self.runtime_needs_attention() {
            item.attention_state = SessionAttentionState::NeedsAttention;
        }

        item.runtime_status = if self.in_flight_prompt.is_some() {
            SessionRuntimeStatus::BackgroundRunning
        } else {
            SessionRuntimeStatus::BackgroundIdle
        };
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

    pub fn session_archive(&mut self, id: &str) -> Result<(), String> {
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
        self.store.archive_session(id).map_err(|e| e.to_string())
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
        let agent_command = self.agent_command.clone();
        let prepared_runtime = self.prepare_session_runtime(&agent_command)?;
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: resume_id,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
        })
        .map_err(|e| e.to_string())?;
        if let Some(acp_id) = resume_id_for_handle {
            session.id = acp_id;
        }

        self.session = session;
        self.agent_command = agent_command;
        self.acp_port = prepared_runtime.acp_port;
        self.remote_ssh = prepared_runtime.remote_ssh;
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
        self.pending_model_restore = Some(ModelSelection::new(
            self.ui.session.model.clone(),
            self.current_model_provider_for_persistence(),
        ));
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

        let (model, model_provider, mode) = self
            .store
            .get_session_model_provider_mode(id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| {
                (
                    self.ui.session.model.clone(),
                    self.current_model_provider_for_persistence(),
                    self.ui.session.mode.clone(),
                )
            });
        let mode = mode.or_else(|| Some("Build".into()));
        let stored_agent_cli = self.store.get_session_agent_cli(id).unwrap_or(None);
        let current_agent_label = self.ui.session.agent_cli.as_deref();
        let session_agent_command = if stored_agent_cli.as_deref() == current_agent_label {
            self.agent_command.clone()
        } else if let Some(label) = stored_agent_cli.as_deref() {
            self.prepare_agent_command_for_stored_label(label)?
                .unwrap_or_else(|| self.agent_command.clone())
        } else {
            self.agent_command.clone()
        };
        let prepared_runtime = self.prepare_session_runtime(&session_agent_command)?;

        let resume_acp_id = self.resume_acp_session_id_for_stored_session(id);
        let has_resume_id = resume_acp_id.is_some();
        let session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: session_agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: resume_acp_id,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));

        let mut ui = self.ui.clone();
        let pending_model_restore =
            Some(ModelSelection::new(model.clone(), model_provider.clone()));
        ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        ui.session.model = model;
        ui.session.mode = mode;
        ui.session.agent_cli = Some(active_agent_label_for_command(
            &session_agent_command,
            stored_agent_cli,
        ));
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
        let _ = self.store.update_session_model_mode_provider(
            id,
            &ui.session.model,
            model_provider.as_deref(),
            ui.session.mode.as_deref(),
        );

        let seq_counter = self.store.next_seq(id).unwrap_or(1);
        let needs_title = is_placeholder_session_title(&ui.session.title);
        Ok(SessionRuntime {
            local_session_id: ui.session.id,
            ui,
            session,
            agent_command: session_agent_command,
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay: has_resume_id,
            pending_model_restore,
            authoritative_model_selection: None,
            file_tracker: FileChangeTracker::new(&self.ui.workspace.root),
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
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

        let agent_command = self.prepare_agent_command_for_new_session(agent)?;
        let prepared_runtime = self.prepare_session_runtime(&agent_command)?;

        let session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
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
        if let Some(agent) = agent {
            if let workspace_model::WorkspaceLocation::RemoteLinux(remote) =
                &mut ui.workspace.location
            {
                remote.agent_cli = Some(agent);
                remote.agent_command = Some(agent_command.clone());
            }
        }
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
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh,
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
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            last_viewed: self.runtime_now(),
            idle_since: None,
            runtime_status: SessionRuntimeStatus::Active,
            attention_state: SessionAttentionState::None,
        })
    }
}
