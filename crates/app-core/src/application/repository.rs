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
        GitService::stage_status_paths(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
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

/// Generate a commit-message draft by spinning up a throwaway sub-agent
    /// session with the current model settings. The agent is given read-only
    /// permission, so only inspection commands run — it inspects the staged
    /// changes itself and returns just the message. The temporary session is
    /// used once and shut down; it never touches the visible conversation.
    /// `progress` receives human-readable status updates as the agent works.
    /// Blocking — call off the UI thread.
    pub fn generate_commit_message(
        &self,
        progress: &dyn Fn(&str),
    ) -> Result<String, String> {
        if self.is_remote_workspace() {
            return Err("远程工作区暂不支持 AI 生成提交信息".to_string());
        }

        let model = self.ui.session.model.clone();
        let config = SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model,
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(
                &self.agent_command,
                &self.app_paths,
            ),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
            remote_ssh: None,
            mcp_servers: Vec::new(),
        };

        let prompt = format!(
            "在当前 Git 仓库里查看已暂存的变更，然后只输出一条简洁的 commit message。\n\
             建议先用只读命令建立全局视图，再按需深入细节，例如：\n\
             - `git status --short`\n\
             - `git diff --staged --stat`\n\
             - `git diff --staged --name-status`\n\
             - `git diff --staged`（内容过长或被截断时，再对关键文件用 `git diff --staged -- <path>`）\n\
             - 必要时用 `git log -5 --oneline` 参考近期提交风格\n\
             要求：只输出 commit message 本身，不要任何解释、前缀、引号、代码块或 markdown；\n\
             使用约定式提交格式（如 feat/fix/chore/refactor: 描述），单行，不超过 72 个字符；\n\
             只允许只读查看命令，不要 stage/unstage/commit/push，也不要修改任何文件。"
        );

        progress("正在启动 AI 会话…");
        let mut handle =
            SessionHandle::start(config).map_err(|e| format!("无法启动 AI 会话：{e}"))?;
        // Read-only permission: inspection commands such as `git diff` /
        // `git status` are auto-approved; mutating commands stay blocked.
        let _ = handle.set_permission_mode("plan");

        progress("正在查看已暂存的变更…");
        let task = handle.send_prompt_async(prompt);
        let collected = match task {
            Ok(mut task) => {
                let mut text = String::new();
                let mut run_error: Option<String> = None;
                while !task.is_finished() {
                    match task.wait_for_events(&mut handle) {
                        Ok(events) => {
                            for event in &events {
                                match event {
                                    ClientEvent::MessageChunk {
                                        role: workspace_model::MessageRole::Assistant,
                                        content,
                                    } => text.push_str(content),
                                    ClientEvent::ToolStarted { name, summary, .. } => {
                                        let label = if summary.is_empty() {
                                            name.clone()
                                        } else {
                                            summary.clone()
                                        };
                                        progress(&format!("正在执行：{label}"));
                                    }
                                    ClientEvent::Interrupted { reason } => {
                                        run_error = Some(reason.clone());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => {
                            run_error = Some(e.to_string());
                            break;
                        }
                    }
                }
                for event in task.into_events() {
                    if let ClientEvent::MessageChunk {
                        role: workspace_model::MessageRole::Assistant,
                        content,
                    } = &event
                    {
                        text.push_str(content);
                    }
                }
                match run_error {
                    Some(e) => Err(format!("AI 生成失败：{e}")),
                    None => Ok(text),
                }
            }
            Err(e) => Err(format!("AI 生成失败：{e}")),
        };
        handle.shutdown();

        let raw = collected?;
        progress("正在整理提交信息…");
        let message = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '`')
            .to_string();
        if message.is_empty() {
            return Err("AI 没有返回可用的提交信息".to_string());
        }
        Ok(message)
    }
}

/// Expand the requested paths to the files currently listed in the
/// repository status. A directory keeps only the status-listed files under
/// it; a file passes through only when it is itself listed. This keeps the
/// remote stage operation aligned with what the panel displays.
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
