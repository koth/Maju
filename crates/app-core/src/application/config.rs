use super::diff_utils::{is_file_write_tool_identity, tool_command_write_hint_paths};
use super::*;

impl Application {
    pub(super) fn persist_current_codex_provider_if_needed(&self) {
        if !self.is_codex_acp_session() {
            return;
        }
        let provider = crate::settings::codex_current_provider(&self.app_paths);
        let _ = self
            .store
            .update_session_codex_provider(&self.ui.session.id.to_string(), &provider);
    }

    pub(super) fn ensure_codex_provider_matches_for_resume(
        &self,
        session_id: &str,
    ) -> Result<(), String> {
        let agent_cli = self.store.get_session_agent_cli(session_id).unwrap_or(None);
        if !agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or(false)
        {
            return Ok(());
        }

        let Some(stored_provider) = self
            .store
            .get_session_codex_provider(session_id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let current_provider = crate::settings::codex_current_provider(&self.app_paths);
        if stored_provider == current_provider {
            return Ok(());
        }

        Err(format!(
            "配置不一致，请新开会话，或者去切换配置。当前配置：{}，会话配置：{}",
            display_codex_provider(&current_provider),
            display_codex_provider(&stored_provider)
        ))
    }

    pub fn set_session_config_control(
        &mut self,
        control_id: &str,
        value_id: &str,
        provider: Option<&str>,
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
        let selected_choice = control
            .choices
            .iter()
            .find(|choice| {
                choice.id == value_id
                    && provider.map_or(true, |provider| {
                        choice.provider.as_deref() == Some(provider)
                    })
            })
            .cloned()
            .or_else(|| {
                provider.is_none().then(|| {
                    control
                        .choices
                        .iter()
                        .find(|choice| choice.id == value_id)
                        .cloned()
                })?
            });
        let Some(selected_choice) = selected_choice else {
            return Err(format!("{} 的值未知：{value_id}", control.label));
        };

        let is_model_control = control.category == workspace_model::SessionConfigCategory::Model;
        let selected_control_id = control.id.clone();
        let selected_label = Some(selected_choice.label.clone());
        let selected_provider = provider
            .map(str::to_string)
            .or_else(|| selected_choice.provider.clone());

        let events = match control.source.clone() {
            SessionConfigSource::ConfigOption => self
                .session
                .set_config_option(control.id.clone(), value_id.to_string(), selected_provider)
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LegacyMode => self
                .session
                .set_mode(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.to_string(), selected_provider)
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LocalMode => {
                self.session
                    .set_permission_mode(value_id)
                    .map_err(|error| error.to_string())?;
                vec![ClientEvent::SessionConfigValueChanged {
                    control_id: control.id.clone(),
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
            self.apply_event_with_dirty_tracking(&event);
        }
        if is_model_control {
            self.pending_model_restore = None;
            self.authoritative_model_selection = Some(value_id.to_string());
            self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
                control_id: selected_control_id,
                value_id: value_id.to_string(),
                value_label: selected_label,
            });
        }
        self.persist_session_model_mode();
        self.bump_revision();

        Ok(self.ui.session_config.clone())
    }

    pub fn resolve_tool_permission(
        &mut self,
        request_id: &str,
        option_id: Option<String>,
    ) -> Result<(), String> {
        self.start_permission_write_baseline_if_allowed(request_id, option_id.as_deref());

        let delivered_to_acp_request = self
            .session
            .resolve_permission(request_id, option_id.clone())
            .map_err(|error| error.to_string())?;

        if !delivered_to_acp_request {
            let decision = option_id.unwrap_or_else(|| "deny".into());
            self.session
                .resolve_codebuddy_interruption(request_id, &decision)
                .map_err(|error| error.to_string())?;
            self.mark_tool_permission_selected(request_id, &decision);
        } else {
            let decision = option_id.as_deref().unwrap_or("cancelled");
            self.mark_tool_permission_selected(request_id, decision);
        }

        Ok(())
    }

    pub(super) fn start_permission_write_baseline_if_allowed(
        &mut self,
        request_id: &str,
        option_id: Option<&str>,
    ) -> bool {
        let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == request_id) else {
            return false;
        };
        if !permission_selection_is_allow(&tool.permission_options, option_id) {
            return false;
        }

        let mut paths = tool
            .raw_input
            .as_deref()
            .map(permission_details_write_paths)
            .unwrap_or_default();
        paths.retain(|path| permission_path_is_trackable(path, &self.ui.workspace.root));
        if paths.is_empty() {
            return false;
        }
        if !permission_tool_should_start_write_baseline(tool) {
            return false;
        }

        self.file_tracker.start_recording(request_id, paths);
        true
    }

    pub(super) fn mark_tool_permission_selected(&mut self, request_id: &str, decision: &str) {
        if let Some(tool) = self
            .ui
            .tools
            .iter_mut()
            .find(|tool| tool.call_id == request_id)
        {
            let outcome = format!("Permission selected: {decision}");
            tool.summary = outcome.clone();
            tool.status = workspace_model::ToolStatus::Succeeded;
            tool.permission_options.clear();
            tool.permission_decision = Some(outcome);
            self.mark_tool_call_dirty(request_id);
            self.bump_revision();
        }
    }

    pub(super) fn persist_session_model_mode(&self) {
        let _ = self.store.update_session_model_mode(
            &self.ui.session.id.to_string(),
            &self.ui.session.model,
            self.ui.session.mode.as_deref(),
        );
    }

    pub(super) fn restore_pending_model_selection(&mut self) {
        let Some(saved_model) = self.pending_model_restore.clone() else {
            return;
        };
        let Some(model_control) = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == workspace_model::SessionConfigCategory::Model)
            .cloned()
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

        let control_id = model_control.id.clone();
        let value_id = choice.id.clone();
        let value_label = choice.label.clone();
        let result = match model_control.source {
            SessionConfigSource::ConfigOption => self.session.set_config_option(
                control_id.clone(),
                value_id.clone(),
                choice.provider.clone(),
            ),
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.clone(), choice.provider.clone()),
            SessionConfigSource::LegacyMode | SessionConfigSource::LocalMode => self
                .session
                .set_model(value_id.clone(), choice.provider.clone()),
        };
        let Ok(events) = result else {
            return;
        };
        self.pending_model_restore = None;
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.authoritative_model_selection = Some(value_id.clone());
        self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id,
            value_label: Some(value_label),
        });
    }
}

