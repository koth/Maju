use super::*;

struct ChangeSectionEntry {
    section: ChangeSection,
    change_set_id: &'static str,
    label: &'static str,
}

impl ChangeSectionEntry {
    fn staged() -> Self {
        Self {
            section: ChangeSection::Staged,
            change_set_id: "git-worktree:staged",
            label: "Git 已暂存",
        }
    }

    fn unstaged() -> Self {
        Self {
            section: ChangeSection::Unstaged,
            change_set_id: "git-worktree:unstaged",
            label: "Git 未暂存",
        }
    }

    fn untracked() -> Self {
        Self {
            section: ChangeSection::Untracked,
            change_set_id: "git-worktree:untracked",
            label: "Git 未跟踪",
        }
    }
}

fn file_summary_from_record(record: &FileChangeRecord) -> FileChangeSummary {
    FileChangeSummary {
        change_set_id: record.change_set_id.clone(),
        path: normalize_tracked_path(&record.path),
        change_type: record.change_type.clone(),
        added_lines: record.added_lines,
        removed_lines: record.removed_lines,
        quality: record.quality.clone(),
        updated_at: record.updated_at.clone(),
    }
}

impl Application {
    pub(in crate::application) fn git_worktree_change_set_summaries(
        &self,
    ) -> Vec<ChangeSetSummary> {
        [
            ChangeSectionEntry::staged(),
            ChangeSectionEntry::unstaged(),
            ChangeSectionEntry::untracked(),
        ]
        .into_iter()
        .filter_map(|entry| {
            let files = self
                .ui
                .repository
                .changed_files
                .iter()
                .filter(|file| file.section == entry.section)
                .collect::<Vec<_>>();
            if files.is_empty() {
                return None;
            }

            Some(ChangeSetSummary {
                id: entry.change_set_id.to_string(),
                source: ChangeSetSource::GitWorktree,
                session_id: None,
                workspace_root: self.store.workspace_root().to_string(),
                message_id: None,
                tool_call_id: None,
                owner_key: Some("workspace".into()),
                label: entry.label.to_string(),
                added_lines: files.iter().map(|file| file.stats.added).sum(),
                removed_lines: files.iter().map(|file| file.stats.removed).sum(),
                file_count: files.len(),
                updated_at: current_timestamp(),
                status: ChangeSetStatus::Live,
            })
        })
        .collect()
    }

    pub(in crate::application) fn git_worktree_file_summaries(
        &self,
        change_set_id: &str,
        section: ChangeSection,
    ) -> Vec<FileChangeSummary> {
        let mut summaries = self
            .ui
            .repository
            .changed_files
            .iter()
            .filter(|file| file.section == section)
            .filter_map(|file| {
                let path = file.path.display().to_string();
                let record = GitService::file_diff(&self.ui.workspace.root, &path, section.clone())
                    .ok()
                    .flatten();
                Some(match record {
                    Some(record) => file_summary_from_record(&record),
                    None => FileChangeSummary {
                        change_set_id: change_set_id.to_string(),
                        path: normalize_tracked_path(&path),
                        change_type: FileChangeType::Modified,
                        added_lines: file.stats.added,
                        removed_lines: file.stats.removed,
                        quality: DiffQuality::MissingBaseline,
                        updated_at: String::new(),
                    },
                })
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|a, b| a.path.cmp(&b.path));
        summaries
    }
}
