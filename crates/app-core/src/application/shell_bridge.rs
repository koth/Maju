use super::{Application, normalize_path_for_storage, normalize_tracked_path};
use crate::remote_workspace::RemoteWorkspaceClient;
use git_service::GitService;
use std::path::PathBuf;
use workspace_model::{
    ChangedFile, EditorFileSnapshot, EditorFileVersion, FileEntry, SearchResult, SessionFileChange,
};

impl Application {
    fn remote_workspace_client(
        &self,
        operation: &str,
    ) -> Result<RemoteWorkspaceClient<'_>, String> {
        let remote_ssh = self.remote_ssh.as_ref().ok_or_else(|| {
            format!("Remote workspace is missing SSH session config for {operation}")
        })?;
        Ok(RemoteWorkspaceClient::new(remote_ssh))
    }

    pub fn editor_open_file(&self, path: &str) -> Result<EditorFileSnapshot, String> {
        if self.is_remote_workspace() {
            return self
                .remote_workspace_client("remote editor file access")?
                .read_file(path)
                .map_err(|error| format!("failed to load remote file: {error}"));
        }

        self.ensure_local_workspace_for("direct local editor file access")?;
        crate::editor_files::read_file_snapshot(&self.ui.workspace.root, path)
    }

    pub fn editor_save_file(
        &mut self,
        path: &str,
        content: &str,
        base_version: Option<&EditorFileVersion>,
        overwrite: bool,
    ) -> Result<EditorFileSnapshot, String> {
        if self.is_remote_workspace() {
            let before_text = self
                .editor_open_file(path)
                .ok()
                .map(|snapshot| snapshot.content);
            let snapshot = {
                let client = self.remote_workspace_client("remote editor file save")?;
                client
                    .save_file(
                        path,
                        content,
                        base_version.map(|version| version.content_hash.as_str()),
                        base_version.map(|version| version.size),
                        overwrite,
                    )
                    .map_err(|error| format!("failed to save remote file: {error}"))?
            };
            self.record_manual_editor_save(&snapshot.path, before_text, snapshot.content.clone());
            self.refresh_repository();
            return Ok(snapshot);
        }

        self.ensure_local_workspace_for("direct local editor file access")?;
        let before_text = self
            .editor_open_file(path)
            .ok()
            .map(|snapshot| snapshot.content);
        let snapshot = crate::editor_files::save_file_snapshot(
            &self.ui.workspace.root,
            path,
            content,
            base_version,
            overwrite,
        )?;
        self.record_manual_editor_save(&snapshot.path, before_text, snapshot.content.clone());
        self.refresh_repository();
        Ok(snapshot)
    }

    pub fn list_workspace_dir(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        if self.is_remote_workspace() {
            return self
                .remote_workspace_client("remote filesystem list")?
                .list_dir(path)
                .map_err(|error| format!("failed to list remote directory: {error}"));
        }

        self.ensure_local_workspace_for("local filesystem commands")?;
        crate::workspace_files::list_dir(&self.ui.workspace.root, path)
    }

    pub fn search_workspace(&self, query: &str) -> Result<SearchResult, String> {
        if self.is_remote_workspace() {
            return self
                .remote_workspace_client("remote workspace search")?
                .search(query)
                .map_err(|error| format!("failed to search remote workspace: {error}"));
        }

        self.ensure_local_workspace_for("local workspace search")?;
        Err("Local workspace search is implemented by the desktop shell".into())
    }

    pub fn rename_workspace_entry(
        &mut self,
        path: &str,
        new_name: &str,
    ) -> Result<FileEntry, String> {
        if self.is_remote_workspace() {
            let entry = {
                let client = self.remote_workspace_client("remote filesystem rename")?;
                client
                    .rename(path, new_name)
                    .map_err(|error| format!("failed to rename remote entry: {error}"))?
            };
            self.refresh_repository();
            return Ok(entry);
        }

        self.ensure_local_workspace_for("local filesystem commands")?;
        let entry = crate::workspace_files::rename(&self.ui.workspace.root, path, new_name)?;
        self.refresh_repository();
        Ok(entry)
    }

    pub fn delete_workspace_file(&mut self, path: &str) -> Result<(), String> {
        if self.is_remote_workspace() {
            {
                let client = self.remote_workspace_client("remote filesystem delete")?;
                client
                    .delete_file(path)
                    .map_err(|error| format!("failed to delete remote file: {error}"))?;
            }
            self.refresh_repository();
            return Ok(());
        }

        self.ensure_local_workspace_for("local filesystem commands")?;
        crate::workspace_files::delete_file(&self.ui.workspace.root, path)?;
        self.refresh_repository();
        Ok(())
    }

    pub fn resolve_workspace_entry_for_shell(&self, path: &str) -> Result<PathBuf, String> {
        self.ensure_local_workspace_for("local filesystem commands")?;
        crate::workspace_files::resolve_existing_path(&self.ui.workspace.root, path)
    }

    pub fn review_changed_file(&self, path: &str) -> Option<ChangedFile> {
        let normalized = normalize_tracked_path(path);
        let normalized_relative = normalize_path_for_storage(path, &self.ui.workspace.root);
        self.ui
            .repository
            .changed_files
            .iter()
            .find(|file| {
                let display = file.path.display().to_string();
                let file_normalized = normalize_tracked_path(&display);
                let file_relative = normalize_path_for_storage(&display, &self.ui.workspace.root);
                file_normalized == normalized
                    || file_normalized == normalized_relative
                    || file_relative == normalized
                    || file_relative == normalized_relative
            })
            .cloned()
    }

    pub fn review_git_diff_content(&self, path: &str) -> Result<Option<SessionFileChange>, String> {
        if self.is_remote_workspace() {
            let normalized_rel = normalize_tracked_path(path);
            let snapshot_section = self
                .ui
                .repository
                .changed_files
                .iter()
                .find(|file| {
                    normalize_tracked_path(&file.path.display().to_string()) == normalized_rel
                })
                .map(|file| file.section.clone());

            let client = self.remote_workspace_client("remote diff review")?;
            return if let Some(section) = snapshot_section {
                client
                    .git_file_diff(path, section)
                    .map_err(|error| format!("failed to load remote git diff: {error}"))
            } else {
                client
                    .git_file_diff_auto(path)
                    .map_err(|error| format!("failed to load remote git diff: {error}"))
            };
        }

        self.ensure_local_workspace_for("local diff review")?;
        let rel_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let normalized_rel = normalize_tracked_path(&rel_path);
        let snapshot_section = self
            .ui
            .repository
            .changed_files
            .iter()
            .find(|file| normalize_tracked_path(&file.path.display().to_string()) == normalized_rel)
            .map(|file| file.section.clone());

        let record = if let Some(section) = snapshot_section {
            GitService::file_diff(&self.ui.workspace.root, &rel_path, section)
                .map_err(|error| format!("failed to load git diff: {error}"))?
        } else {
            GitService::file_diff_auto(&self.ui.workspace.root, &rel_path)
                .map_err(|error| format!("failed to load git diff: {error}"))?
        };

        Ok(record.map(|record| SessionFileChange {
            path: record.path,
            change_type: record.change_type,
            old_text: record.old_text,
            new_text: record.new_text.unwrap_or_default(),
            added_lines: record.added_lines,
            removed_lines: record.removed_lines,
            timestamp: record.updated_at,
        }))
    }

    pub fn session_file_diff(&self, path: &str) -> Result<SessionFileChange, String> {
        let normalized = normalize_tracked_path(path);
        let normalized_relative = normalize_path_for_storage(path, &self.ui.workspace.root);
        self.ui
            .session_changes
            .iter()
            .find(|change| {
                let change_normalized = normalize_tracked_path(&change.path);
                let change_relative =
                    normalize_path_for_storage(&change.path, &self.ui.workspace.root);
                change_normalized == normalized
                    || change_normalized == normalized_relative
                    || change_relative == normalized
                    || change_relative == normalized_relative
            })
            .cloned()
            .ok_or_else(|| format!("No change found for path: {path}"))
    }
}
