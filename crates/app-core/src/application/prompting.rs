use super::*;

impl Application {
    pub fn send_prompt(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.inline_think_filter.reset();
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;
        let events = self.session.send_prompt(prompt)?;
        let turn_stop_reason = events.iter().rev().find_map(|event| match event {
            ClientEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        });
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        if let Some(stop_reason) = turn_stop_reason.as_deref()
            && let Some(notice) =
                turn_finished_notice(stop_reason, self.ui.session.agent_cli.as_deref())
        {
            self.push_system_message(notice);
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
            let error = anyhow::anyhow!("提示请求已在运行中");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "图片提示".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("提示内容不能为空");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_image(&prompt) && !self.ui.prompt_capabilities.image {
            let error = anyhow::anyhow!("当前智能体不支持图片提示");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_file(&prompt) && !self.ui.prompt_capabilities.embedded_context {
            let error = anyhow::anyhow!("当前智能体不支持文件附件");
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
                    .unwrap_or_else(|| "ACP 子进程意外退出".to_string());
                let reason = humanize_acp_disconnect_reason(&reason);
                let error = anyhow::anyhow!(reason);
                self.push_system_message(format!("会话已断开：{error}"));
                return Err(error);
            }
        }

        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::User,
            body: display_body,
            created_at: current_timestamp(),
        };
        let message_id = message.id;

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
        self.review_changes_started = false;
        self.current_turn_user_message_id = Some(message_id);
        self.inline_think_filter.reset();

        // Step 1: Always install a local fallback immediately. Protocol agents
        // can still replace it later with SessionTitleUpdated.
        if self.needs_title && is_placeholder_session_title(&self.ui.session.title) {
            let title = extract_title_from_prompt(&title_source);
            self.ui.session.title = title.clone();
            self.provisional_prompt_title = Some(title.clone());
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
        self.bump_revision();
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
                    let reason = format!("ACP 子进程退出且重连失败：{error}");
                    let reason = humanize_acp_disconnect_reason(&reason);
                    self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                        reason: reason.clone(),
                    });
                    self.push_system_message(format!("会话已断开：{}", reason));
                    self.bump_revision();
                }
                return;
            }

            let reason = last_error.unwrap_or_else(|| "ACP 子进程意外退出".to_string());
            let reason = humanize_acp_disconnect_reason(&reason);
            self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                reason: reason.clone(),
            });
            self.push_system_message(format!("会话已断开：{}", reason));
            self.bump_revision();
            return;
        }

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            self.session.update_session_id(&events);
            let has_events = !events.is_empty();
            for event in events {
                self.apply_event_and_restore_model(event);
            }
            if has_events {
                self.bump_revision();
            }
            return;
        };

        let events = match in_flight.task.collect_ready_events(&mut self.session) {
            Ok(events) => events,
            Err(error) => {
                self.ui.session.status = SessionStatus::Interrupted;
                self.ui.agent_plan.clear();
                self.push_system_message(format!(
                    "从 `{}` 读取 ACP 事件失败：{}",
                    self.agent_command, error
                ));
                self.in_flight_prompt = None;
                self.current_turn_user_message_id = None;
                self.bump_revision();
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
                    self.bump_revision();
                }
            }
            if is_finished {
                self.skip_replay = false;
                self.in_flight_prompt = None;
                self.current_turn_user_message_id = None;
                self.ui.session.status = SessionStatus::Idle;
                self.bump_revision();
            }
            return;
        }

        // Preprocess ToolDiff events: fill in old_text from the correct baseline.
        // For the tool card diff, old_text should be "what was on disk when the tool started"
        // so the card shows what THIS tool changed.
        // For session-level changes, the reducer's upsert_session_change preserves the
        // first-ever baseline separately.
        let workspace_root = self.ui.workspace.root.clone();
        let mut events = events;
        let mut had_file_changes = false;
        let mut batch_file_versions = HashMap::<String, String>::new();
        let turn_stop_reason = events.iter().rev().find_map(|event| match event {
            ClientEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        });

        // Events are collected in batches. Some agents emit ToolStarted and ToolDiff in
        // the same batch after the file has already been written. Start recording before
        // the ToolDiff preprocessing pass so `get_any_baseline_text` can still supply
        // a baseline instead of letting the card diff against an empty file.
        for event in &events {
            if let ClientEvent::ToolStarted { id, raw_input, .. } = event {
                self.file_tracker
                    .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
            }
        }

        for event in events.iter_mut() {
            match event {
                ClientEvent::ToolDiff {
                    id,
                    path,
                    old_text,
                    new_text,
                    ..
                } => {
                    had_file_changes = true;
                    // Normalize path to workspace-relative with forward slashes
                    let normalized = normalize_path_for_storage(path, &workspace_root);
                    self.file_tracker.add_candidate(id, normalized.clone());
                    let abs_path = workspace_root.join(&normalized);
                    if let Some((expanded_old, expanded_new)) = expand_tool_diff_fragment_from_disk(
                        &abs_path,
                        old_text.as_deref(),
                        new_text,
                    ) {
                        *old_text = Some(expanded_old);
                        *new_text = expanded_new;
                    }
                    let old_text_is_untrusted = old_text.as_deref().map_or(true, |text| {
                        text.is_empty() || looks_like_fragment_to_full_file_text(text, new_text)
                    });
                    if old_text_is_untrusted {
                        // 1. For multiple ToolDiffs for the same file in one poll batch,
                        // use the previous diff's new_text. This keeps each ToolCard scoped
                        // to this tool's own edit instead of every card comparing against an
                        // empty/missing base and showing the whole file as added.
                        if let Some(baseline) = self.tool_diff_baseline_text(
                            id,
                            &normalized,
                            new_text,
                            &batch_file_versions,
                        ) {
                            *old_text = Some(baseline);
                        } else if old_text.as_deref().is_some_and(str::is_empty) {
                            *old_text = None;
                        }
                    } else if old_text.as_deref().is_some_and(|text| {
                        normalize_diff_text_for_session_change(text)
                            == normalize_diff_text_for_session_change(new_text)
                    }) {
                        *old_text = None;
                    }

                    // Last resort requested by user: read the file directly only when
                    // the file on disk is different from the preview target. If it is
                    // already equal, treating an unknown baseline as "created" would
                    // make the UI show the whole file as added.
                    if old_text.is_none()
                        && let Ok(content) = std::fs::read_to_string(&abs_path)
                        && normalize_diff_text_for_session_change(&content)
                            != normalize_diff_text_for_session_change(new_text)
                    {
                        *old_text = Some(normalize_diff_text_for_session_change(&content));
                    }
                    batch_file_versions.insert(normalized.clone(), new_text.clone());
                    *path = normalized;
                }
                ClientEvent::ToolDiffPreview { id, path, .. } => {
                    let normalized = normalize_path_for_storage(path, &workspace_root);
                    self.file_tracker.add_candidate(id, normalized.clone());
                    *path = normalized;
                }
                _ => {}
            }
        }

        // Process events and track tool lifecycle for file change detection
        let mut ui_changed = !events.is_empty();
        let mut completed_tool_ids = Vec::new();
        let mut failed_tool_ids_without_changes = HashSet::new();
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, raw_input, .. } => {
                    self.file_tracker
                        .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
                }
                ClientEvent::ToolUpdated { id, raw_input, .. } => {
                    for path in tool_event_hint_paths(raw_input.as_deref()) {
                        self.file_tracker.add_candidate(id, path);
                    }
                }
                ClientEvent::ToolCompleted { id, .. } => {
                    completed_tool_ids.push(id.clone());
                    let changes = self.file_tracker.finish_recording(id);
                    had_file_changes |= self.apply_tracker_changes(id, changes);
                }
                ClientEvent::ToolFailed { id, .. } => {
                    completed_tool_ids.push(id.clone());
                    let changes = self.file_tracker.finish_recording(id);
                    let tracker_changed = self.apply_tracker_changes(id, changes);
                    if !tracker_changed {
                        failed_tool_ids_without_changes.insert(id.clone());
                    }
                    had_file_changes |= tracker_changed;
                }
                _ => {}
            }
            if matches!(event, ClientEvent::SessionConfigUpdated { .. }) {
                self.apply_event_and_restore_model(event.clone());
            } else {
                self.apply_event_with_dirty_tracking(event);
            }
            if let ClientEvent::ToolFailed { id, .. } = event
                && failed_tool_ids_without_changes.contains(id)
            {
                had_file_changes |= self.discard_failed_tool_speculative_diffs(id);
            }
            if let ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
                ..
            } = event
            {
                if self.should_apply_tool_diff_to_review(path, old_text.as_deref(), new_text) {
                    let change_type = self.tool_diff_change_type(id, path, old_text.as_deref());
                    self.upsert_review_file_change(
                        path,
                        change_type,
                        old_text.clone(),
                        new_text.clone(),
                    );
                    had_file_changes = true;
                }
            }
            if let ClientEvent::ToolDiffPreview { path, hunks, .. } = event {
                had_file_changes |= self.apply_tool_diff_preview_to_review(path, path, hunks);
            }
        }
        self.session.update_session_id(&events);

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        if !completed_tool_ids.is_empty() {
            had_file_changes |= self.detect_file_writes_from_tools(&completed_tool_ids);
        }

        // Persist session_changes to SQLite after all file-change sources have run.
        if had_file_changes {
            self.persist_file_changes();
            self.persist_review_file_changes();
        }

        if is_finished {
            if self.ui.session.status == SessionStatus::Streaming {
                self.ui.session.status = SessionStatus::Idle;
                ui_changed = true;
            }

            // Step 2: If ACP did not provide title metadata yet, refine the local fallback.
            if self.refine_session_title_after_turn_if_needed() {
                ui_changed = true;
            }

            ui_changed |= self.persist_current_turn_file_changes();
            if let Some(stop_reason) = turn_stop_reason.as_deref()
                && let Some(notice) =
                    turn_finished_notice(stop_reason, self.ui.session.agent_cli.as_deref())
            {
                self.push_system_message(notice);
                ui_changed = true;
            }
            self.current_turn_user_message_id = None;
            self.in_flight_prompt = None;
        }

        if ui_changed || had_file_changes {
            self.bump_revision();
        }
    }

    pub fn has_in_flight_prompt(&self) -> bool {
        self.in_flight_prompt.is_some()
    }

    pub fn cancel_prompt(&mut self) -> Result<(), String> {
        if self.in_flight_prompt.is_none() {
            return Ok(());
        }
        self.session
            .cancel_prompt()
            .map_err(|error| error.to_string())?;
        self.mark_current_turn_cancelled();
        self.bump_revision();
        Ok(())
    }

    pub(super) fn mark_current_turn_cancelled(&mut self) {
        let session_id = self.ui.session.id.to_string();
        let mut cancelled_tools = Vec::new();
        let mut dirty_tool_call_ids = Vec::new();

        for tool in self
            .ui
            .tools
            .iter_mut()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
        {
            dirty_tool_call_ids.push(tool.call_id.clone());
            tool.status = ToolStatus::Interrupted;
            if tool.summary.trim().is_empty()
                || tool.summary == "等待活动"
                || tool.summary.starts_with("等待权限")
            {
                tool.summary = "已取消".into();
            }
            if tool.kind == "permission" && tool.permission_decision.is_none() {
                tool.permission_decision = Some("已取消".into());
            }
            tool.logs.push(ToolLogEntry {
                title: "已取消".into(),
                body: "客户端发送了 session/cancel 取消当前轮次".into(),
            });
            cancelled_tools.push(tool.clone());
        }
        self.dirty_tool_call_ids.extend(dirty_tool_call_ids);

        for tool in cancelled_tools {
            let seq = self.next_seq();
            let _ = self.store.insert_tool(&session_id, &tool, seq);
        }
    }

    // ── Title refinement ──

    /// After the first turn completes, try to extract a better local fallback
    /// from the assistant's response. ACP session metadata wins when present.
    pub(super) fn refine_session_title_after_turn_if_needed(&mut self) -> bool {
        if !self.needs_title || self.agent_title_received {
            return false;
        }

        self.needs_title = false;
        self.refine_session_title();
        true
    }

    /// Try to extract a better local fallback from the assistant's response.
    /// ACP session metadata can still replace it later.
    pub(super) fn refine_session_title(&mut self) {
        let Some(assistant_body) = self.first_assistant_body_for_title() else {
            return;
        };

        // Try to extract a meaningful title from the assistant's first sentence.
        // Common patterns: "I'll help you X", "Let me X", "Here's how to X", etc.
        let refined = extract_title_from_response(&assistant_body);
        if let Some(title) = refined {
            self.ui.session.title = title.clone();
            self.provisional_prompt_title = None;
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }
        // If extraction fails, keep the truncated user prompt title from Step 1
    }

    fn first_assistant_body_for_title(&self) -> Option<String> {
        let start_index = self
            .current_turn_user_message_id
            .and_then(|id| self.ui.messages.iter().position(|message| message.id == id))
            .or_else(|| {
                self.ui
                    .messages
                    .iter()
                    .position(|message| message.role == MessageRole::User)
            })?;

        self.ui
            .messages
            .iter()
            .skip(start_index.saturating_add(1))
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| message.body.clone())
    }

    pub(super) fn uses_claude_session_titles(&self) -> bool {
        self.ui
            .session
            .agent_cli
            .as_deref()
            .is_some_and(is_claude_agent_label)
    }

    // ── Internal helpers ──

    pub(super) fn push_system_message(&mut self, body: impl Into<String>) {
        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::System,
            body: body.into(),
            created_at: current_timestamp(),
        };
        let seq = self.next_seq();
        let _ = self.store.insert_message(
            &self.ui.session.id.to_string(),
            &message.id.to_string(),
            "System",
            &message.body,
            seq,
        );
        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
    }
}
