use super::*;

fn should_prefer_exact_tool_review_diff(
    exact: &CanonicalTextDiff,
    fallback: &CanonicalTextDiff,
) -> bool {
    if exact.quality != DiffQuality::Exact {
        return false;
    }

    let exact_total = exact.added_lines + exact.removed_lines;
    if exact_total == 0 {
        return false;
    }

    if fallback.quality != DiffQuality::Exact {
        return true;
    }

    let fallback_total = fallback.added_lines + fallback.removed_lines;
    let noisy_threshold = exact_total.saturating_mul(3).max(exact_total + 20);
    fallback_total >= noisy_threshold
}

impl Application {
    pub(in crate::application) fn persist_review_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        let _ = self
            .store
            .replace_review_file_changes(&session_id, &self.ui.review_changes);
    }

    pub(in crate::application) fn begin_review_changes_if_needed(&mut self) {
        if self.review_changes_started {
            return;
        }
        self.review_changes_started = true;
        self.ui.review_changes.clear();
        self.persist_review_file_changes();
        self.remove_current_agent_turn_change_set();
    }

    pub(in crate::application) fn upsert_review_file_change(
        &mut self,
        path: &str,
        change_type: FileChangeType,
        old_text: Option<String>,
        new_text: String,
    ) {
        self.begin_review_changes_if_needed();
        let normalized_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let normalized_old_text = old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let normalized_new_text = normalize_diff_text_for_session_change(&new_text);
        let existing_index = self
            .ui
            .review_changes
            .iter()
            .position(|change| normalize_tracked_path(&change.path) == normalized_path);
        let base_text = existing_index
            .and_then(|index| self.ui.review_changes[index].old_text.clone())
            .or(normalized_old_text);

        if !is_trustworthy_review_change_text(
            &change_type,
            base_text.as_deref(),
            &normalized_new_text,
        ) {
            return;
        }

        if base_text.as_deref().unwrap_or_default() == normalized_new_text {
            if let Some(index) = existing_index {
                self.ui.review_changes.remove(index);
            }
            self.persist_review_file_changes();
            self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
            return;
        }

        let canonical = canonical_text_diff(
            &change_type,
            base_text.as_deref(),
            Some(&normalized_new_text),
            None,
        );
        let added = canonical.added_lines;
        let removed = canonical.removed_lines;
        if added == 0 && removed == 0 {
            if let Some(index) = existing_index {
                self.ui.review_changes.remove(index);
            }
            self.persist_review_file_changes();
            self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
            return;
        }

        let timestamp = current_timestamp();
        if let Some(index) = existing_index {
            let existing = &mut self.ui.review_changes[index];
            existing.new_text = canonical.new_text.unwrap_or(normalized_new_text);
            existing.change_type = change_type;
            existing.added_lines = added;
            existing.removed_lines = removed;
            existing.timestamp = timestamp;
        } else {
            self.ui.review_changes.push(SessionFileChange {
                path: normalized_path,
                change_type,
                old_text: base_text,
                new_text: canonical.new_text.unwrap_or(normalized_new_text),
                added_lines: added,
                removed_lines: removed,
                timestamp,
            });
        }
        self.persist_review_file_changes();
        self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
    }

    pub(in crate::application) fn upsert_review_file_change_for_tool(
        &mut self,
        path: &str,
        change_type: FileChangeType,
        exact_edit: Option<&ExactEditText>,
        fallback_old_text: Option<String>,
        fallback_new_text: String,
    ) {
        let fallback_new_text = normalize_diff_text_for_session_change(&fallback_new_text);
        let fallback_old_text = fallback_old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let fallback_canonical = canonical_text_diff(
            &change_type,
            fallback_old_text.as_deref(),
            Some(&fallback_new_text),
            None,
        );

        if let Some(exact_edit) = exact_edit {
            let exact_canonical = canonical_text_diff(
                &change_type,
                Some(&exact_edit.old_text),
                Some(&exact_edit.new_text),
                None,
            );
            if should_prefer_exact_tool_review_diff(&exact_canonical, &fallback_canonical) {
                self.upsert_review_file_change(
                    path,
                    change_type,
                    Some(exact_edit.old_text.clone()),
                    exact_edit.new_text.clone(),
                );
                return;
            }
        }

        if matches!(fallback_canonical.quality, DiffQuality::Exact) {
            self.upsert_review_file_change(path, change_type, fallback_old_text, fallback_new_text);
        } else if let Some(exact_edit) = exact_edit {
            self.upsert_review_file_change(
                path,
                change_type,
                Some(exact_edit.old_text.clone()),
                exact_edit.new_text.clone(),
            );
        }
    }

    pub(in crate::application) fn tool_diff_baseline_text(
        &self,
        call_id: &str,
        normalized_path: &str,
        new_text: &str,
        batch_file_versions: &HashMap<String, String>,
    ) -> Option<String> {
        let normalized_new_text = normalize_diff_text_for_session_change(new_text);
        let candidate = |text: Option<String>| -> Option<String> {
            let text = normalize_diff_text_for_session_change(text?.as_str());
            (text != normalized_new_text).then_some(text)
        };

        candidate(batch_file_versions.get(normalized_path).cloned())
            .or_else(|| {
                candidate(
                    self.file_tracker
                        .get_baseline_text(call_id, normalized_path)
                        .map(str::to_string),
                )
            })
            .or_else(|| candidate(self.review_new_text_for_path(normalized_path)))
            .or_else(|| candidate(self.session_new_text_for_path(normalized_path)))
            .or_else(|| candidate(self.git_head_text_for_path(normalized_path)))
    }

    pub(in crate::application) fn tool_diff_change_type(
        &self,
        call_id: &str,
        normalized_path: &str,
        old_text: Option<&str>,
    ) -> FileChangeType {
        if old_text.is_some() {
            return FileChangeType::Modified;
        }
        if call_id.starts_with("fs_write:")
            || self
                .file_tracker
                .was_missing_at_start(call_id, normalized_path)
                .unwrap_or(false)
        {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        }
    }

    pub(in crate::application) fn review_new_text_for_path(
        &self,
        normalized_path: &str,
    ) -> Option<String> {
        self.ui
            .review_changes
            .iter()
            .find(|change| normalize_tracked_path(&change.path) == normalized_path)
            .map(|change| change.new_text.clone())
    }

    pub(in crate::application) fn session_new_text_for_path(
        &self,
        normalized_path: &str,
    ) -> Option<String> {
        self.ui
            .session_changes
            .iter()
            .find(|change| normalize_tracked_path(&change.path) == normalized_path)
            .map(|change| change.new_text.clone())
    }

    pub(in crate::application) fn git_head_text_for_path(
        &self,
        normalized_path: &str,
    ) -> Option<String> {
        GitService::head_text(&self.ui.workspace.root, normalized_path)
            .ok()
            .flatten()
            .map(|text| normalize_diff_text_for_session_change(&text))
    }
}
