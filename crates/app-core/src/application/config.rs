use super::diff_utils::{is_file_write_tool_identity, tool_command_write_hint_paths};
use super::*;

fn permission_selection_outcome_for_display(
    tool: &workspace_model::ToolInvocation,
    decision: &str,
) -> String {
    let option = tool
        .permission_options
        .iter()
        .find(|option| option.id == decision);
    let label = option
        .map(|option| option.label.as_str())
        .unwrap_or(decision);
    if decision.eq_ignore_ascii_case("abort")
        && label.trim().eq_ignore_ascii_case("No, provide feedback")
    {
        "编辑已拒绝".into()
    } else if option
        .map(|option| option.kind.to_ascii_lowercase().contains("allow"))
        .unwrap_or(false)
    {
        "Permission selected: Allow".into()
    } else if option
        .map(|option| option.kind.to_ascii_lowercase().contains("reject"))
        .unwrap_or(false)
    {
        "Permission selected: Reject".into()
    } else {
        format!("Permission selected: {label}")
    }
}

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
        let selected_provider_for_state = selected_provider.clone();

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
            self.authoritative_model_selection =
                Some(ModelSelection::new(value_id, selected_provider_for_state));
            let ui_value_id = provider_qualified_model_value(
                value_id,
                self.current_model_provider_for_persistence().as_deref(),
            );
            self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
                control_id: selected_control_id,
                value_id: ui_value_id,
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
        guidance: Option<String>,
        input_response: Option<workspace_model::PermissionInputResponse>,
    ) -> Result<(), String> {
        self.start_permission_write_baseline_if_allowed(request_id, option_id.as_deref());

        let delivered_to_acp_request = self
            .session
            .resolve_permission(request_id, option_id.clone(), guidance, input_response)
            .map_err(|error| error.to_string())?;

        if !delivered_to_acp_request {
            let decision = codebuddy_interruption_decision(option_id.as_deref());
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

    pub(super) fn auto_resolve_full_access_permission_if_applicable(
        &mut self,
        request_id: &str,
    ) -> bool {
        if !session_mode_is_full_access(self.ui.session.mode.as_deref()) {
            return false;
        }

        let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == request_id) else {
            return false;
        };
        if tool.permission_input.is_some() || !permission_tool_should_start_write_baseline(tool) {
            return false;
        }

        let Some(option_id) = allow_permission_option_id(&tool.permission_options) else {
            return false;
        };

        self.start_permission_write_baseline_if_allowed(request_id, Some(&option_id));
        let delivered = self
            .session
            .resolve_permission(request_id, Some(option_id.clone()), None, None)
            .unwrap_or(false);
        self.mark_tool_permission_selected(request_id, &option_id);
        delivered
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
            let outcome = permission_selection_outcome_for_display(tool, decision);
            tool.summary = outcome.clone();
            tool.status = workspace_model::ToolStatus::Succeeded;
            tool.permission_options.clear();
            tool.permission_input = None;
            tool.permission_decision = Some(outcome);
            self.mark_tool_call_dirty(request_id);
            self.bump_revision();
        }
    }

    pub(super) fn persist_session_model_mode(&self) {
        let _ = self.store.update_session_model_mode_provider(
            &self.ui.session.id.to_string(),
            &self.ui.session.model,
            self.current_model_provider_for_persistence().as_deref(),
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

        let Some(choice) = model_control
            .choices
            .iter()
            .find(|choice| choice_matches_model_selection(choice, &saved_model))
            .cloned()
        else {
            return;
        };

        let control_id = model_control.id.clone();
        let value_id = choice.id.clone();
        let value_label = choice.label.clone();
        let value_provider = saved_model
            .provider
            .clone()
            .or_else(|| provider_from_model_value(&saved_model.value).map(str::to_string))
            .or_else(|| choice_provider(&choice));
        let result = match model_control.source {
            SessionConfigSource::ConfigOption => self.session.set_config_option(
                control_id.clone(),
                value_id.clone(),
                value_provider.clone(),
            ),
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.clone(), value_provider.clone()),
            SessionConfigSource::LegacyMode | SessionConfigSource::LocalMode => self
                .session
                .set_model(value_id.clone(), value_provider.clone()),
        };
        let Ok(events) = result else {
            return;
        };
        self.pending_model_restore = None;
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.authoritative_model_selection = Some(ModelSelection::new(
            value_id.clone(),
            value_provider.clone(),
        ));
        self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id: provider_qualified_model_value(&value_id, value_provider.as_deref()),
            value_label: Some(value_label),
        });
    }

    pub(super) fn current_model_provider_for_persistence(&self) -> Option<String> {
        if let Some(provider) = self
            .authoritative_model_selection
            .as_ref()
            .and_then(|selection| selection.provider.clone())
        {
            return Some(provider);
        }
        if let Some(provider) = self
            .authoritative_model_selection
            .as_ref()
            .and_then(|selection| provider_from_model_value(&selection.value).map(str::to_string))
        {
            return Some(provider);
        }

        let model_control =
            self.ui.session_config.controls.iter().find(|control| {
                control.category == workspace_model::SessionConfigCategory::Model
            })?;

        infer_current_model_provider(model_control)
    }
}

