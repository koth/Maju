use super::*;

pub(super) struct RuntimeEventApplyResult {
    pub(super) ui_changed: bool,
    pub(super) had_file_changes: bool,
    pub(super) turn_stop_reason: Option<String>,
}

fn events_may_affect_review_changes(events: &[ClientEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            ClientEvent::ToolDiff { .. }
                | ClientEvent::ToolDiffPreview { .. }
                | ClientEvent::ToolCompleted { .. }
                | ClientEvent::ToolFailed { .. }
        )
    })
}

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
        self.send_prompt_content_background_inner(prompt, None)
    }

    pub fn retry_user_message_background(
        &mut self,
        message_id: &str,
        text: String,
    ) -> anyhow::Result<()> {
        let message_id = uuid::Uuid::parse_str(message_id)
            .map_err(|_| anyhow::anyhow!("无效的消息 ID：{message_id}"))?;
        self.send_prompt_content_background_inner(
            vec![UserPromptContent::text(text)],
            Some(message_id),
        )
    }

    fn send_prompt_content_background_inner(
        &mut self,
        mut prompt: Vec<UserPromptContent>,
        existing_user_message_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<()> {
        if self.in_flight_prompt.is_some() {
            let error = anyhow::anyhow!("提示请求已在运行中");
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

        if prompt_has_image(&prompt) {
            let _ = crate::attachment_cache::cache_prompt_images(
                &mut prompt,
                &self.app_paths.attachments_dir(),
            );
        }
        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "图片提示".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("提示内容不能为空");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let existing_user_message_index = existing_user_message_id
            .map(|id| self.retryable_user_message_index(id))
            .transpose()?;

        if let Some((message_id, message_index)) =
            existing_user_message_id.zip(existing_user_message_index)
        {
            self.remove_retry_artifacts_after_message(message_index);
            if let Some(message) = self
                .ui
                .messages
                .iter_mut()
                .find(|message| message.id == message_id)
            {
                message.body = display_body.clone();
            }
            let _ = self
                .store
                .update_message_body(&message_id.to_string(), &display_body);
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

        let message_id = if let Some(message_id) = existing_user_message_id {
            message_id
        } else {
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
            message_id
        };
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
        self.poll_current_runtime_progress();
        self.poll_background_runtimes();
        self.retire_idle_background_runtimes();
    }

    pub(super) fn poll_current_runtime_progress(&mut self) {
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

        let pending_retry_changed = self.retry_pending_tool_write_detections();

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            if events.is_empty() {
                if pending_retry_changed {
                    self.bump_revision();
                }
                return;
            }
            if self.skip_replay {
                self.session.update_session_id(&events);
                for event in events {
                    self.apply_event_and_restore_model(event);
                }
                self.bump_revision();
                return;
            }
            let result = self.apply_idle_runtime_events_with_file_tracking(events);
            if pending_retry_changed || result.ui_changed || result.had_file_changes {
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

        let result = self.apply_runtime_events_with_file_tracking(events);
        let mut ui_changed = result.ui_changed;
        let had_file_changes = result.had_file_changes;

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
            if let Some(stop_reason) = result.turn_stop_reason.as_deref()
                && let Some(notice) =
                    turn_finished_notice(stop_reason, self.ui.session.agent_cli.as_deref())
            {
                self.push_system_message(notice);
                ui_changed = true;
            }
            self.current_turn_user_message_id = None;
            self.in_flight_prompt = None;
        }

        if pending_retry_changed || ui_changed || had_file_changes {
            self.bump_revision();
        }
    }

    pub(super) fn apply_runtime_events_with_file_tracking(
        &mut self,
        mut events: Vec<ClientEvent>,
    ) -> RuntimeEventApplyResult {
        // Preprocess ToolDiff events: fill in old_text from the correct baseline.
        // For the tool card diff, old_text should be "what was on disk when the tool started"
        // so the card shows what THIS tool changed.
        // For session-level changes, the reducer's upsert_session_change preserves the
        // first-ever baseline separately.
        let workspace_root = self.ui.workspace.root.clone();
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
                    // Normalize path to workspace-relative with forward slashes
                    let normalized = normalize_path_for_storage(path, &workspace_root);
                    self.file_tracker.add_diff_candidate(id, normalized.clone());
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

                    if old_text.is_none()
                        && self
                            .file_tracker
                            .was_missing_at_start(id, &normalized)
                            .unwrap_or(false)
                    {
                        *old_text = Some(String::new());
                    }
                    if old_text.is_none() && self.git_head_text_for_path(&normalized).is_none() {
                        *old_text = Some(String::new());
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
                    self.file_tracker.add_diff_candidate(id, normalized.clone());
                    *path = normalized;
                }
                _ => {}
            }
        }

        // Process events and track tool lifecycle for file change detection
        let ui_changed = !events.is_empty();
        let mut completed_tool_ids = Vec::new();
        let mut completed_tool_raw_outputs = HashMap::<String, String>::new();
        let mut completed_tool_ids_with_tracker_changes = HashSet::new();
        let mut late_write_hint_tool_ids = Vec::new();
        let mut failed_tool_ids_without_changes = HashSet::new();
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, raw_input, .. } => {
                    self.file_tracker
                        .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
                }
                ClientEvent::ToolUpdated { id, raw_input, .. } => {
                    for path in tool_event_hint_paths(raw_input.as_deref()) {
                        if raw_input_has_write_payload(raw_input.as_deref()) {
                            self.file_tracker.add_diff_candidate(id, path);
                        } else {
                            self.file_tracker.add_candidate(id, path);
                        }
                    }
                }
                ClientEvent::ToolCompleted { id, raw_output, .. } => {
                    completed_tool_ids.push(id.clone());
                    if let Some(raw_output) = raw_output {
                        completed_tool_raw_outputs.insert(id.clone(), raw_output.clone());
                    }
                }
                ClientEvent::ToolFailed { id, .. } => {
                    let changes = self.file_tracker.finish_recording(id);
                    let tracker_changed = self.apply_tracker_changes(id, changes);
                    if tracker_changed {
                        completed_tool_ids_with_tracker_changes.insert(id.clone());
                    }
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
            if let ClientEvent::ToolPermissionRequest { id, .. } = event {
                self.auto_resolve_full_access_permission_if_applicable(id);
            }
            if let ClientEvent::ToolFailed { id, .. } = event
                && failed_tool_ids_without_changes.contains(id)
            {
                had_file_changes |= self.discard_failed_tool_speculative_diffs(id);
            }
            if let ClientEvent::ToolUpdated { id, .. } = event
                && self.completed_tool_has_detectable_write_hint(id)
            {
                late_write_hint_tool_ids.push(id.clone());
            }
            if let ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
            } = event
                && self.apply_verified_fs_write_tool_diff(id, path, old_text.as_deref(), new_text)
            {
                had_file_changes = true;
            }
        }
        self.session.update_session_id(&events);

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        for id in &completed_tool_ids {
            if completed_tool_ids_with_tracker_changes.contains(id) {
                continue;
            }
            if self.apply_completed_tool_landed_edit_payload_with_raw_output(
                id,
                completed_tool_raw_outputs.get(id).map(String::as_str),
            ) {
                self.file_tracker.discard_recording(id);
                had_file_changes = true;
                continue;
            }
            if self.file_tracker.has_active_candidates(id)
                || self.completed_tool_has_detectable_write_hint(id)
            {
                self.enqueue_pending_tool_write_detection(id);
                continue;
            }
            let changes = self.file_tracker.finish_recording(id);
            let tracker_changed = self.apply_tracker_changes(id, changes);
            if tracker_changed {
                had_file_changes = true;
                continue;
            }
            if self.detect_file_writes_from_tools(std::slice::from_ref(id)) {
                had_file_changes = true;
            } else if self.completed_tool_has_detectable_write_hint(id) {
                self.enqueue_pending_tool_write_detection(id);
            } else {
                self.file_tracker.discard_recording(id);
            }
        }
        if !late_write_hint_tool_ids.is_empty() {
            late_write_hint_tool_ids.sort();
            late_write_hint_tool_ids.dedup();
            let late_write_changed = self.detect_file_writes_from_tools(&late_write_hint_tool_ids);
            if late_write_changed {
                had_file_changes = true;
            } else {
                for id in &late_write_hint_tool_ids {
                    self.enqueue_pending_tool_write_detection(id);
                }
            }
        }

        // Persist session_changes to SQLite after all file-change sources have run.
        if had_file_changes {
            self.persist_file_changes();
            self.persist_review_file_changes();
        }

        RuntimeEventApplyResult {
            ui_changed,
            had_file_changes,
            turn_stop_reason,
        }
    }

    pub(super) fn apply_idle_runtime_events_with_file_tracking(
        &mut self,
        events: Vec<ClientEvent>,
    ) -> RuntimeEventApplyResult {
        let recovered_turn_user_id = if self.current_turn_user_message_id.is_none()
            && events_may_affect_review_changes(&events)
        {
            self.latest_completed_turn_user_message_id()
        } else {
            None
        };
        let previous_turn_user_id = self.current_turn_user_message_id;
        if let Some(user_id) = recovered_turn_user_id {
            self.current_turn_user_message_id = Some(user_id);
        }

        let mut result = self.apply_runtime_events_with_file_tracking(events);
        if recovered_turn_user_id.is_some() && result.had_file_changes {
            result.ui_changed |= self.persist_current_turn_file_changes();
        }
        self.current_turn_user_message_id = previous_turn_user_id;
        result
    }

    fn enqueue_pending_tool_write_detection(&mut self, call_id: &str) {
        let now = self.runtime_now();
        let next_retry_at = now + PENDING_TOOL_WRITE_SETTLE_DELAY;
        let expires_at = now + PENDING_TOOL_WRITE_DETECTION_TTL;
        if let Some(pending) = self
            .pending_tool_write_detections
            .iter_mut()
            .find(|pending| pending.call_id == call_id)
        {
            pending.turn_user_message_id = self.current_turn_user_message_id;
            pending.next_retry_at = next_retry_at;
            pending.expires_at = expires_at;
            return;
        }

        self.pending_tool_write_detections
            .push(PendingToolWriteDetection {
                call_id: call_id.to_string(),
                turn_user_message_id: self.current_turn_user_message_id,
                next_retry_at,
                expires_at,
            });
    }

    pub(super) fn retry_pending_tool_write_detections(&mut self) -> bool {
        if self.pending_tool_write_detections.is_empty() {
            return false;
        }

        let now = self.runtime_now();
        let previous_turn_user_id = self.current_turn_user_message_id;
        let mut changed = false;
        let pending = std::mem::take(&mut self.pending_tool_write_detections);

        for mut detection in pending {
            let expired = now >= detection.expires_at;
            let owner = detection.turn_user_message_id.or(previous_turn_user_id);
            let due = now >= detection.next_retry_at;
            if !expired && !due {
                self.pending_tool_write_detections.push(detection);
                continue;
            }
            if previous_turn_user_id.is_some()
                && detection.turn_user_message_id.is_some()
                && previous_turn_user_id != detection.turn_user_message_id
            {
                if !expired {
                    self.pending_tool_write_detections.push(detection);
                }
                continue;
            }

            self.current_turn_user_message_id = owner;
            let tracker_changes = self.file_tracker.finish_recording(&detection.call_id);
            let tracker_changed = self.apply_tracker_changes(&detection.call_id, tracker_changes);
            if tracker_changed {
                self.persist_file_changes();
                self.persist_review_file_changes();
                changed = true;
                if self.in_flight_prompt.is_none() && owner.is_some() {
                    changed |= self.persist_current_turn_file_changes();
                }
            } else if self.detect_file_writes_from_tools(std::slice::from_ref(&detection.call_id)) {
                self.file_tracker.discard_recording(&detection.call_id);
                self.persist_file_changes();
                self.persist_review_file_changes();
                changed = true;
                if self.in_flight_prompt.is_none() && owner.is_some() {
                    changed |= self.persist_current_turn_file_changes();
                }
            } else if !expired
                && (self.completed_tool_has_detectable_write_hint(&detection.call_id)
                    || self.file_tracker.has_active_candidates(&detection.call_id))
            {
                detection.next_retry_at = now + PENDING_TOOL_RETRY_INTERVAL;
                self.pending_tool_write_detections.push(detection);
            } else {
                self.file_tracker.discard_recording(&detection.call_id);
            }
            self.current_turn_user_message_id = previous_turn_user_id;
        }

        self.current_turn_user_message_id = previous_turn_user_id;
        changed
    }

    fn latest_completed_turn_user_message_id(&self) -> Option<uuid::Uuid> {
        let mut last_user_id = None;
        let mut latest_user_before_assistant = None;

        for item in &self.ui.timeline {
            let TimelineItem::Message(message_id) = item else {
                continue;
            };
            let Some(message) = self
                .ui
                .messages
                .iter()
                .find(|message| message.id == *message_id)
            else {
                continue;
            };
            match message.role {
                MessageRole::User => last_user_id = Some(message.id),
                MessageRole::Assistant => {
                    if last_user_id.is_some() {
                        latest_user_before_assistant = last_user_id;
                    }
                }
                MessageRole::System => {}
            }
        }

        latest_user_before_assistant.or(last_user_id)
    }

    fn poll_background_runtimes(&mut self) {
        let runtime_ids = self
            .runtime_registry
            .entries
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        for runtime_id in runtime_ids {
            let Some(mut runtime) = self.runtime_registry.remove(&runtime_id) else {
                continue;
            };
            let was_in_flight = runtime.is_in_flight();
            let previous_attention = runtime.attention_state.clone();
            let previous_idle_since = runtime.idle_since;
            let last_viewed = runtime.last_viewed;

            self.swap_visible_state_with_runtime(&mut runtime);
            self.poll_current_runtime_progress();
            let still_in_flight = self.in_flight_prompt.is_some();
            let needs_attention = self.runtime_needs_attention();
            self.swap_visible_state_with_runtime(&mut runtime);

            runtime.last_viewed = last_viewed;
            if still_in_flight {
                runtime.runtime_status = SessionRuntimeStatus::BackgroundRunning;
                runtime.idle_since = None;
            } else {
                runtime.runtime_status = SessionRuntimeStatus::BackgroundIdle;
                runtime.idle_since = previous_idle_since.or_else(|| Some(self.runtime_now()));
            }

            runtime.attention_state = if needs_attention {
                SessionAttentionState::NeedsAttention
            } else if was_in_flight
                && !still_in_flight
                && matches!(previous_attention, SessionAttentionState::None)
            {
                SessionAttentionState::CompletedUnviewed
            } else {
                previous_attention
            };
            self.runtime_registry.insert(runtime);
        }
    }

    fn retire_idle_background_runtimes(&mut self) {
        let now = self.runtime_now();
        let retire_ids = self
            .runtime_registry
            .entries
            .iter()
            .filter_map(|(id, runtime)| {
                let idle_for = runtime
                    .idle_since
                    .and_then(|idle_since| now.checked_duration_since(idle_since))?;
                (!runtime.is_in_flight() && idle_for >= BACKGROUND_RUNTIME_IDLE_GRACE)
                    .then(|| id.clone())
            })
            .collect::<Vec<_>>();

        for runtime_id in retire_ids {
            if let Some(mut runtime) = self.runtime_registry.remove(&runtime_id) {
                let attention = runtime.attention_state.clone();
                runtime.session.shutdown();
                self.runtime_registry
                    .retain_attention_after_retirement(runtime_id, attention);
            }
        }
    }

    pub fn has_in_flight_prompt(&self) -> bool {
        self.in_flight_prompt.is_some()
    }

    fn retryable_user_message_index(&self, message_id: uuid::Uuid) -> anyhow::Result<usize> {
        let message_index = self
            .ui
            .timeline
            .iter()
            .position(|item| matches!(item, TimelineItem::Message(id) if *id == message_id))
            .ok_or_else(|| anyhow::anyhow!("消息不存在，无法重发"))?;

        let Some(message) = self
            .ui
            .messages
            .iter()
            .find(|message| message.id == message_id)
        else {
            anyhow::bail!("消息不存在，无法重发");
        };
        if message.role != MessageRole::User {
            anyhow::bail!("只能重新编辑用户消息");
        }

        for item in self.ui.timeline.iter().skip(message_index + 1) {
            match item {
                TimelineItem::Message(id) => {
                    let role = self
                        .ui
                        .messages
                        .iter()
                        .find(|message| message.id == *id)
                        .map(|message| &message.role);
                    if !matches!(role, Some(MessageRole::System)) {
                        anyhow::bail!("智能体已经开始回复，不能重写这条消息");
                    }
                }
                TimelineItem::Tool(_) => {
                    anyhow::bail!("智能体已经开始执行工具，不能重写这条消息");
                }
                TimelineItem::Thinking => {}
            }
        }

        Ok(message_index)
    }

    fn remove_retry_artifacts_after_message(&mut self, message_index: usize) {
        let message_ids_to_delete = self
            .ui
            .timeline
            .iter()
            .skip(message_index + 1)
            .filter_map(|item| match item {
                TimelineItem::Message(id) => Some(*id),
                TimelineItem::Tool(_) | TimelineItem::Thinking => None,
            })
            .collect::<Vec<_>>();
        if message_ids_to_delete.is_empty() && self.ui.timeline.len() == message_index + 1 {
            return;
        }

        self.ui.timeline.truncate(message_index + 1);
        self.ui.thinking_status = None;

        if message_ids_to_delete.is_empty() {
            return;
        }
        self.ui
            .messages
            .retain(|message| !message_ids_to_delete.contains(&message.id));
        let ids = message_ids_to_delete
            .iter()
            .map(uuid::Uuid::to_string)
            .collect::<Vec<_>>();
        let _ = self.store.delete_messages(&ids);
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

    pub fn stop_tool(&mut self, tool_call_id: &str) -> Result<(), String> {
        let tool_call_id = tool_call_id.trim();
        if tool_call_id.is_empty() {
            return Err("tool_call_id is required".into());
        }

        let events = self
            .session
            .stop_tool(tool_call_id)
            .map_err(|error| error.to_string())?;
        let result = self.apply_idle_runtime_events_with_file_tracking(events);
        if result.ui_changed || result.had_file_changes {
            self.bump_revision();
        }
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
            tool.can_stop = false;
            tool.stop_kind = None;
            tool.stop_status = None;
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
