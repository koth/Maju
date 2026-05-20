use super::diff_utils::{
    CanonicalTextDiff, ExactEditText, canonical_text_diff, is_trustworthy_review_change_text,
    normalize_diff_text_for_session_change, sanitize_session_file_changes,
};
use super::{Application, current_timestamp, normalize_path_for_storage, normalize_tracked_path};
use git_service::GitService;
use std::collections::HashMap;
use workspace_model::{
    ChangeSection, ChangeSetFilesResponse, ChangeSetSource, ChangeSetStatus, ChangeSetSummary,
    DiffQuality, FileChangeRecord, FileChangeSummary, FileChangeType, GetChangeSetFileDiffRequest,
    ListChangeSetFilesRequest, ListChangeSetsRequest, MessageRole, SessionFileChange, TimelineItem,
    TurnFileChanges,
};

mod agent_turn;
mod git_worktree;
mod manual;
mod query;
mod review;

fn git_section_from_change_set_id(change_set_id: &str) -> Option<ChangeSection> {
    match change_set_id {
        "git-worktree:staged" => Some(ChangeSection::Staged),
        "git-worktree:unstaged" => Some(ChangeSection::Unstaged),
        "git-worktree:untracked" => Some(ChangeSection::Untracked),
        _ => None,
    }
}

impl Application {
    pub(in crate::application) fn load_change_set_records(
        &self,
        change_set_id: &str,
    ) -> Vec<FileChangeRecord> {
        self.store
            .list_change_set_files(change_set_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| {
                self.store
                    .load_change_set_file_diff(change_set_id, &summary.path)
                    .ok()
                    .flatten()
            })
            .collect()
    }

    pub(in crate::application) fn make_change_set_summary(
        &self,
        id: String,
        source: ChangeSetSource,
        message_id: Option<uuid::Uuid>,
        owner_key: Option<String>,
        label: &str,
        status: ChangeSetStatus,
    ) -> ChangeSetSummary {
        ChangeSetSummary {
            id,
            source,
            session_id: Some(self.ui.session.id),
            workspace_root: self.store.workspace_root().to_string(),
            message_id,
            tool_call_id: None,
            owner_key,
            label: label.to_string(),
            added_lines: 0,
            removed_lines: 0,
            file_count: 0,
            updated_at: current_timestamp(),
            status,
        }
    }
}
