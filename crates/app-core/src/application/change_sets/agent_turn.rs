use super::*;

impl Application {
    /// Persist current session_changes to SQLite.
    pub(in crate::application) fn persist_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        let _ = self
            .store
            .replace_file_changes(&session_id, &self.ui.session_changes);
    }

    pub(in crate::application) fn current_agent_turn_change_set_id(&self) -> Option<String> {
        let user_message_id = self.current_turn_user_message_id?;
        Some(format!(
            "agent-turn:{}:{user_message_id}",
            self.ui.session.id
        ))
    }

    pub(in crate::application) fn file_record_from_session_change(
        change_set_id: &str,
        change: &SessionFileChange,
    ) -> Option<FileChangeRecord> {
        let target_text = if change.change_type == FileChangeType::Deleted {
            None
        } else {
            Some(change.new_text.as_str())
        };
        let canonical = canonical_text_diff(
            &change.change_type,
            change.old_text.as_deref(),
            target_text,
            None,
        );
        if canonical.quality == DiffQuality::Exact
            && canonical.added_lines == 0
            && canonical.removed_lines == 0
        {
            return None;
        }

        Some(FileChangeRecord {
            change_set_id: change_set_id.to_string(),
            path: normalize_tracked_path(&change.path),
            change_type: change.change_type.clone(),
            old_text: canonical.old_text,
            new_text: canonical.new_text,
            added_lines: canonical.added_lines,
            removed_lines: canonical.removed_lines,
            quality: canonical.quality,
            updated_at: change.timestamp.clone(),
        })
    }

    pub(in crate::application) fn persist_current_agent_turn_change_set(
        &self,
        message_id: Option<uuid::Uuid>,
        status: ChangeSetStatus,
    ) {
        let Some(change_set_id) = self.current_agent_turn_change_set_id() else {
            return;
        };
        let owner_key = self
            .current_turn_user_message_id
            .map(|id| format!("user-message:{id}"));
        let summary = self.make_change_set_summary(
            change_set_id.clone(),
            ChangeSetSource::AgentTurn,
            message_id,
            owner_key,
            "本轮对话",
            status,
        );
        let records = self
            .ui
            .review_changes
            .iter()
            .filter_map(|change| Self::file_record_from_session_change(&change_set_id, change))
            .collect::<Vec<_>>();
        let _ = self.store.replace_change_set(&summary, &records);
    }

    pub(in crate::application) fn remove_current_agent_turn_change_set(&self) {
        let Some(change_set_id) = self.current_agent_turn_change_set_id() else {
            return;
        };
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::AgentTurn,
            None,
            self.current_turn_user_message_id
                .map(|id| format!("user-message:{id}")),
            "本轮对话",
            ChangeSetStatus::Pending,
        );
        let _ = self.store.replace_change_set(&summary, &[]);
    }

    pub(in crate::application) fn persist_agent_conversation_change_set_from_turns(&self) {
        let change_set_id = format!("agent-conversation:{}", self.ui.session.id);
        let session_id = self.ui.session.id.to_string();
        let mut turn_summaries = self
            .store
            .list_change_sets_with_legacy(&session_id, Some(ChangeSetSource::AgentTurn))
            .unwrap_or_default();
        turn_summaries.retain(|summary| {
            summary.message_id.is_some()
                && matches!(
                    summary.status,
                    ChangeSetStatus::Complete | ChangeSetStatus::LegacyIncomplete
                )
        });
        let message_order = self
            .ui
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                TimelineItem::Message(message_id) => Some((*message_id, index)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        turn_summaries.sort_by(|a, b| {
            let a_order = a
                .message_id
                .and_then(|message_id| message_order.get(&message_id).copied())
                .unwrap_or(usize::MAX);
            let b_order = b
                .message_id
                .and_then(|message_id| message_order.get(&message_id).copied())
                .unwrap_or(usize::MAX);
            a_order
                .cmp(&b_order)
                .then(a.updated_at.cmp(&b.updated_at))
                .then(a.id.cmp(&b.id))
        });

        let mut aggregate = HashMap::<String, FileChangeRecord>::new();
        for summary in turn_summaries {
            let files = self
                .store
                .list_change_set_files_with_legacy(&summary.id)
                .unwrap_or_default();
            for file in files {
                let Some(record) = self
                    .store
                    .load_change_set_file_diff_with_legacy(&summary.id, &file.path)
                    .ok()
                    .flatten()
                else {
                    continue;
                };
                let path = normalize_tracked_path(&record.path);
                if let Some(existing) = aggregate.get_mut(&path) {
                    existing.new_text = record.new_text.clone();
                    existing.change_type = if existing.old_text.is_none() {
                        FileChangeType::Created
                    } else if record.change_type == FileChangeType::Deleted {
                        FileChangeType::Deleted
                    } else {
                        FileChangeType::Modified
                    };
                    existing.quality = if existing.quality == DiffQuality::Exact
                        && record.quality == DiffQuality::Exact
                    {
                        DiffQuality::Exact
                    } else {
                        DiffQuality::LegacyIncomplete
                    };
                    existing.updated_at = record.updated_at;
                } else {
                    aggregate.insert(
                        path.clone(),
                        FileChangeRecord {
                            change_set_id: change_set_id.clone(),
                            path,
                            ..record
                        },
                    );
                }
            }
        }

        let mut records = aggregate
            .into_values()
            .map(|mut record| {
                let canonical = canonical_text_diff(
                    &record.change_type,
                    record.old_text.as_deref(),
                    record.new_text.as_deref(),
                    Some(record.quality.clone()),
                );
                record.change_set_id = change_set_id.clone();
                record.old_text = canonical.old_text;
                record.new_text = canonical.new_text;
                record.added_lines = canonical.added_lines;
                record.removed_lines = canonical.removed_lines;
                record.quality = canonical.quality;
                record
            })
            .collect::<Vec<_>>();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::AgentConversation,
            None,
            Some(format!("session:{}", self.ui.session.id)),
            "整体对话",
            ChangeSetStatus::Complete,
        );
        let _ = self.store.replace_change_set(&summary, &records);
    }

    pub(in crate::application) fn persist_current_turn_file_changes(&mut self) -> bool {
        let Some(message_id) = self.current_turn_assistant_message_id() else {
            return false;
        };

        if !self.review_changes_started {
            let session_id = self.ui.session.id.to_string();
            let before = self.ui.turn_changes.len();
            self.ui
                .turn_changes
                .retain(|entry| entry.message_id != message_id);
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &message_id, &[]);
            self.remove_current_agent_turn_change_set();
            self.persist_agent_conversation_change_set_from_turns();
            return self.ui.turn_changes.len() != before;
        }

        let mut changes = self.ui.review_changes.clone();
        sanitize_session_file_changes(&mut changes);
        let session_id = self.ui.session.id.to_string();

        if changes.is_empty() {
            let before = self.ui.turn_changes.len();
            self.ui
                .turn_changes
                .retain(|entry| entry.message_id != message_id);
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &message_id, &[]);
            self.remove_current_agent_turn_change_set();
            self.persist_agent_conversation_change_set_from_turns();
            return self.ui.turn_changes.len() != before;
        }

        let mut changed = false;
        if let Some(index) = self
            .ui
            .turn_changes
            .iter()
            .position(|entry| entry.message_id == message_id)
        {
            if self.ui.turn_changes[index].changes != changes {
                self.ui.turn_changes[index].changes = changes.clone();
                changed = true;
            }
        } else {
            self.ui.turn_changes.push(TurnFileChanges {
                message_id,
                changes: changes.clone(),
            });
            changed = true;
        }

        let _ = self
            .store
            .replace_turn_file_changes(&session_id, &message_id, &changes);
        self.persist_current_agent_turn_change_set(Some(message_id), ChangeSetStatus::Complete);
        self.persist_agent_conversation_change_set_from_turns();
        changed
    }

    pub(in crate::application) fn current_turn_assistant_message_id(&self) -> Option<uuid::Uuid> {
        let start_id = self.current_turn_user_message_id?;
        let mut after_start = false;
        let mut assistant_id = None;

        for item in &self.ui.timeline {
            let TimelineItem::Message(message_id) = item else {
                continue;
            };
            if *message_id == start_id {
                after_start = true;
                continue;
            }
            if !after_start {
                continue;
            }
            if self
                .ui
                .messages
                .iter()
                .any(|message| message.id == *message_id && message.role == MessageRole::Assistant)
            {
                assistant_id = Some(*message_id);
            }
        }

        assistant_id
    }
}
