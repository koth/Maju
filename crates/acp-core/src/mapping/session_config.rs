use super::*;

pub(super) fn emit_plan_update(tx: &mpsc::Sender<ClientEvent>, plan: Plan) -> anyhow::Result<()> {
    let entries = plan
        .entries
        .into_iter()
        .map(|entry| AgentPlanEntry {
            id: None,
            content: entry.content,
            priority: normalize_plan_priority(entry.priority),
            status: normalize_plan_status(entry.status),
        })
        .collect();

    tx.send(ClientEvent::PlanUpdated { entries })
        .map_err(|_| anyhow!("failed to emit plan update"))
}

pub(super) fn emit_available_commands(
    tx: &mpsc::Sender<ClientEvent>,
    update: agent_client_protocol::schema::AvailableCommandsUpdate,
) -> anyhow::Result<()> {
    let commands = update
        .available_commands
        .into_iter()
        .map(|cmd| {
            let input_hint = cmd.input.and_then(|input| match input {
                AvailableCommandInput::Unstructured(u) => Some(u.hint),
                _ => None,
            });
            AvailableCommand {
                name: cmd.name,
                description: cmd.description,
                input_hint,
            }
        })
        .collect();

    tx.send(ClientEvent::AvailableCommandsUpdated { commands })
        .map_err(|_| anyhow!("failed to emit available commands update"))
}

fn normalize_plan_priority(priority: AcpPlanEntryPriority) -> AgentPlanEntryPriority {
    match priority {
        AcpPlanEntryPriority::High => AgentPlanEntryPriority::High,
        AcpPlanEntryPriority::Medium => AgentPlanEntryPriority::Medium,
        AcpPlanEntryPriority::Low => AgentPlanEntryPriority::Low,
        _ => AgentPlanEntryPriority::Medium,
    }
}

fn normalize_plan_status(status: AcpPlanEntryStatus) -> AgentPlanEntryStatus {
    match status {
        AcpPlanEntryStatus::Pending => AgentPlanEntryStatus::Pending,
        AcpPlanEntryStatus::InProgress => AgentPlanEntryStatus::InProgress,
        AcpPlanEntryStatus::Completed => AgentPlanEntryStatus::Completed,
        _ => AgentPlanEntryStatus::Pending,
    }
}

pub(crate) fn session_config_from_options(options: Vec<SessionConfigOption>) -> SessionConfigState {
    let controls = options
        .into_iter()
        .filter_map(normalize_config_option)
        .collect::<Vec<_>>();

    SessionConfigState {
        hydrated: true,
        controls: with_policy_mode_control(controls, None),
    }
}

pub(crate) fn session_config_from_parts(
    options: Option<Vec<SessionConfigOption>>,
    modes: Option<&SessionModeState>,
    models: Option<&SessionModelState>,
) -> SessionConfigState {
    let mut controls = options
        .unwrap_or_default()
        .into_iter()
        .filter_map(normalize_config_option)
        .collect::<Vec<_>>();

    if let Some(model_control) = models.map(session_config_control_from_models)
        && !controls
            .iter()
            .any(|control| control.category == SessionConfigCategory::Model)
    {
        controls.insert(0, model_control);
    }

    SessionConfigState {
        hydrated: true,
        controls: with_policy_mode_control(controls, modes),
    }
}

fn with_policy_mode_control(
    mut controls: Vec<SessionConfigControl>,
    modes: Option<&SessionModeState>,
) -> Vec<SessionConfigControl> {
    let current_mode = controls
        .iter()
        .filter(|control| control.category == SessionConfigCategory::Mode)
        .find_map(|control| policy_mode_id(&control.current_value_id, &control.current_value_label))
        .or_else(|| modes.and_then(policy_mode_from_modes))
        .unwrap_or(BUILD_MODE_ID);

    controls.retain(|control| control.category != SessionConfigCategory::Mode);
    controls.push(policy_mode_control(current_mode));
    controls
}

fn policy_mode_control(current_mode: &str) -> SessionConfigControl {
    let current_value_id = match current_mode {
        BUILD_MODE_ID => BUILD_MODE_ID,
        FULL_ACCESS_MODE_ID => FULL_ACCESS_MODE_ID,
        _ => PLAN_MODE_ID,
    };
    SessionConfigControl {
        id: "mode".into(),
        label: "Mode".into(),
        description: None,
        category: SessionConfigCategory::Mode,
        source: SessionConfigSource::LocalMode,
        current_value_id: current_value_id.into(),
        current_value_label: policy_mode_label(current_value_id).into(),
        choices: vec![
            SessionConfigChoice {
                id: PLAN_MODE_ID.into(),
                label: "Plan".into(),
                description: Some(
                    "Allow workspace reads and markdown writes; reject shell execution".into(),
                ),
                provider: None,
                provider_label: None,
            },
            SessionConfigChoice {
                id: BUILD_MODE_ID.into(),
                label: "Build".into(),
                description: Some(
                    "Allow read-only work automatically; ask before write operations".into(),
                ),
                provider: None,
                provider_label: None,
            },
            SessionConfigChoice {
                id: FULL_ACCESS_MODE_ID.into(),
                label: "完全访问".into(),
                description: Some(
                    "Ask through the same write gate, then approve automatically".into(),
                ),
                provider: None,
                provider_label: None,
            },
        ],
        enabled: true,
    }
}

fn policy_mode_from_modes(modes: &SessionModeState) -> Option<&'static str> {
    let current_mode_id = modes.current_mode_id.0.as_ref();
    modes
        .available_modes
        .iter()
        .find(|mode| mode.id.0.as_ref() == current_mode_id)
        .and_then(|mode| policy_mode_id(mode.id.0.as_ref(), &mode.name))
        .or_else(|| policy_mode_id(current_mode_id, current_mode_id))
}

