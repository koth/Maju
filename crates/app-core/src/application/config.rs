use super::diff_utils::{
    is_file_write_tool_identity, tool_command_write_hint_paths, tool_event_hint_paths,
};
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
        let is_model_control = control.category == workspace_model::SessionConfigCategory::Model;
        let selected_choice = control
            .choices
            .iter()
            .find(|choice| {
                if is_model_control {
                    model_choice_matches_request(choice, value_id, provider)
                } else {
                    choice.id == value_id
                        && provider.map_or(true, |provider| {
                            choice_provider(choice).as_deref() == Some(provider)
                        })
                }
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

        let selected_control_id = control.id.clone();
        let selected_label = Some(selected_choice.label.clone());
        let selected_provider = provider
            .map(str::to_string)
            .or_else(|| provider_from_model_value(value_id).map(str::to_string))
            .or_else(|| choice_provider(&selected_choice));
        let selected_provider_for_state = selected_provider.clone();
        let (request_value_id, request_provider) = if is_model_control {
            model_request_value_and_provider(value_id, selected_provider.clone())
        } else {
            (value_id.to_string(), selected_provider.clone())
        };

        let events = match control.source.clone() {
            SessionConfigSource::ConfigOption => self
                .session
                .set_config_option(
                    control.id.clone(),
                    request_value_id.clone(),
                    request_provider,
                )
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LegacyMode => self
                .session
                .set_mode(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::SessionModel => self
                .session
                .set_model(request_value_id.clone(), request_provider)
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LocalMode => {
                let is_codex_agent = self.is_codex_acp_session();
                self.session
                    .set_permission_mode(value_id)
                    .map_err(|error| error.to_string())?;
                sync_codex_agent_mode_for_policy_mode(
                    &mut self.session,
                    is_codex_agent,
                    Some(value_id),
                )?;
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

        if is_model_control {
            self.pending_model_restore = None;
            // Capture the provider for image-capability re-resolution before it
            // is moved into the authoritative model selection below.
            let caps_provider = selected_provider_for_state.clone();
            self.authoritative_model_selection = Some(ModelSelection::new(
                request_value_id.clone(),
                selected_provider_for_state,
            ));
            // Install the authoritative selection before applying agent-side
            // config events. Those events call `persist_session_model_mode`,
            // which must not fall back to inferring a provider from bare model
            // names when the same model id is offered by multiple providers.
            for event in events {
                self.apply_event_with_dirty_tracking(&event);
            }
            let ui_value_id = provider_qualified_model_value(
                &request_value_id,
                self.current_model_provider_for_persistence().as_deref(),
            );
            self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
                control_id: selected_control_id,
                value_id: ui_value_id,
                value_label: selected_label,
            });
            // A model switch can change native image understanding/generation.
            // Re-resolve capabilities and update the running image MCP server's
            // offered tool set (and the prompt-capability gate) without
            // restarting the server (design D10).
            self.reapply_image_capabilities(&request_value_id, caps_provider.as_deref());
        } else {
            for event in events {
                self.apply_event_with_dirty_tracking(&event);
            }
        }
        self.persist_session_model_mode();
        self.bump_revision();

        Ok(self.ui.session_config.clone())
    }

    /// Re-resolve native image capabilities after a model switch and propagate
    /// the result to the running image MCP server's `tools/list` trim and to
    /// the prompt-capability gate, without restarting the server.
    ///
    /// `native_view` is always re-resolved from the model name so text-only
    /// models gate image attachments correctly even when no fallback MCP is
    /// attached (Bug 1). `view_fallback` reflects whether the `kodex-image`
    /// MCP server is currently attached. The prompt gate becomes
    /// `native_view || view_fallback`, allowing text-only models to accept
    /// image attachments that are degraded through `view_image` (Bug 3).
    pub(super) fn reapply_image_capabilities(&mut self, model: &str, provider: Option<&str>) {
        let mut caps = crate::image_capability::resolve_image_capabilities(
            model,
            provider,
            &self.agent_command,
        );
        caps.view_fallback = self.image_mcp.is_some();
        self.ui.image_capabilities = caps;
        // `prompt_capabilities.image` is normally derived from the agent
        // handshake in the reducer's `PromptCapabilitiesUpdated` handler; a
        // model switch does not re-run the handshake, so set it directly from
        // the freshly resolved capabilities.
        self.ui.prompt_capabilities.image = caps.image_capable();
        if let Some(handle) = self.image_mcp.as_ref() {
            handle.update_capabilities(caps);
        }
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
        // Remote (relay/phone) sessions never auto-approve destructive
        // permissions, even in full-access mode — the phone must approve.
        if self.remote_mode {
            return false;
        }
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

        let mut paths = Vec::new();
        if let Some(raw_input) = tool.raw_input.as_deref() {
            paths.extend(permission_details_write_paths(raw_input));
            paths.extend(tool_event_hint_paths(Some(raw_input)));
        }
        if !tool.detail_text.trim().is_empty() {
            paths.extend(permission_details_write_paths(&tool.detail_text));
        }
        paths.sort();
        paths.dedup();
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
        // Persist the provider-qualified model value so the provider is
        // always embedded in the non-null `model` column and survives a
        // session reopen even when the separate `model_provider` column is
        // empty. On restore the provider is recovered via
        // `provider_from_model_value` and the bare label via
        // `display_model_from_persisted`.
        let session_id = self.ui.session.id.to_string();
        let existing = self
            .store
            .get_session_model_provider_mode(&session_id)
            .ok()
            .flatten();
        // Prefer the live selection. If it is temporarily unavailable (e.g.
        // agent config events land while restore is still pending, or the
        // control only carries a bare model id shared by multiple providers),
        // keep the previously persisted provider instead of writing NULL and
        // downgrading `kodex-provider/<p>/<m>` to a bare model name. That
        // downgrade is what makes session A reopen as session B's provider
        // when both use the same model id.
        let provider = self.current_model_provider_for_persistence().or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, provider, _)| {
                    provider.as_deref().and_then(real_provider).map(str::to_string)
                })
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|(model, _, _)| provider_from_model_value(model).map(str::to_string))
                })
        });
        let display_model = display_model_from_persisted(&self.ui.session.model);
        let persisted_model = if let Some(provider) = provider.as_deref() {
            provider_qualified_model_value(&display_model, Some(provider))
        } else if let Some((existing_model, _, _)) = existing.as_ref() {
            // Unknown provider: never replace a still-qualified stored value
            // with the bare UI label.
            if provider_from_model_value(existing_model).is_some() {
                existing_model.clone()
            } else {
                display_model
            }
        } else {
            display_model
        };
        let _ = self.store.update_session_model_mode_provider(
            &session_id,
            &persisted_model,
            provider.as_deref(),
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

        let saved_provider = saved_model
            .provider
            .as_deref()
            .and_then(real_provider)
            .map(str::to_string)
            .or_else(|| provider_from_model_value(&saved_model.value).map(str::to_string));

        // `choice_matches_model_selection` is provider-aware when the provider
        // is known. When it cannot be recovered (e.g. a pre-migration row
        // downgraded to a bare model name), it would otherwise match the first
        // choice offering that model name, silently switching the session to
        // whichever provider happens to be listed first. In that case only
        // restore when the model is offered by a single provider; otherwise
        // leave `pending_model_restore` intact so a later, richer config
        // update resolves it instead of committing a guessed provider.
        let choice = match saved_provider.as_deref() {
            Some(_) => model_control
                .choices
                .iter()
                .find(|choice| choice_matches_model_selection(choice, &saved_model))
                .cloned(),
            None => {
                let value =
                    model_from_provider_value(&saved_model.value).unwrap_or(&saved_model.value);
                let mut providers: Vec<String> = model_control
                    .choices
                    .iter()
                    .filter(|choice| choice_matches_model_value(choice, value))
                    .filter_map(choice_provider)
                    .collect();
                providers.sort();
                providers.dedup();
                if providers.len() == 1 {
                    model_control
                        .choices
                        .iter()
                        .find(|choice| choice_matches_model_value(choice, value))
                        .cloned()
                } else {
                    None
                }
            }
        };
        let Some(choice) = choice else {
            return;
        };

        let control_id = model_control.id.clone();
        let value_id = choice.id.clone();
        let value_label = choice.label.clone();
        let value_provider = saved_provider.or_else(|| choice_provider(&choice));
        let (request_value_id, request_provider) =
            model_request_value_and_provider(&value_id, value_provider.clone());
        let result = match model_control.source {
            SessionConfigSource::ConfigOption => self.session.set_config_option(
                control_id.clone(),
                request_value_id.clone(),
                request_provider.clone(),
            ),
            SessionConfigSource::SessionModel => self
                .session
                .set_model(request_value_id.clone(), request_provider.clone()),
            SessionConfigSource::LegacyMode | SessionConfigSource::LocalMode => self
                .session
                .set_model(request_value_id.clone(), request_provider.clone()),
        };
        let Ok(events) = result else {
            return;
        };
        self.pending_model_restore = None;
        // Install the authoritative selection before applying agent-side
        // config events. Intermediate `SessionConfigUpdated` /
        // `SessionConfigValueChanged` events persist model mode and must not
        // fall back to bare-model provider inference while restore is still
        // in flight.
        self.authoritative_model_selection = Some(ModelSelection::new(
            request_value_id.clone(),
            value_provider.clone(),
        ));
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.apply_event_with_dirty_tracking(&ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id: provider_qualified_model_value(&request_value_id, value_provider.as_deref()),
            value_label: Some(value_label),
        });
    }

    pub(super) fn current_model_provider_for_persistence(&self) -> Option<String> {
        let authoritative = self.authoritative_model_selection.as_ref();
        let pending = self.pending_model_restore.as_ref();
        let candidates = [
            authoritative.and_then(|selection| selection.provider.clone()),
            authoritative
                .and_then(|selection| provider_from_model_value(&selection.value).map(str::to_string)),
            pending.and_then(|selection| selection.provider.clone()),
            pending.and_then(|selection| provider_from_model_value(&selection.value).map(str::to_string)),
        ];
        // Skip the generic "byok" wrapper id: it is not a per-model source
        // provider and writing it to the `model_provider` column (or embedding
        // it into the model value) corrupts the row so the real provider is
        // lost across reopens. Fall through to the next candidate instead.
        for candidate in candidates {
            if let Some(provider) = candidate.as_deref().and_then(real_provider) {
                return Some(provider.to_string());
            }
        }

        let model_control =
            self.ui.session_config.controls.iter().find(|control| {
                control.category == workspace_model::SessionConfigCategory::Model
            })?;

        infer_current_model_provider(model_control)
            .and_then(|provider| real_provider(&provider).map(str::to_string))
    }
}