pub(super) fn choice_matches_model_selection(
    choice: &workspace_model::SessionConfigChoice,
    selection: &ModelSelection,
) -> bool {
    let selection_value = model_from_provider_value(&selection.value).unwrap_or(&selection.value);
    let choice_id = model_from_provider_value(&choice.id).unwrap_or(&choice.id);
    let choice_label = model_from_provider_value(&choice.label).unwrap_or(&choice.label);
    if choice.id != selection.value
        && choice.label != selection.value
        && choice.id != selection_value
        && choice.label != selection_value
        && choice_id != selection.value
        && choice_label != selection.value
        && choice_id != selection_value
        && choice_label != selection_value
    {
        return false;
    }

    let Some(provider) = selection
        .provider
        .as_deref()
        .or_else(|| provider_from_model_value(&selection.value))
    else {
        return true;
    };

    choice_provider(choice).is_some_and(|candidate| candidate == provider)
}

pub(super) fn apply_model_selection_to_control(
    control: &mut workspace_model::SessionConfigControl,
    selection: &ModelSelection,
) {
    if let Some(choice) = control
        .choices
        .iter()
        .find(|choice| choice_matches_model_selection(choice, selection))
    {
        let provider = selection
            .provider
            .as_deref()
            .or_else(|| provider_from_model_value(&selection.value))
            .or_else(|| choice.provider.as_deref());
        control.current_value_id = provider_qualified_model_value(&choice.id, provider);
        control.current_value_label = choice.label.clone();
        return;
    }

    let selection_label = model_from_provider_value(&selection.value).unwrap_or(&selection.value);
    control.current_value_id = provider_qualified_model_value(
        selection_label,
        selection
            .provider
            .as_deref()
            .or_else(|| provider_from_model_value(&selection.value)),
    );
    control.current_value_label = selection_label.to_string();
}

pub(super) fn choice_provider(choice: &workspace_model::SessionConfigChoice) -> Option<String> {
    choice
        .provider
        .clone()
        .or_else(|| provider_from_model_value(&choice.id).map(str::to_string))
        .or_else(|| provider_from_model_value(&choice.label).map(str::to_string))
}

pub(super) fn provider_qualified_model_value(value: &str, provider: Option<&str>) -> String {
    if let Some(provider) = provider {
        if provider_from_model_value(value).is_none() {
            return format!("kodex-provider/{provider}/{value}");
        }
    }
    value.to_string()
}

pub(super) fn qualify_current_model_control_provider(
    control: &mut workspace_model::SessionConfigControl,
) {
    if provider_from_model_value(&control.current_value_id).is_some() {
        return;
    }

    let Some(provider) = infer_current_model_provider(control) else {
        return;
    };
    control.current_value_id =
        provider_qualified_model_value(current_model_value(control), Some(&provider));
}