fn permission_selection_is_allow(
    options: &[workspace_model::PermissionOption],
    option_id: Option<&str>,
) -> bool {
    let Some(option_id) = option_id else {
        return false;
    };
    options
        .iter()
        .find(|option| option.id == option_id)
        .is_some_and(|option| {
            let kind = option.kind.to_ascii_lowercase();
            let label = option.label.to_ascii_lowercase();
            let id = option.id.to_ascii_lowercase();
            kind.contains("allow") || label.contains("allow") || id.contains("allow")
        })
}

fn permission_tool_should_start_write_baseline(tool: &workspace_model::ToolInvocation) -> bool {
    if is_file_write_tool_identity(&tool.kind, &tool.name) {
        return true;
    }

    if permission_tool_is_shell_command(&tool.kind) || permission_tool_is_shell_command(&tool.name)
    {
        return true;
    }

    !tool_command_write_hint_paths(tool.raw_input.as_deref()).is_empty()
}

fn permission_tool_is_shell_command(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "bash" | "shell")
}

fn permission_path_is_trackable(path: &str, workspace_root: &std::path::Path) -> bool {
    let normalized = normalize_path_for_storage(path, workspace_root)
        .trim_start_matches("./")
        .to_string();
    if normalized.is_empty() || normalized.split('/').any(|part| part == "..") {
        return false;
    }

    workspace_root.join(normalized).starts_with(workspace_root)
}

fn permission_details_write_paths(details: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_paths_section = false;

    for line in details.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(path) = trimmed.strip_prefix("Path:") {
            let path = path.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
            in_paths_section = false;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Paths:") {
            in_paths_section = true;
            continue;
        }
        if trimmed.ends_with(':') {
            in_paths_section = false;
            continue;
        }
        if in_paths_section && let Some(path) = trimmed.strip_prefix("- ") {
            let path = path.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }

    paths.sort();
    paths.dedup();
    paths
}
