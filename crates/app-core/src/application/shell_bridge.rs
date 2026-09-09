use super::{Application, normalize_path_for_storage, normalize_tracked_path};
use crate::remote_workspace::RemoteWorkspaceClient;
use git_service::GitService;
use std::path::PathBuf;
use workspace_model::{
    ChangedFile, ChatMessage, EditorFileSnapshot, EditorFileVersion, FileChangeType, FileEntry,
    SearchResult, SessionFileChange, TimelineItem, ToolInvocation,
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

    /// Existence probe for chat file links. Accepts workspace-relative or
    /// absolute-in-workspace paths; returns false for missing/outside/dir.
    pub fn workspace_paths_exist(&self, paths: &[String]) -> Result<Vec<bool>, String> {
        if self.is_remote_workspace() {
            let remote_root = match &self.ui.workspace.location {
                workspace_model::WorkspaceLocation::RemoteLinux(remote) => {
                    PathBuf::from(&remote.remote_path)
                }
                _ => self.ui.workspace.root.clone(),
            };
            let mut relative = Vec::with_capacity(paths.len());
            for path in paths {
                let rel = normalize_path_for_storage(path, &remote_root);
                // Absolute foreign paths stay absolute after normalization and
                // must not be probed remotely (sanitize would reject them).
                if rel.is_empty()
                    || rel.starts_with('/')
                    || (rel.len() > 2 && rel.as_bytes()[1] == b':')
                {
                    relative.push(String::new());
                } else {
                    relative.push(rel);
                }
            }
            // Empty placeholders stay false without a remote round-trip.
            if relative.iter().all(|path| path.is_empty()) {
                return Ok(paths.iter().map(|_| false).collect());
            }
            let client = self.remote_workspace_client("remote path exists")?;
            let mut results = Vec::with_capacity(paths.len());
            let mut probe_paths = Vec::new();
            let mut probe_indexes = Vec::new();
            for (index, path) in relative.iter().enumerate() {
                if path.is_empty() {
                    results.push(false);
                } else {
                    results.push(false); // placeholder
                    probe_paths.push(path.clone());
                    probe_indexes.push(index);
                }
            }
            if !probe_paths.is_empty() {
                let probed = client
                    .paths_exist(&probe_paths)
                    .map_err(|error| format!("failed to probe remote paths: {error}"))?;
                for (slot, exists) in probe_indexes.into_iter().zip(probed) {
                    results[slot] = exists;
                }
            }
            return Ok(results);
        }

        self.ensure_local_workspace_for("local filesystem commands")?;
        Ok(paths
            .iter()
            .map(|path| {
                crate::workspace_files::resolve_existing_path(&self.ui.workspace.root, path)
                    .map(|target| target.is_file())
                    .unwrap_or(false)
            })
            .collect())
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

    pub fn reject_review_file_change(&mut self, path: &str) -> Result<(), String> {
        self.ensure_local_workspace_for("local change review")?;
        let rel_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let normalized_rel = normalize_tracked_path(&rel_path);
        let section = self
            .ui
            .repository
            .changed_files
            .iter()
            .find(|file| normalize_tracked_path(&file.path.display().to_string()) == normalized_rel)
            .map(|file| file.section.clone())
            .ok_or_else(|| "没有可撤销的工作区改动".to_string())?;
        let record = GitService::file_diff(&self.ui.workspace.root, &rel_path, section)
            .map_err(|error| format!("无法加载差异: {error}"))?
            .ok_or_else(|| "没有可撤销的工作区改动".to_string())?;

        if matches!(record.change_type, FileChangeType::Created) {
            let abs_path =
                crate::editor_files::resolve_workspace_path(&self.ui.workspace.root, path, true)?;
            std::fs::remove_file(&abs_path).map_err(|error| format!("无法删除文件: {error}"))?;
        } else {
            let old_text = record
                .old_text
                .ok_or_else(|| "无法确定撤销基线".to_string())?;
            let abs_path =
                crate::editor_files::resolve_workspace_path(&self.ui.workspace.root, path, false)?;
            std::fs::write(&abs_path, old_text)
                .map_err(|error| format!("无法还原文件: {error}"))?;
        }

        self.refresh_repository();
        self.bump_revision();
        Ok(())
    }

    pub fn session_file_diff(&self, path: &str) -> Result<SessionFileChange, String> {
        let normalized = normalize_tracked_path(path);
        let normalized_relative = normalize_path_for_storage(path, &self.ui.workspace.root);
        let matches = |change: &SessionFileChange| {
            let change_normalized = normalize_tracked_path(&change.path);
            let change_relative = normalize_path_for_storage(&change.path, &self.ui.workspace.root);
            change_normalized == normalized
                || change_normalized == normalized_relative
                || change_relative == normalized
                || change_relative == normalized_relative
        };
        self.ui
            .session_changes
            .iter()
            .find(|change| matches(change))
            .or_else(|| self.ui.review_changes.iter().find(|change| matches(change)))
            .cloned()
            .ok_or_else(|| format!("No change found for path: {path}"))
    }

    /// Fetch the full diff (with `old_text`/`new_text`) for one file of a
    /// specific turn. Snapshots strip embedded diff text, so diff views load
    /// it on demand through this lookup.
    pub fn session_turn_file_diff(
        &self,
        message_id: &str,
        path: &str,
    ) -> Result<SessionFileChange, String> {
        let message_uuid = uuid::Uuid::parse_str(message_id)
            .map_err(|e| format!("Invalid message id {message_id}: {e}"))?;
        let turn = self
            .ui
            .turn_changes
            .iter()
            .find(|turn| turn.message_id == message_uuid)
            .ok_or_else(|| format!("No turn changes found for message: {message_id}"))?;
        let normalized = normalize_tracked_path(path);
        let normalized_relative = normalize_path_for_storage(path, &self.ui.workspace.root);
        turn.changes
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

    /// Page older timeline history (entries strictly before `before_seq`).
    /// The frontend prepends these to its local snapshot; the backend does not
    /// mutate the in-memory window, keeping the live append path untouched.
    pub fn load_history_before(
        &self,
        before_seq: i64,
        limit: usize,
    ) -> Result<HistoryPage, String> {
        let session_id = self.ui.session.id.to_string();
        let (messages, tools, timeline, earliest_seq) = self
            .store
            .load_history_before(&session_id, before_seq, limit)
            .map_err(|e| e.to_string())?;
        // When the page came back short of `limit` there is no older history.
        let page_len = timeline.len();
        Ok(HistoryPage {
            messages,
            tools,
            timeline,
            earliest_seq,
            has_more: page_len >= limit,
        })
    }

    /// Fetch a single tool invocation's full stored detail (uncapped raw
    /// fields + diff previews) for an expanded tool card.
    pub fn session_tool_detail(&self, tool_id: &str) -> Result<ToolInvocation, String> {
        let session_id = self.ui.session.id.to_string();
        let mut tool = self
            .store
            .load_tool_detail(&session_id, tool_id)
            .map_err(|e| e.to_string())?;
        // The expanded card renders only a bounded window of the raw fields,
        // so shipping multi-megabyte text buys nothing — and a multi-MB JSON
        // response is parsed on the webview main thread, freezing the UI on
        // every expand of a huge tool log. 1MB keeps every realistically
        // readable/copyable log while bounding the parse.
        cap_detail_field(&mut tool.detail_text);
        if let Some(value) = tool.raw_input.as_mut() {
            cap_detail_field(value);
        }
        if let Some(value) = tool.raw_output.as_mut() {
            cap_detail_field(value);
        }
        Ok(tool)
    }
}

/// Wire cap for one raw text field of a tool detail response.
const TOOL_DETAIL_WIRE_CAP_BYTES: usize = 1024 * 1024;

fn cap_detail_field(value: &mut String) {
    if value.len() <= TOOL_DETAIL_WIRE_CAP_BYTES {
        return;
    }
    let mut cut = TOOL_DETAIL_WIRE_CAP_BYTES;
    while cut < value.len() && !value.is_char_boundary(cut) {
        cut += 1;
    }
    value.truncate(cut);
    value.push_str("\n\n…[详情过长，已截断显示]");
}

/// A page of older timeline history prepended into the visible session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryPage {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolInvocation>,
    pub timeline: Vec<TimelineItem>,
    pub earliest_seq: Option<i64>,
    pub has_more: bool,
}
