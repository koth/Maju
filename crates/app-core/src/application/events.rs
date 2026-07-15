use super::*;

struct MessagePersistenceSnapshot {
    messages_len: usize,
    last_message_id: Option<uuid::Uuid>,
    last_message_body: Option<String>,
}

/// Build a self-contained markdown image string (`![描述](data:...;base64,...)`)
/// for a `generate_image` / `edit_image` tool result, so the generated image
/// renders inline in the conversation timeline and survives even if the local
/// file is deleted. Returns `None` for non-image tool results.
///
/// `raw_output` is the MCP tool result, which may be either:
/// - MCP-wrapped: `{"content":[{"type":"text","text":"<inner-json-string>"}]}` (the
///   `kodex-image` server wraps its JSON result as a text content item), or
/// - a bare JSON object/string.
/// The inner JSON carries `saved_path` (+ `mime_type`) — at top level (edit) or
/// inside `images[]` (generate). The file is read and base64-encoded here so the
/// large data never enters the tool `raw_output` (which the agent consumes and
/// the UI displays verbatim).
fn extract_generated_image_markdown(raw: &str) -> Option<String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use serde_json::Value;
    let trimmed = raw.trim();
    let value: Value = serde_json::from_str(trimmed).ok()?;

    // Unwrap the MCP `{"content":[{"text": "<json>"}]}` envelope, then parse
    // the inner text as JSON (it is itself a JSON string).
    let inner: Value = if let Some(text) = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
    {
        serde_json::from_str(text).ok().unwrap_or(Value::Null)
    } else {
        value
    };

    // Only image tool results carry `images`/`saved_path`; bail for anything else.
    let candidates: Vec<(&Value, &str)> = inner
        .get("images")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(|v| (v, "生成的图片")).collect())
        .unwrap_or_else(|| vec![(&inner, "编辑后的图片")]);

    if candidates.iter().all(|(entry, _)| entry.get("saved_path").is_none()) {
        return None;
    }

    for (entry, alt) in candidates {
        let saved_path = entry.get("saved_path").and_then(Value::as_str)?;
        let mime = entry
            .get("mime_type")
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        let bytes = std::fs::read(saved_path).ok()?;
        let data_url = format!("data:{mime};base64,{}", BASE64.encode(&bytes));
        return Some(format!("![{alt}]({data_url})"));
    }
    None
}

fn message_role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::System => "System",
    }
}

impl Application {
    pub(super) fn next_seq(&mut self) -> i64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    fn message_persistence_snapshot(&self) -> MessagePersistenceSnapshot {
        let last_message_id = self.ui.timeline.last().and_then(|item| match item {
            TimelineItem::Message(id) => Some(*id),
            TimelineItem::Tool(_) | TimelineItem::Thinking => None,
        });
        let last_message_body = last_message_id.and_then(|id| {
            self.ui
                .messages
                .iter()
                .find(|message| message.id == id)
                .map(|message| message.body.clone())
        });

        MessagePersistenceSnapshot {
            messages_len: self.ui.messages.len(),
            last_message_id,
            last_message_body,
        }
    }

    fn persist_changed_message(&mut self, role: &MessageRole, before: MessagePersistenceSnapshot) {
        if self.ui.messages.len() > before.messages_len {
            let messages = self.ui.messages[before.messages_len..]
                .iter()
                .filter(|message| &message.role == role)
                .cloned()
                .collect::<Vec<_>>();
            for message in messages {
                self.persist_message_record(&message);
            }
            return;
        }

        let Some(message_id) = before.last_message_id else {
            return;
        };
        let message = self
            .ui
            .messages
            .iter()
            .find(|message| {
                message.id == message_id
                    && &message.role == role
                    && before.last_message_body.as_deref() != Some(message.body.as_str())
            })
            .cloned();
        if let Some(message) = message {
            self.persist_message_record(&message);
        }
    }

