use super::*;

impl Application {
    pub fn record_manual_editor_save(
        &mut self,
        path: &str,
        before_text: Option<String>,
        after_text: String,
    ) {
        let change_set_id = self.manual_edit_change_set_id();
        let normalized_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let mut records = self.load_change_set_records(&change_set_id);
        let existing_index = records
            .iter()
            .position(|record| normalize_tracked_path(&record.path) == normalized_path);
        let base_text = existing_index
            .and_then(|index| records[index].old_text.clone())
            .or(before_text)
            .map(|text| normalize_diff_text_for_session_change(&text));
        let normalized_after_text = normalize_diff_text_for_session_change(&after_text);
        let change_type = if base_text.is_none() {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        };
        let canonical = canonical_text_diff(
            &change_type,
            base_text.as_deref(),
            Some(&normalized_after_text),
            None,
        );

        if canonical.quality == DiffQuality::Exact
            && canonical.added_lines == 0
            && canonical.removed_lines == 0
        {
            if let Some(index) = existing_index {
                records.remove(index);
                self.replace_manual_edit_change_set(records);
                self.bump_revision();
            }
            return;
        }

        let record = FileChangeRecord {
            change_set_id: change_set_id.clone(),
            path: normalized_path,
            change_type,
            old_text: canonical.old_text,
            new_text: canonical.new_text,
            added_lines: canonical.added_lines,
            removed_lines: canonical.removed_lines,
            quality: canonical.quality,
            updated_at: current_timestamp(),
        };

        if let Some(index) = existing_index {
            records[index] = record;
        } else {
            records.push(record);
        }
        self.replace_manual_edit_change_set(records);
        self.bump_revision();
    }

    pub(in crate::application) fn manual_edit_change_set_id(&self) -> String {
        format!("manual-edit:{}", self.ui.session.id)
    }

    pub(in crate::application) fn replace_manual_edit_change_set(
        &self,
        mut records: Vec<FileChangeRecord>,
    ) {
        let change_set_id = self.manual_edit_change_set_id();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let summary = self.make_change_set_summary(
            change_set_id,
            ChangeSetSource::ManualEdit,
            None,
            Some(format!("session:{}", self.ui.session.id)),
            "手工修改",
            ChangeSetStatus::Complete,
        );
        let _ = self.store.replace_change_set(&summary, &records);
    }
}