fn infer_current_model_provider(control: &workspace_model::SessionConfigControl) -> Option<String> {
    provider_from_model_value(&control.current_value_id)
        .or_else(|| provider_from_model_value(&control.current_value_label))
        .map(str::to_string)
        .or_else(|| {
            let current = current_model_value(control);
            let mut providers = control
                .choices
                .iter()
                .filter(|choice| choice_matches_model_value(choice, current))
                .filter_map(choice_provider)
                .collect::<Vec<_>>();
            providers.sort();
            providers.dedup();
            if providers.len() == 1 {
                return providers.pop();
            }

            inferred_provider_for_model_name(current)
                .filter(|provider| providers.iter().any(|candidate| candidate == provider))
                .map(str::to_string)
        })
}

fn choice_matches_model_value(choice: &workspace_model::SessionConfigChoice, model: &str) -> bool {
    let choice_id = model_from_provider_value(&choice.id).unwrap_or(&choice.id);
    let choice_label = model_from_provider_value(&choice.label).unwrap_or(&choice.label);
    choice.id == model || choice.label == model || choice_id == model || choice_label == model
}

fn current_model_value(control: &workspace_model::SessionConfigControl) -> &str {
    model_from_provider_value(&control.current_value_id)
        .or_else(|| model_from_provider_value(&control.current_value_label))
        .unwrap_or_else(|| {
            if control.current_value_label.trim().is_empty() {
                control.current_value_id.as_str()
            } else {
                control.current_value_label.as_str()
            }
        })
}

fn inferred_provider_for_model_name(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.starts_with("qwen/")
        || normalized.starts_with("minimaxai/")
        || normalized.starts_with("moonshotai/")
        || normalized.starts_with("zai-org/")
        || normalized.starts_with("stepfun/")
        || normalized.starts_with("google/")
    {
        Some("commandcode")
    } else if normalized.contains("deepseek") {
        Some("deepseek")
    } else if normalized.contains("kimi") {
        Some("kimi_code")
    } else if normalized.contains("mimo") || normalized.contains("xiaomi") {
        Some("xiaomi_mimo")
    } else {
        None
    }
}

pub(super) fn provider_from_model_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("kodex-provider/") {
        return rest.split_once('/').map(|(provider, _)| provider);
    }
    if let Some(rest) = trimmed.strip_prefix("kodex-provider:") {
        return rest.split_once(':').map(|(provider, _)| provider);
    }
    None
}

fn model_from_provider_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("kodex-provider/") {
        return rest.split_once('/').map(|(_, model)| model);
    }
    if let Some(rest) = trimmed.strip_prefix("kodex-provider:") {
        return rest.split_once(':').map(|(_, model)| model);
    }
    None
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

fn allow_permission_option_id(options: &[workspace_model::PermissionOption]) -> Option<String> {
    options
        .iter()
        .find(|option| {
            let kind = option.kind.to_ascii_lowercase();
            let label = option.label.to_ascii_lowercase();
            let id = option.id.to_ascii_lowercase();
            kind.contains("allow") || label.contains("allow") || id.contains("allow")
        })
        .map(|option| option.id.clone())
}

fn codebuddy_interruption_decision(option_id: Option<&str>) -> String {
    let Some(option_id) = option_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return "deny".into();
    };
    let normalized = option_id
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "allowalways" | "alwaysallow" | "allowall" => "allowAll".into(),
        "allowonce" | "allow" => "allow".into(),
        "rejectonce" | "rejectalways" | "reject" | "deny" | "cancel" | "cancelled" | "canceled" => {
            "deny".into()
        }
        _ => option_id.to_string(),
    }
}

fn session_mode_is_full_access(mode: Option<&str>) -> bool {
    let Some(mode) = mode else {
        return false;
    };
    matches!(
        mode.trim().to_ascii_lowercase().as_str(),
        "full-access"
            | "fullaccess"
            | "full_access"
            | "danger-full-access"
            | "bypasspermissions"
            | "bypass"
            | "完全访问"
    )
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

#[cfg(test)]
mod tests {
    use super::codebuddy_interruption_decision;

    #[test]
    fn codebuddy_interruption_decision_normalizes_acp_option_ids() {
        assert_eq!(
            codebuddy_interruption_decision(Some("allow_always")),
            "allowAll"
        );
        assert_eq!(codebuddy_interruption_decision(Some("allow")), "allow");
        assert_eq!(codebuddy_interruption_decision(Some("reject")), "deny");
        assert_eq!(codebuddy_interruption_decision(None), "deny");
    }
}