    fn persist_message_record(&mut self, message: &ChatMessage) {
        let session_id = self.ui.session.id.to_string();
        let role = message_role_label(&message.role);
        let seq = self.next_seq();
        if self
            .store
            .insert_message(
                &session_id,
                &message.id.to_string(),
                role,
                &message.body,
                seq,
            )
            .is_err()
        {
            let _ = self
                .store
                .update_message_body(&message.id.to_string(), &message.body);
        }
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

                // NOTE: we intentionally do NOT evict the CodeBuddy proxy
                // session on a ContextCompacted event. The CodeBuddy agent
                // manages its own context compaction transparently; the proxy
                // reuses the pooled CLI session and only sends incremental
                // turns, so evicting would discard the CLI's (self-compacted)
                // history and force a cold reseed. codex CLI itself never
                // auto-compacts for the CodeBuddy provider (catalog reports a
                // ~1B context window), so there is no codex-side compact to
                // resync from either.
            }
            ClientEvent::TurnFinished { .. } => {
                // Keep legacy final-message persistence as a fallback for old event flows.
                let message = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::Assistant)
                    .cloned();
                if let Some(message) = message {
                    self.persist_message_record(&message);
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
            ClientEvent::UsageUpdated { usage } => {
                let _ = self.store.append_usage_event(
                    &session_id,
                    usage,
                    Some(&self.ui.session.model),
                    self.ui.session.agent_cli.as_deref(),
                );
            }
            ClientEvent::ToolStarted { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. }
            | ClientEvent::ToolStopped { id, .. }
            | ClientEvent::ToolStopAvailability { id, .. } => {
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

    /// After a `generate_image` / `edit_image` MCP tool completes, inject an
    /// assistant message that renders the generated image inline via markdown
    /// so it shows up in the conversation timeline (not only as raw tool
    /// output). The tool result carries a `markdown` field
    /// (`![描述](/abs/path.png)`) which MarkdownBody renders directly because
    /// the saved path is an absolute filesystem path.
    fn maybe_inject_generated_image(&mut self, event: &ClientEvent) {
        let ClientEvent::ToolCompleted { raw_output, .. } = event else {
            return;
        };
        let Some(raw) = raw_output.as_deref() else {
            return;
        };
        // MCP tool results for `generate_image`/`edit_image` are detected by
        // content (the tool name is not always carried through to this event),
        // so the parser only succeeds when the output is an image result.
        let Some(markdown) = extract_generated_image_markdown(raw) else {
            return;
        };

        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::Assistant,
            body: markdown,
            created_at: current_timestamp(),
            ..Default::default()
        };
        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message.clone());
        self.persist_message_record(&message);
        self.bump_revision();
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
        // When the agent starts producing output for a steer (the first
        // Assistant message chunk after the steer was accepted) or when the
        // turn ends while steers are still queued, move the queued steers
        // into the timeline first. This places each steer's User message
        // right before the assistant response to it, without cutting the
        // previously-streaming assistant message.
        let flush_for_assistant_chunk = matches!(
            &event,
            ClientEvent::MessageChunk {
                role: workspace_model::MessageRole::Assistant,
                ..
            }
        );
        let flush_for_turn_end = matches!(
            &event,
            ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. }
        );
        if (flush_for_assistant_chunk || flush_for_turn_end)
            && self.flush_pending_steers()
        {
            // The flushed User messages now sit at the end of the timeline,
            // so the following assistant chunk coalesces into a brand-new
            // Assistant message instead of appending to the prior one.
        }
        let message_before = match &event {
            ClientEvent::MessageChunk { .. } => Some(self.message_persistence_snapshot()),
            _ => None,
        };
        self.mark_event_tools_dirty(&event);
        apply_event(&mut self.ui, event.clone());
        if let ClientEvent::ToolPermissionRequest { id, input, .. } = &event {
            if let Some(request) = input.clone() {
                self.broadcast_permission_request(id, request);
            }
        } else {
            self.broadcast_ui_updated();
        }
        if let (ClientEvent::MessageChunk { role, .. }, Some(message_before)) =
            (&event, message_before)
        {
            self.persist_changed_message(role, message_before);
        }
        self.persist_event(&event);
        self.maybe_inject_generated_image(&event);
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
            | ClientEvent::ToolStopAvailability { id, .. }
            | ClientEvent::ToolStopped { id, .. }
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
            | ClientEvent::PlanUpdated { .. }
            | ClientEvent::UsageUpdated { .. } => {}
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
            // Agents echo the user's prompt back as a UserMessageChunk. For a
            // queued steer this echo would re-insert the User message into the
            // timeline mid-stream (cutting the streaming assistant output) and
            // create a duplicate of the already-queued/pending message. Drop
            // the echo when it matches a pending steer; the canonical message
            // is moved into the timeline by `flush_pending_steers` once the
            // agent actually starts responding to the steer.
            ClientEvent::MessageChunk {
                role: workspace_model::MessageRole::User,
                content,
            } if self.steer_echo_matches_pending(content) => None,
            _ => Some(event.clone()),
        }
    }

    /// Whether a streamed User message chunk is the agent echoing back a
    /// queued steer prompt. The echo is suppressed so it does not cut the
    /// streaming assistant output or duplicate the pending steer message.
    /// Matches when the chunk content is a prefix of (or exactly) a pending
    /// steer body — covering the common single-chunk echo and the first
    /// chunk of a multi-chunk echo.
    pub(super) fn steer_echo_matches_pending(&self, content: &str) -> bool {
        if self.ui.pending_steers.is_empty() {
            return false;
        }
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.ui.pending_steers.iter().any(|steer| {
            let body = steer.body.trim();
            !body.is_empty() && body.starts_with(trimmed)
        })
    }

    pub(super) fn prepare_session_config_update(
        &self,
        state: &workspace_model::SessionConfigState,
    ) -> workspace_model::SessionConfigState {
        let mut state = state.clone();
        self.fill_session_config_provider_labels(&mut state);
        if let Some(saved_model) = self.pending_model_restore.as_ref() {
            for control in &mut state.controls {
                if control.category != workspace_model::SessionConfigCategory::Model {
                    continue;
                }

                super::config::apply_model_selection_to_control(control, saved_model);
            }
            return state;
        }

        let Some(selected_model) = self.authoritative_model_selection.as_ref() else {
            for control in &mut state.controls {
                if control.category != workspace_model::SessionConfigCategory::Model {
                    continue;
                }

                super::config::qualify_current_model_control_provider(control);
            }
            return state;
        };

        for control in &mut state.controls {
            if control.category != workspace_model::SessionConfigCategory::Model {
                continue;
            }

            super::config::apply_model_selection_to_control(control, selected_model);
        }

        state
    }

    fn fill_session_config_provider_labels(&self, state: &mut workspace_model::SessionConfigState) {
        for control in &mut state.controls {
            if control.category != workspace_model::SessionConfigCategory::Model {
                continue;
            }

            for choice in &mut control.choices {
                let Some(provider) = super::config::choice_provider(choice) else {
                    continue;
                };
                let current_label = choice.provider_label.as_deref().map(str::trim);
                let needs_label = match current_label {
                    Some(label) if !label.is_empty() && !label.eq_ignore_ascii_case(&provider) => {
                        false
                    }
                    _ => true,
                };
                if needs_label {
                    choice.provider_label = Some(crate::settings::provider_label_for_paths(
                        &self.app_paths,
                        &provider,
                    ));
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    #[test]
    fn extract_markdown_reads_saved_path_to_data_url() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kodex-img-test-{}.png", uuid::Uuid::new_v4()));
        let bytes = b"\x89PNG\r\n\x1a\n fake png";
        std::fs::write(&path, bytes).unwrap();
        let saved_path = path.to_string_lossy().to_string();

        // generate_image: MCP-wrapped {"content":[{"text":"<json>"}]}, inner json has images[]
        let inner = format!(
            "{{\"images\":[{{\"saved_path\":{saved_path:?},\"mime_type\":\"image/png\"}}],\"saved_dir\":\"x\"}}"
        );
        let raw = serde_json::json!({ "content": [{ "type": "text", "text": inner }] }).to_string();
        let md = extract_generated_image_markdown(&raw).expect("markdown built");
        assert!(md.starts_with("![生成的图片](data:image/png;base64,"));
        assert!(md.contains(&BASE64.encode(bytes)));

        // edit_image: MCP-wrapped, inner json has top-level saved_path
        let inner_edit = format!(
            "{{\"saved_path\":{saved_path:?},\"mime_type\":\"image/png\",\"source\":\"in\"}}"
        );
        let raw_edit = serde_json::json!({ "content": [{ "type": "text", "text": inner_edit }] }).to_string();
        let md_edit = extract_generated_image_markdown(&raw_edit).expect("markdown built");
        assert!(md_edit.starts_with("![编辑后的图片](data:image/png;base64,"));

        // non-image tool result -> None
        let raw_other = serde_json::json!({ "content": [{ "type": "text", "text": "{\"files\":[\"a.rs\"]}" }] }).to_string();
        assert!(extract_generated_image_markdown(&raw_other).is_none());

        // missing file -> None
        let raw_bad = serde_json::json!({ "content": [{ "type": "text", "text": "{\"images\":[{\"saved_path\":\"/nope/x.png\"}]}" }] }).to_string();
        assert!(extract_generated_image_markdown(&raw_bad).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
