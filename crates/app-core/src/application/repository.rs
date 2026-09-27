use super::*;
use crate::remote_workspace::RemoteWorkspaceClient;

impl Application {
    pub fn replace_repository_snapshot(&mut self, snapshot: workspace_model::RepositorySnapshot) {
        if snapshot != self.ui.repository {
            self.ui.repository = snapshot;
            self.bump_revision();
        }
    }

    pub fn refresh_repository(&mut self) {
        if self.is_remote_workspace() {
            match self
                .remote_ssh
                .as_ref()
                .map(RemoteWorkspaceClient::new)
                .map(|client| client.git_status())
            {
                Some(Ok(snapshot)) if snapshot != self.ui.repository => {
                    self.ui.repository = snapshot;
                    self.bump_revision();
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None if !self.ui.repository.changed_files.is_empty() => {
                    self.ui.repository.changed_files.clear();
                    self.bump_revision();
                }
                Some(Err(_)) | None => {}
            }
            return;
        }

        match GitService::open(&self.ui.workspace.root) {
            Ok(snapshot) if snapshot != self.ui.repository => {
                self.ui.repository = snapshot;
                self.bump_revision();
            }
            Ok(_) => {}
            Err(_) if !self.ui.repository.changed_files.is_empty() => {
                self.ui.repository.changed_files.clear();
                self.bump_revision();
            }
            Err(_) => {}
        }
    }

    pub fn stage_files(&mut self, paths: &[String]) -> Result<(), String> {
        if self.is_remote_workspace() {
            // Only stage files that currently show up in the repository
            // status — a directory expands to the status-listed files under
            // it so ignored files are never swept in.
            let paths = expand_to_status_listed(&self.ui.repository, paths);
            if paths.is_empty() {
                return Ok(());
            }
            {
                let remote_ssh = self.remote_ssh.as_ref().ok_or_else(|| {
                    "Remote workspace is missing SSH session config for git stage".to_string()
                })?;
                RemoteWorkspaceClient::new(remote_ssh)
                    .git_stage(&paths)
                    .map_err(|error| format!("failed to stage remote files: {error}"))?;
            }
            self.refresh_repository();
            return Ok(());
        }

        self.ensure_local_workspace_for("local git commands")?;
        GitService::stage_status_paths(&self.ui.workspace.root, paths)
            .map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    pub fn unstage_files(&mut self, paths: &[String]) -> Result<(), String> {
        if self.is_remote_workspace() {
            {
                let remote_ssh = self.remote_ssh.as_ref().ok_or_else(|| {
                    "Remote workspace is missing SSH session config for git unstage".to_string()
                })?;
                RemoteWorkspaceClient::new(remote_ssh)
                    .git_unstage(paths)
                    .map_err(|error| format!("failed to unstage remote files: {error}"))?;
            }
            self.refresh_repository();
            return Ok(());
        }

        self.ensure_local_workspace_for("local git commands")?;
        GitService::unstage(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    pub fn commit_files(&mut self, message: &str) -> Result<(), String> {
        if self.is_remote_workspace() {
            {
                let remote_ssh = self.remote_ssh.as_ref().ok_or_else(|| {
                    "Remote workspace is missing SSH session config for git commit".to_string()
                })?;
                RemoteWorkspaceClient::new(remote_ssh)
                    .git_commit(message)
                    .map_err(|error| format!("failed to commit remote files: {error}"))?;
            }
            self.refresh_repository();
            return Ok(());
        }

        self.ensure_local_workspace_for("local git commands")?;
        GitService::commit(&self.ui.workspace.root, message).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    pub fn push_files(&mut self) -> Result<String, String> {
        if self.is_remote_workspace() {
            return Err("远程工作区暂不支持推送".to_string());
        }
        self.ensure_local_workspace_for("local git commands")?;
        let result = GitService::push(&self.ui.workspace.root).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(result)
    }
}

fn expand_to_status_listed(
    repository: &workspace_model::RepositorySnapshot,
    paths: &[String],
) -> Vec<String> {
    let mut expanded: Vec<String> = Vec::new();
    for raw in paths {
        let normalized = raw.replace('\\', "/").trim_end_matches('/').to_string();
        if normalized.is_empty() {
            continue;
        }
        let is_listed = repository
            .changed_files
            .iter()
            .any(|file| file.path.to_string_lossy().replace('\\', "/") == normalized);
        if is_listed {
            expanded.push(normalized.clone());
            continue;
        }
        let prefix = format!("{normalized}/");
        for file in &repository.changed_files {
            let file_path = file.path.to_string_lossy().replace('\\', "/");
            if file_path.starts_with(&prefix) && !expanded.contains(&file_path) {
                expanded.push(file_path);
            }
        }
    }
    expanded
}
