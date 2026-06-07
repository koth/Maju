use super::*;

impl Application {
    pub(super) fn next_seq(&mut self) -> i64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    pub(super) fn persist_event(&mut self, event: &ClientEvent) {
        let session_id = self.ui.session.id.to_string();
        match event {
            ClientEvent::SessionStarted { session_id: acp_id } => {
                // Persist the agent-side ACP session ID for future session/load.
                let _ = self.store.update_acp_session_id(&session_id, acp_id);
                self.persist_current_codex_provider_if_needed();
            }
            ClientEvent::MessageChunk { .. } => {}
            ClientEvent::ContextCompactionStarted { .. } | ClientEvent::ContextCompacted { .. } => {
                let msg_data = self
                    .ui
                    .timeline
                    .last()
                    .and_then(|item| match item {
                        TimelineItem::Message(id) => Some(id),
                        TimelineItem::Tool(_) | TimelineItem::Thinking => None,
                    })
                    .and_then(|id| {
                        self.ui
                            .messages
                            .iter()
                            .find(|m| m.id == *id && m.role == MessageRole::System)
                    })
                    .map(|m| (m.id.to_string(), m.body.clone()));

                if let Some((id_str, body)) = msg_data {
                    let seq = self.next_seq();
                    if self
                        .store
                        .insert_message(&session_id, &id_str, "System", &body, seq)
                        .is_err()
                    {
                        let _ = self.store.update_message_body(&id_str, &body);
                    }
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
                let _ = self.store.update_session_title(&session_id, title);
            }
            ClientEvent::ToolStarted { id, .. }
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

    pub(super) fn apply_event_with_dirty_tracking(&mut self, event: &ClientEvent) {
        let events = self.filter_inline_think_event(event.clone());
        for event in events {
            self.apply_event_with_dirty_tracking_unfiltered(&event);
        }
    }

    pub(super) fn apply_event_with_dirty_tracking_unfiltered(&mut self, event: &ClientEvent) {
        let Some(event) = self.prepare_event_for_application(event) else {
            return;
        };
        self.mark_event_tools_dirty(&event);
        apply_event(&mut self.ui, event.clone());
        self.persist_event(&event);
    }

    pub(super) fn mark_tool_call_dirty(&mut self, call_id: &str) {
        self.dirty_tool_call_ids.insert(call_id.to_string());
    }

    pub(super) fn mark_running_tools_dirty(&mut self) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    pub(super) fn mark_running_child_tools_dirty(
        &mut self,
        parent_call_id: &str,
        except_call_id: Option<&str>,
    ) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| {
                tool.parent_call_id.as_deref() == Some(parent_call_id)
                    && except_call_id != Some(tool.call_id.as_str())
                    && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            })
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    pub(super) fn mark_event_tools_dirty(&mut self, event: &ClientEvent) {
        match event {
            ClientEvent::ToolMessageChunk { id, .. }
            | ClientEvent::ToolPermissionRequest { id, .. }
            | ClientEvent::ToolPermissionResolved { id, .. }
            | ClientEvent::ToolProgress { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. }
            | ClientEvent::ToolDiff { id, .. }
            | ClientEvent::ToolDiffPreview { id, .. } => {
                self.mark_tool_call_dirty(id);
            }
            ClientEvent::ToolStarted { id, parent_id, .. }
            | ClientEvent::ToolUpdated { id, parent_id, .. } => {
                self.mark_tool_call_dirty(id);
                if let Some(parent_id) = parent_id.as_deref() {
                    self.mark_running_child_tools_dirty(parent_id, Some(id));
                }
            }
            ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. } => {
                self.mark_running_tools_dirty();
            }
            ClientEvent::SessionStarted { .. }
            | ClientEvent::ThinkingActivity { .. }
            | ClientEvent::ContextCompactionStarted { .. }
            | ClientEvent::ContextCompacted { .. }
            | ClientEvent::MessageChunk { .. }
            | ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::PromptCapabilitiesUpdated { .. }
            | ClientEvent::AvailableCommandsUpdated { .. }
            | ClientEvent::SessionTitleUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. }
            | ClientEvent::PlanUpdated { .. } => {}
        }
    }

