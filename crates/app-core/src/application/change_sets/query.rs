use super::*;

impl Application {
    pub fn list_change_sets(&self, request: ListChangeSetsRequest) -> Vec<ChangeSetSummary> {
        let requested_workspace = request
            .workspace_root
            .as_deref()
            .map(normalize_tracked_path)
            .unwrap_or_else(|| {
                normalize_tracked_path(&self.ui.workspace.root.display().to_string())
            });
        if requested_workspace
            != normalize_tracked_path(&self.ui.workspace.root.display().to_string())
        {
            return Vec::new();
        }

        let session_id = request.session_id.unwrap_or(self.ui.session.id).to_string();
        let mut summaries = if request.source == Some(ChangeSetSource::GitWorktree) {
            Vec::new()
        } else {
            self.store
                .list_change_sets_with_legacy(&session_id, request.source.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|summary| summary.source != ChangeSetSource::GitWorktree)
                .collect()
        };

        if request
            .source
            .as_ref()
            .is_none_or(|source| source == &ChangeSetSource::GitWorktree)
        {
            summaries.extend(self.git_worktree_change_set_summaries());
        }

        summaries
    }

    pub fn list_change_set_files(
        &self,
        request: ListChangeSetFilesRequest,
    ) -> ChangeSetFilesResponse {
        let files = if let Some(section) = git_section_from_change_set_id(&request.change_set_id) {
            self.git_worktree_file_summaries(&request.change_set_id, section)
        } else {
            self.store
                .list_change_set_files_with_legacy(&request.change_set_id)
                .unwrap_or_default()
        };

        ChangeSetFilesResponse {
            change_set_id: request.change_set_id,
            files,
        }
    }

    pub fn get_change_set_file_diff(
        &self,
        request: GetChangeSetFileDiffRequest,
    ) -> Option<FileChangeRecord> {
        if let Some(section) = git_section_from_change_set_id(&request.change_set_id) {
            return GitService::file_diff(&self.ui.workspace.root, &request.path, section)
                .ok()
                .flatten();
        }

        self.store
            .load_change_set_file_diff_with_legacy(&request.change_set_id, &request.path)
            .ok()
            .flatten()
    }
}