pub(super) fn sync_codex_agent_mode_for_policy_mode(
    session: &mut SessionHandle,
    is_codex_agent: bool,
    policy_mode: Option<&str>,
) -> Result<(), String> {
    if !is_codex_agent {
        return Ok(());
    }
    let Some(agent_mode) = codex_agent_mode_for_policy_mode(policy_mode.unwrap_or("Build")) else {
        return Ok(());
    };
    session
        .set_config_option("mode", agent_mode, None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(super) fn queue_codex_agent_mode_for_policy_mode(
    session: &mut SessionHandle,
    is_codex_agent: bool,
    policy_mode: Option<&str>,
) -> Result<(), String> {
    if !is_codex_agent {
        return Ok(());
    }
    let Some(agent_mode) = codex_agent_mode_for_policy_mode(policy_mode.unwrap_or("Build")) else {
        return Ok(());
    };
    session
        .queue_config_option("mode", agent_mode, None)
        .map_err(|error| error.to_string())
}

fn codex_agent_mode_for_policy_mode(policy_mode: &str) -> Option<&'static str> {
    match policy_mode.to_ascii_lowercase().as_str() {
        "plan" | "read-only" | "readonly" => Some("read-only"),
        "build" | "auto" => Some("auto"),
        "full-access" | "fullaccess" | "full_access" | "danger-full-access" => Some("full-access"),
        _ => None,
    }
}

fn model_choice_matches_request(
    choice: &workspace_model::SessionConfigChoice,
    value_id: &str,
    provider: Option<&str>,
) -> bool {
    let request_value = model_from_provider_value(value_id).unwrap_or(value_id);
    let choice_id = model_from_provider_value(&choice.id).unwrap_or(&choice.id);
    let choice_label = model_from_provider_value(&choice.label).unwrap_or(&choice.label);
    let value_matches = choice.id == value_id
        || choice.label == value_id
        || choice.id == request_value
        || choice.label == request_value
        || choice_id == value_id
        || choice_label == value_id
        || choice_id == request_value
        || choice_label == request_value;
    if !value_matches {
        return false;
    }

    let Some(provider) = provider.or_else(|| provider_from_model_value(value_id)) else {
        return true;
    };
    if provider == "byok" {
        return true;
    }

    choice_provider(choice).is_some_and(|candidate| candidate == provider)
}

fn model_request_value_and_provider(
    value_id: &str,
    provider: Option<String>,
) -> (String, Option<String>) {
    if provider_from_model_value(value_id).is_some() {
        return (value_id.to_string(), None);
    }
    if provider.as_deref() == Some("custom") {
        return (
            provider_qualified_model_value(value_id, Some("custom")),
            None,
        );
    }
    (value_id.to_string(), provider)
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
    let selection_provider = selection
        .provider
        .as_deref()
        .and_then(real_provider)
        .or_else(|| provider_from_model_value(&selection.value))
        .map(str::to_string);
    let selection_label = model_from_provider_value(&selection.value).unwrap_or(&selection.value);

    // When the provider is known, match provider-aware so reopening a session
    // restored as provider p1 does not snap to whichever provider happens to
    // list the model first.
    //
    // When the provider cannot be recovered (a bare model id with a NULL
    // model_provider column) and the model is offered by more than one
    // provider, do NOT commit the first matching provider — that both
    // displays the wrong provider and corrupts the persisted row so every
    // later reopen keeps the guessed provider (session A reopens as session
    // B's provider when both share the same model id). Leave the control
    // unqualified so a later, richer config update (or the user) resolves
    // the provider, mirroring `restore_pending_model_selection`'s skip-guess
    // branch.
    let matched = if selection_provider.is_some() {
        control
            .choices
            .iter()
            .find(|choice| choice_matches_model_selection(choice, selection))
            .cloned()
    } else {
        // Provider unrecoverable. Only skip guessing when the bare model is
        // provably offered by more than one *known* provider. When choices
        // carry no provider meta we cannot establish ambiguity, so fall back
        // to a label/id match (the historical behavior) — otherwise sessions
        // whose agent catalog only has bare model ids would never resolve.
        let matching: Vec<&workspace_model::SessionConfigChoice> = control
            .choices
            .iter()
            .filter(|choice| choice_matches_model_value(choice, selection_label))
            .collect();
        let mut known_providers: Vec<String> =
            matching.iter().filter_map(|&choice| choice_provider(choice)).collect();
        known_providers.sort();
        known_providers.dedup();
        if known_providers.len() > 1 {
            None
        } else {
            matching.into_iter().next().cloned()
        }
    };

    if let Some(choice) = matched {
        let provider = selection_provider
            .clone()
            .or_else(|| choice_provider(&choice));
        control.current_value_id = provider_qualified_model_value(&choice.id, provider.as_deref());
        control.current_value_label = choice.label.clone();
        return;
    }

    // Fallback: keep the bare label unqualified when no provider is known, so
    // the persisted row is not downgraded to a guessed provider.
    control.current_value_id =
        provider_qualified_model_value(selection_label, selection_provider.as_deref());
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
    let Some(provider) = provider.and_then(real_provider) else {
        return value.to_string();
    };
    if provider_from_model_value(value).is_none() {
        if byok_source_provider_id(provider).is_some() {
            return format!("kodex-provider/byok/{provider}/{value}");
        }
        return format!("kodex-provider/{provider}/{value}");
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

/// Re-qualify a persisted model value so the `model` column keeps the provider
/// embedded. `display_model` is the bare label shown in the UI; `persisted_model`
/// is the raw value read from storage (possibly already provider-qualified).
/// When the separate `model_provider` column is NULL (e.g. pre-migration
/// sessions), recover the provider from the qualified `persisted_model` value
/// so re-qualification does not downgrade `kodex-provider/<p>/<m>` to a bare
/// model name and lose the provider across session reopens. Returns the
/// re-qualified model value and the effective provider to persist.
pub(super) fn requalify_persisted_model(
    display_model: &str,
    persisted_model: &str,
    provider: Option<&str>,
) -> (String, Option<String>) {
    let effective = provider
        .and_then(real_provider)
        .map(str::to_string)
        .or_else(|| provider_from_model_value(persisted_model).map(str::to_string));
    let qualified = provider_qualified_model_value(display_model, effective.as_deref());
    (qualified, effective)
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
        let (provider, model) = rest.split_once('/')?;
        if provider == "byok" {
            if let Some((source_provider, _)) = model.split_once('/') {
                if byok_source_provider_id(source_provider).is_some() {
                    return Some(source_provider);
                }
            }
            // `kodex-provider/byok/<model>` without a source-provider segment
            // is malformed and unrecoverable: the generic "byok" id is not a
            // per-model source provider. Returning None (rather than "byok")
            // keeps the preserve-existing / skip-guess restore logic engaged so
            // a session is not snapped to another session's provider.
            return None;
        }
        return Some(provider);
    }
    if let Some(rest) = trimmed.strip_prefix("kodex-provider:") {
        return rest.split_once(':').map(|(provider, _)| provider);
    }
    None
}

fn model_from_provider_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("kodex-provider/") {
        let (provider, model) = rest.split_once('/')?;
        if provider == "byok" {
            if let Some((source_provider, source_model)) = model.split_once('/') {
                if byok_source_provider_id(source_provider).is_some() {
                    return Some(source_model);
                }
            }
        }
        return Some(model);
    }
    if let Some(rest) = trimmed.strip_prefix("kodex-provider:") {
        return rest.split_once(':').map(|(_, model)| model);
    }
    None
}

/// Strip the provider prefix from a persisted model value so the UI can show
/// the bare model label. Persisted values are stored with the provider
/// embedded (see [`provider_qualified_model_value`]) so the provider survives
/// a session reopen even when the separate `model_provider` column is empty.
pub(super) fn display_model_from_persisted(stored: &str) -> String {
    model_from_provider_value(stored)
        .unwrap_or(stored.trim())
        .to_string()
}

fn byok_source_provider_id(provider: &str) -> Option<&str> {
    // User-configured BYOK sources are generated as `custom_*` ids (see
    // `custom_provider_id_base`), so recognize them structurally in addition to
    // the built-in sources. Without this, a correctly-encoded value like
    // `kodex-provider/byok/custom_quest/<model>` fails the static match below,
    // falls back to the generic "byok" id, and is then persisted as the
    // malformed `kodex-provider/byok/<model>` — losing the real provider so
    // every reopen snaps to whichever session wrote last.
    let trimmed = provider.trim();
    let is_byok_source = matches!(
        trimmed,
        "timiai" | "commandcode" | "codebuddy" | "deepseek" | "kimi_code" | "xiaomi_mimo"
            | "custom"
    ) || trimmed.starts_with("custom_");
    if is_byok_source {
        Some(trimmed)
    } else {
        None
    }
}

/// Returns the provider unless it is the generic BYOK wrapper id ("byok"),
/// which is not a per-model source provider. The generic id can never be
/// embedded into a qualified model value (the result
/// `kodex-provider/byok/<model>` is missing the source-provider segment and
/// decodes back to "byok" instead of the real provider) and can never match a
/// model choice's source provider. Treating it as unrecoverable keeps
/// restore/persist from snapping a session to another session's provider when
/// the real provider was lost (e.g. legacy rows persisted with the generic id).
fn real_provider(provider: &str) -> Option<&str> {
    let trimmed = provider.trim();
    (!trimmed.is_empty() && trimmed != "byok").then_some(trimmed)
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
    let mut previous_was_write_file = false;

    for line in details.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            previous_was_write_file = false;
            continue;
        }
        if previous_was_write_file {
            paths.push(trimmed.to_string());
            previous_was_write_file = false;
            in_paths_section = false;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Write file") {
            previous_was_write_file = true;
            in_paths_section = false;
            continue;
        }
        if let Some(path) = trimmed.strip_prefix("Path:") {
            let path = path.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
            in_paths_section = false;
            previous_was_write_file = false;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Paths:") {
            in_paths_section = true;
            previous_was_write_file = false;
            continue;
        }
        if trimmed.ends_with(':') {
            in_paths_section = false;
            previous_was_write_file = false;
            continue;
        }
        if in_paths_section && let Some(path) = trimmed.strip_prefix("- ") {
            let path = path.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
            previous_was_write_file = false;
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::{codebuddy_interruption_decision, permission_details_write_paths};

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

    #[test]
    fn permission_details_write_paths_parses_write_file_details() {
        assert_eq!(
            permission_details_write_paths("Write file\n/Users/me/project/src/new_file.rs"),
            vec!["/Users/me/project/src/new_file.rs"]
        );
    }
}

#[cfg(test)]
mod model_config_tests {
    use super::*;

    #[test]
    fn custom_model_request_uses_slash_encoded_value() {
        let (value_id, provider) =
            model_request_value_and_provider("lab-model", Some("custom".to_string()));

        assert_eq!(value_id, "kodex-provider/byok/custom/lab-model");
        assert_eq!(provider, None);
    }

    #[test]
    fn model_choice_request_matches_encoded_custom_value_to_bare_choice() {
        let choice = workspace_model::SessionConfigChoice {
            id: "lab-model".into(),
            label: "lab-model".into(),
            description: None,
            provider: Some("custom".into()),
            provider_label: Some("Lab Provider".into()),
        };

        assert!(model_choice_matches_request(
            &choice,
            "kodex-provider/byok/lab-model",
            None,
        ));
    }
}
