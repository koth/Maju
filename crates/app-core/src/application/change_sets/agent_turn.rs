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

    pub(in crate::application) fn persist_agent_conversation_change_set_from_turns(&mut self) {
        let change_set_id = format!("agent-conversation:{}", self.ui.session.id);
        let session_id = self.ui.session.id.to_string();
        let turn_summaries = self
            .store
            .list_change_sets_with_legacy(&session_id, Some(ChangeSetSource::AgentTurn))
            .unwrap_or_default();
        let turn_summaries: Vec<_> = turn_summaries
            .into_iter()
            .filter(|summary| {
                summary.message_id.is_some()
                    && matches!(
                        summary.status,
                        ChangeSetStatus::Complete | ChangeSetStatus::LegacyIncomplete
                    )
            })
            .collect();
        let mut signature: u64 = 17;
        for summary in &turn_summaries {
            for byte in summary.id.bytes() {
                signature = (signature << 5)
                    .wrapping_sub(signature)
                    .wrapping_add(byte as u64);
            }
            signature = signature
                .wrapping_mul(31)
                .wrapping_add(summary.message_id.map_or(0, |id| id.as_u128() as u64));
            signature = signature
                .wrapping_mul(31)
                .wrapping_add(summary.updated_at.parse::<u64>().unwrap_or_default());
        }
        if self.conversation_change_set_signature == signature {
            return;
        }
        let mut turn_summaries = turn_summaries;
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

        let mut cache = std::mem::take(&mut self.conversation_change_set_turn_cache);
        cache.retain(|turn_id, _| {
            turn_summaries
                .iter()
                .any(|summary| summary.id.as_str() == turn_id.as_str())
        });
        let mut aggregate = HashMap::<String, FileChangeRecord>::new();
        for summary in turn_summaries {
            let cached = cache.get(&summary.id).filter(|(cached_updated_at, _)| {
                cached_updated_at.as_str() == summary.updated_at.as_str()
            });
            let turn_records: Vec<FileChangeRecord> = match cached {
                Some((_, records)) => records.clone(),
                None => {
                    let files = self
                        .store
                        .list_change_set_files_with_legacy(&summary.id)
                        .unwrap_or_default();
                    files
                        .iter()
                        .filter_map(|file| {
                            self.store
                                .load_change_set_file_diff_with_legacy(&summary.id, &file.path)
                                .ok()
                                .flatten()
                        })
                        .collect()
                }
            };
            cache.insert(
                summary.id.clone(),
                (summary.updated_at.clone(), turn_records.clone()),
            );
            for record in turn_records {
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
        self.conversation_change_set_signature = signature;
        self.conversation_change_set_turn_cache = cache;
    }

    pub(in crate::application) fn persist_current_turn_file_changes(&mut self) -> bool {
        let current_turn_assistant_ids = self.current_turn_assistant_message_ids();
        let Some(message_id) = current_turn_assistant_ids.last().copied() else {
            return false;
        };
        let stale_message_ids = current_turn_assistant_ids
            .iter()
            .copied()
            .filter(|id| *id != message_id)
            .collect::<Vec<_>>();

        if !self.review_changes_started {
            let session_id = self.ui.session.id.to_string();
            let before = self.ui.turn_changes.len();
            self.ui
                .turn_changes
                .retain(|entry| !current_turn_assistant_ids.contains(&entry.message_id));
            for stale_id in current_turn_assistant_ids {
                let _ = self
                    .store
                    .replace_turn_file_changes(&session_id, &stale_id, &[]);
            }
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
                .retain(|entry| !current_turn_assistant_ids.contains(&entry.message_id));
            for stale_id in current_turn_assistant_ids {
                let _ = self
                    .store
                    .replace_turn_file_changes(&session_id, &stale_id, &[]);
            }
            self.remove_current_agent_turn_change_set();
            self.persist_agent_conversation_change_set_from_turns();
            return self.ui.turn_changes.len() != before;
        }

        let mut changed = false;
        let before_stale = self.ui.turn_changes.len();
        self.ui
            .turn_changes
            .retain(|entry| !stale_message_ids.contains(&entry.message_id));
        if before_stale != self.ui.turn_changes.len() {
            changed = true;
        }
        for stale_id in stale_message_ids {
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &stale_id, &[]);
        }

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

    pub(in crate::application) fn current_turn_assistant_message_ids(&self) -> Vec<uuid::Uuid> {
        let Some(start_id) = self.current_turn_user_message_id else {
            return Vec::new();
        };
        let mut after_start = false;
        let mut assistant_ids = Vec::new();

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
                assistant_ids.push(*message_id);
            }
        }

        assistant_ids
    }

    /// Re-anchor a turn's file changes to the turn's LAST assistant message.
    ///
    /// The per-turn ChangesBar renders under the message the change set is
    /// anchored to, and the timeline's collapse summary anchors at the same
    /// final reply. When the closing assistant message lands after the change
    /// set was persisted (dsh delivers the closing text after the final
    /// write-detection settle window, replays re-order, ...), the change set
    /// stays on an intermediate reply: its ChangesBar then renders ABOVE the
    /// turn's collapse summary, visually detached from its own turn. Moves
    /// every `turn_changes` entry of the turn onto the last assistant and
    /// re-points the persisted AgentTurn change set row at the same anchor.
    pub(in crate::application) fn reanchor_turn_file_changes_to_last_assistant(
        &mut self,
        turn_user_message_id: uuid::Uuid,
    ) -> bool {
        let previous_turn_user_id = self.current_turn_user_message_id;
        self.current_turn_user_message_id = Some(turn_user_message_id);
        let assistant_ids = self.current_turn_assistant_message_ids();
        self.current_turn_user_message_id = previous_turn_user_id;

        let Some(anchor_id) = assistant_ids.last().copied() else {
            return false;
        };
        let session_id = self.ui.session.id.to_string();
        let stale: Vec<(uuid::Uuid, Vec<SessionFileChange>)> = self
            .ui
            .turn_changes
            .iter()
            .filter(|entry| {
                entry.message_id != anchor_id && assistant_ids.contains(&entry.message_id)
            })
            .map(|entry| (entry.message_id, entry.changes.clone()))
            .collect();
        if stale.is_empty() {
            return false;
        }
        let mut changed = false;
        for (stale_id, changes) in stale {
            self.ui
                .turn_changes
                .retain(|entry| entry.message_id != stale_id);
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &stale_id, &[]);
            match self
                .ui
                .turn_changes
                .iter_mut()
                .find(|entry| entry.message_id == anchor_id)
            {
                Some(entry) => {
                    if entry.changes != changes {
                        entry.changes = changes.clone();
                        changed = true;
                    }
                }
                None => {
                    self.ui.turn_changes.push(TurnFileChanges {
                        message_id: anchor_id,
                        changes: changes.clone(),
                    });
                    changed = true;
                }
            }
            let _ = self
                .store
                .replace_turn_file_changes(&session_id, &anchor_id, &changes);
        }
        // Re-point the persisted AgentTurn change set row at the same anchor.
        // `upsert_change_set` only touches the change_sets row, so the already
        // persisted file records survive (unlike
        // `persist_current_agent_turn_change_set`, which rebuilds records from
        // `review_changes` — empty once the turn has ended).
        let change_set_id = format!("agent-turn:{}:{turn_user_message_id}", self.ui.session.id);
        if let Ok(summaries) = self
            .store
            .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
            && let Some(mut summary) =
                summaries.into_iter().find(|summary| summary.id == change_set_id)
        {
            summary.message_id = Some(anchor_id);
            let _ = self.store.upsert_change_set(&summary);
        }
        changed
    }
}