pub(super) fn policy_mode_id(id: &str, label: &str) -> Option<&'static str> {
    let id = id.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    if id == PLAN_MODE_ID || label == PLAN_MODE_ID || label.contains("plan") {
        return Some(PLAN_MODE_ID);
    }
    if id == FULL_ACCESS_MODE_ID
        || label == FULL_ACCESS_MODE_ID
        || id == "fullaccess"
        || id == "full_access"
        || id == "danger-full-access"
        || id == "bypasspermissions"
        || id == "bypass"
        || label.contains("full access")
        || label.contains("bypass")
        || label.contains("完全访问")
    {
        return Some(FULL_ACCESS_MODE_ID);
    }
    if id == BUILD_MODE_ID
        || label == BUILD_MODE_ID
        || label.contains("build")
        || matches!(id.as_str(), "default" | "acceptedits" | "auto" | "dontask")
        || label.contains("manual")
        || label.contains("accept")
        || label.contains("auto")
        || label.contains("don't ask")
        || label.contains("dont ask")
    {
        return Some(BUILD_MODE_ID);
    }
    None
}

pub(super) fn policy_mode_label(id: &str) -> &'static str {
    match id {
        BUILD_MODE_ID => "Build",
        FULL_ACCESS_MODE_ID => "完全访问",
        _ => "Plan",
    }
}

fn session_config_control_from_models(models: &SessionModelState) -> SessionConfigControl {
    let choices = models
        .available_models
        .iter()
        .map(|model| SessionConfigChoice {
            id: model.model_id.0.to_string(),
            label: model.name.clone(),
            description: model.description.clone(),
            provider: model.meta.as_ref().and_then(provider_from_meta),
            provider_label: model.meta.as_ref().and_then(provider_label_from_meta),
        })
        .collect::<Vec<_>>();
    let current_value_id = models.current_model_id.0.to_string();
    let current_value_label = choices
        .iter()
        .find(|choice| choice.id == current_value_id)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| current_value_id.clone());

    SessionConfigControl {
        id: "model".into(),
        label: "Model".into(),
        description: None,
        category: SessionConfigCategory::Model,
        source: SessionConfigSource::SessionModel,
        current_value_id,
        current_value_label,
        choices,
        enabled: true,
    }
}

pub(super) fn emit_config_option_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: ConfigOptionUpdate,
) -> anyhow::Result<()> {
    tx.send(ClientEvent::SessionConfigUpdated {
        state: session_config_from_options(update.config_options),
    })
    .map_err(|_| anyhow!("failed to emit session config update"))
}

pub(super) fn emit_current_mode_update(
    tx: &mpsc::Sender<ClientEvent>,
    update: CurrentModeUpdate,
) -> anyhow::Result<()> {
    let Some(mode_id) = policy_mode_id(
        update.current_mode_id.0.as_ref(),
        update.current_mode_id.0.as_ref(),
    ) else {
        return Ok(());
    };

    tx.send(ClientEvent::SessionConfigValueChanged {
        control_id: "mode".into(),
        value_id: mode_id.into(),
        value_label: Some(policy_mode_label(mode_id).into()),
    })
    .map_err(|_| anyhow!("failed to emit session mode update"))
}

fn normalize_config_option(option: SessionConfigOption) -> Option<SessionConfigControl> {
    let select = match option.kind {
        SessionConfigKind::Select(select) => select,
        _ => return None,
    };
    let choices = flatten_select_options(select.options);
    let current_value_id = select.current_value.0.to_string();
    let current_value_label = choices
        .iter()
        .find(|choice| choice.id == current_value_id)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| current_value_id.clone());

    Some(SessionConfigControl {
        id: option.id.0.to_string(),
        label: option.name,
        description: option.description,
        category: normalize_category(option.category),
        source: SessionConfigSource::ConfigOption,
        current_value_id,
        current_value_label,
        choices,
        enabled: true,
    })
}

fn flatten_select_options(options: SessionConfigSelectOptions) -> Vec<SessionConfigChoice> {
    match options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .into_iter()
            .map(|option| SessionConfigChoice {
                id: option.value.0.to_string(),
                label: option.name,
                description: option.description,
                provider: option.meta.as_ref().and_then(provider_from_meta),
                provider_label: option.meta.as_ref().and_then(provider_label_from_meta),
            })
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .into_iter()
            .flat_map(|group| group.options)
            .map(|option| SessionConfigChoice {
                id: option.value.0.to_string(),
                label: option.name,
                description: option.description,
                provider: option.meta.as_ref().and_then(provider_from_meta),
                provider_label: option.meta.as_ref().and_then(provider_label_from_meta),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn provider_from_meta(meta: &serde_json::Map<String, Value>) -> Option<String> {
    ["source_provider", "sourceProvider", "provider"]
        .into_iter()
        .find_map(|key| meta.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn provider_label_from_meta(meta: &serde_json::Map<String, Value>) -> Option<String> {
    [
        "source_provider_label",
        "sourceProviderLabel",
        "provider_label",
        "providerLabel",
        "provider_name",
        "providerName",
    ]
    .into_iter()
    .find_map(|key| meta.get(key).and_then(Value::as_str))
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
}
fn normalize_category(category: Option<SessionConfigOptionCategory>) -> SessionConfigCategory {
    match category {
        Some(SessionConfigOptionCategory::Model) => SessionConfigCategory::Model,
        Some(SessionConfigOptionCategory::Mode) => SessionConfigCategory::Mode,
        Some(SessionConfigOptionCategory::ThoughtLevel) => SessionConfigCategory::ThoughtLevel,
        Some(SessionConfigOptionCategory::Other(_)) | Some(_) | None => {
            SessionConfigCategory::Other
        }
    }
}