    pub(super) fn apply_event_and_restore_model(&mut self, event: ClientEvent) {
        let events = self.filter_inline_think_event(event);
        for event in events {
            let Some(event) = self.prepare_event_for_application(&event) else {
                continue;
            };
            let should_restore_model = matches!(event, ClientEvent::SessionConfigUpdated { .. });
            self.mark_event_tools_dirty(&event);
            apply_event(&mut self.ui, event.clone());
            if should_restore_model {
                self.restore_pending_model_selection();
            }
            self.persist_event(&event);
        }
    }

    pub(super) fn prepare_event_for_application(
        &mut self,
        event: &ClientEvent,
    ) -> Option<ClientEvent> {
        match event {
            ClientEvent::SessionTitleUpdated { title } => self.prepare_session_title_update(title),
            ClientEvent::SessionConfigUpdated { state } => {
                Some(ClientEvent::SessionConfigUpdated {
                    state: self.prepare_session_config_update(state),
                })
            }
            ClientEvent::SessionConfigValueChanged {
                control_id,
                value_id,
                ..
            } if control_id == "mode" => {
                let _ = self.session.set_permission_mode(value_id);
                Some(event.clone())
            }
            _ => Some(event.clone()),
        }
    }

    pub(super) fn prepare_session_config_update(
        &self,
        state: &workspace_model::SessionConfigState,
    ) -> workspace_model::SessionConfigState {
        let mut state = state.clone();
        if self.pending_model_restore.is_some() {
            return state;
        }

        let Some(selected_model) = self.authoritative_model_selection.as_deref() else {
            return state;
        };

        for control in &mut state.controls {
            if control.category != workspace_model::SessionConfigCategory::Model {
                continue;
            }

            if let Some(choice) = control
                .choices
                .iter()
                .find(|choice| choice.id == selected_model || choice.label == selected_model)
            {
                control.current_value_id = choice.id.clone();
                control.current_value_label = choice.label.clone();
            }
        }

        state
    }

    pub(super) fn prepare_session_title_update(&mut self, title: &str) -> Option<ClientEvent> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return None;
        }

        if is_placeholder_session_title(trimmed) {
            return None;
        }

        if self.uses_claude_session_titles() && self.title_matches_user_prompt(trimmed) {
            return None;
        }

        self.agent_title_received = true;
        self.needs_title = false;
        self.provisional_prompt_title = None;

        Some(ClientEvent::SessionTitleUpdated {
            title: trimmed.to_string(),
        })
    }

    pub(super) fn title_matches_user_prompt(&self, title: &str) -> bool {
        let normalized_title = normalize_title_for_prompt_compare(title);
        if normalized_title.is_empty() {
            return false;
        }

        self.ui.messages.iter().any(|message| {
            message.role == MessageRole::User
                && normalize_title_for_prompt_compare(&message.body) == normalized_title
        })
    }

    pub(super) fn filter_inline_think_event(&mut self, event: ClientEvent) -> Vec<ClientEvent> {
        match event {
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content,
            } => self
                .inline_think_filter
                .filter_chunk(&content)
                .map(|content| {
                    vec![ClientEvent::MessageChunk {
                        role: MessageRole::Assistant,
                        content,
                    }]
                })
                .unwrap_or_default(),
            ClientEvent::TurnFinished { stop_reason } => {
                let mut events = Vec::new();
                if let Some(content) = self.inline_think_filter.flush() {
                    events.push(ClientEvent::MessageChunk {
                        role: MessageRole::Assistant,
                        content,
                    });
                }
                events.push(ClientEvent::TurnFinished { stop_reason });
                events
            }
            ClientEvent::Interrupted { reason } => {
                self.inline_think_filter.reset();
                vec![ClientEvent::Interrupted {
                    reason: humanize_acp_disconnect_reason(&reason),
                }]
            }
            other => vec![other],
        }
    }
}
