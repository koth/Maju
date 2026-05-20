use super::diff_utils::{
    ExactEditText, canonical_text_diff, edit_input_after_text, edit_input_before_text,
    is_file_write_tool_identity, looks_like_fragment_to_full_file_text,
    looks_like_whole_file_addition_hunks, normalize_diff_text_for_session_change,
    tool_command_write_hint_paths, tool_diff_hunks_for_detected_write,
    tool_hunks_for_tracker_update,
};
use super::{Application, current_timestamp, normalize_path_for_storage, normalize_tracked_path};
use acp_core::diff_to_hunks;
use git_service::GitService;
use std::collections::HashSet;
use std::path::PathBuf;
use workspace_model::{
    DiffHunk, DiffLineKind, DiffQuality, FileChangeType, SessionFileChange, ToolDiffPreview,
    ToolStatus,
};

impl Application {
    /// Detect file writes from completed tool calls by examining tool summaries/titles.
    /// Apply verified file changes from the tracker to session state and tool diff previews.
    pub(super) fn apply_tracker_changes(
        &mut self,
        call_id: &str,
        changes: Vec<crate::file_tracker::VerifiedFileChange>,
    ) -> bool {
        let mut changed = false;
        for change in changes {
            let normalized = normalize_tracked_path(&change.path);
            let existing_index = self
                .ui
                .session_changes
                .iter()
                .position(|c| normalize_tracked_path(&c.path) == normalized);
            let previous_session_new_text =
                existing_index.map(|index| self.ui.session_changes[index].new_text.clone());
            let effective_old_text = existing_index
                .and_then(|index| self.ui.session_changes[index].old_text.clone())
                .or_else(|| change.old_text.clone());
            let target_text = if change.change_type == FileChangeType::Deleted {
                None
            } else {
                Some(change.new_text.as_str())
            };
            let canonical = canonical_text_diff(
                &change.change_type,
                effective_old_text.as_deref(),
                target_text,
                (change.quality != DiffQuality::Exact).then_some(change.quality.clone()),
            );
            let exact_edit = self.exact_edit_text_for_tool(call_id, &normalized);
            let exact_edit_hunks = exact_edit.as_ref().map(|edit| edit.hunks.clone());
            let existing_tool_hunks = self.existing_tool_diff_hunks(call_id, &normalized);
            let tool_hunks = tool_hunks_for_tracker_update(
                change.skipped_diff,
                exact_edit_hunks,
                existing_tool_hunks,
                previous_session_new_text.as_deref(),
                change.old_text.as_deref(),
                &change.new_text,
                &canonical.hunks,
            );

            if canonical.quality == DiffQuality::Exact {
                if canonical.added_lines == 0 && canonical.removed_lines == 0 {
                    if let Some(index) = existing_index {
                        self.ui.session_changes.remove(index);
                        changed = true;
                    }
                    self.upsert_review_file_change_for_tool(
                        &change.path,
                        change.change_type.clone(),
                        exact_edit.as_ref(),
                        change
                            .old_text
                            .clone()
                            .or(previous_session_new_text.clone()),
                        change.new_text.clone(),
                    );
                    self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
                    continue;
                }

                let added = canonical.added_lines;
                let removed = canonical.removed_lines;

                if added == 0 && removed == 0 {
                    continue;
                }

                if let Some(index) = existing_index {
                    let existing = &mut self.ui.session_changes[index];
                    if existing.old_text.is_none() {
                        existing.old_text = canonical.old_text.clone();
                    }
                    existing.new_text = canonical.new_text.clone().unwrap_or_default();
                    existing.change_type = change.change_type.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                } else {
                    self.ui.session_changes.push(SessionFileChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        old_text: canonical.old_text.clone(),
                        new_text: canonical.new_text.clone().unwrap_or_default(),
                        added_lines: added,
                        removed_lines: removed,
                        timestamp: current_timestamp(),
                    });
                }
                self.upsert_review_file_change_for_tool(
                    &change.path,
                    change.change_type.clone(),
                    exact_edit.as_ref(),
                    change
                        .old_text
                        .clone()
                        .or(previous_session_new_text.clone())
                        .or(effective_old_text),
                    change.new_text.clone(),
                );
                changed = true;
            }

            // Attach only this tool's diff preview, not the cumulative session diff.
            self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
        }
        changed
    }

    pub(super) fn exact_edit_text_for_tool(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        let tool = self.ui.tools.iter().find(|tool| tool.call_id == call_id)?;
        let input = tool.raw_input.as_deref()?;
        let json = serde_json::from_str::<serde_json::Value>(input).ok()?;
        let before = edit_input_before_text(&json)?;
        let after = edit_input_after_text(&json)?;
        let input_path = json
            .get("path")
            .or_else(|| json.get("file_path"))
            .or_else(|| json.get("filePath"))
            .and_then(|value| value.as_str())?;
        if normalize_path_for_storage(input_path, &self.ui.workspace.root) != normalized_path
            && normalize_tracked_path(input_path) != normalized_path
        {
            return None;
        }
        if looks_like_fragment_to_full_file_text(before, after) {
            return None;
        }

        let before = normalize_diff_text_for_session_change(before);
        let after = normalize_diff_text_for_session_change(after);
        if before == after {
            return None;
        }

        if let Some(exact_edit) =
            self.expand_exact_edit_from_current_file(normalized_path, &before, &after)
        {
            return Some(exact_edit);
        }

        let hunks = diff_to_hunks(Some(&before), &after);
        (!hunks.is_empty()).then_some(ExactEditText {
            old_text: before,
            new_text: after,
            hunks,
        })
    }

    pub(super) fn expand_exact_edit_from_current_file(
        &self,
        normalized_path: &str,
        before: &str,
        after: &str,
    ) -> Option<ExactEditText> {
        if after.is_empty() {
            return None;
        }

        let full_new = std::fs::read_to_string(self.ui.workspace.root.join(normalized_path))
            .ok()
            .map(|text| normalize_diff_text_for_session_change(&text))?;
        if full_new == after {
            let hunks = diff_to_hunks(Some(before), after);
            return (!hunks.is_empty()).then_some(ExactEditText {
                old_text: before.to_string(),
                new_text: after.to_string(),
                hunks,
            });
        }

        let occurrence_count = full_new.match_indices(after).take(2).count();
        if occurrence_count != 1 {
            return None;
        }

        let full_old = full_new.replacen(after, before, 1);
        if full_old == full_new {
            return None;
        }

        let hunks = diff_to_hunks(Some(&full_old), &full_new);
        (!hunks.is_empty()).then_some(ExactEditText {
            old_text: full_old,
            new_text: full_new,
            hunks,
        })
    }

    pub(super) fn existing_tool_diff_hunks(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<Vec<DiffHunk>> {
        self.ui
            .tools
            .iter()
            .find(|tool| tool.call_id == call_id)?
            .diff_previews
            .iter()
            .find(|preview| {
                normalize_tracked_path(&preview.path.display().to_string()) == normalized_path
            })
            .map(|preview| preview.hunks.clone())
            .filter(|hunks| !hunks.is_empty() && !looks_like_whole_file_addition_hunks(hunks))
    }

    pub(super) fn attach_tool_diff_preview(
        &mut self,
        call_id: &str,
        path: &str,
        normalized_path: &str,
        hunks: Vec<DiffHunk>,
    ) {
        if hunks.is_empty() {
            return;
        }
        self.mark_tool_call_dirty(call_id);
        let Some(tool) = self.ui.tools.iter_mut().find(|t| t.call_id == call_id) else {
            return;
        };

        let path_buf = PathBuf::from(path);
        if !tool
            .diff_paths
            .iter()
            .any(|p| normalize_tracked_path(&p.display().to_string()) == normalized_path)
        {
            tool.diff_paths.push(path_buf.clone());
        }
        if let Some(preview) = tool
            .diff_previews
            .iter_mut()
            .find(|p| normalize_tracked_path(&p.path.display().to_string()) == normalized_path)
        {
            preview.path = path_buf;
            preview.hunks = hunks;
        } else {
            tool.diff_previews.push(ToolDiffPreview {
                path: path_buf,
                hunks,
            });
        }
    }

    /// CodeBuddy agent uses terminal commands (cat > file << 'EOF') to write files,
    /// so we can't rely on ToolDiff events from the ACP protocol. Instead, we check
    /// completed tools for edit-related patterns and read the current file content.
    pub(super) fn detect_file_writes_from_tools(&mut self, completed_tool_ids: &[String]) -> bool {
        // Normalize path for comparison: forward slashes, lowercase drive letter on Windows
        fn normalize_path(p: &str) -> String {
            normalize_tracked_path(p)
        }

        let workspace_root = self.ui.workspace.root.clone();
        let mut changed = false;

        // Collect normalized paths already tracked in session_changes
        let tracked_paths: HashSet<String> = self
            .ui
            .session_changes
            .iter()
            .map(|c| normalize_path(&c.path))
            .collect();

        let completed_tool_ids: HashSet<&str> =
            completed_tool_ids.iter().map(String::as_str).collect();

        // Look only at tools that completed in this poll batch. Scanning all historical
        // tools every 220ms makes long CodeBuddy sessions burn the desktop process.
        let mut write_paths: Vec<(String, String)> = Vec::new();
        for tool in self.ui.tools.iter().filter(|t| {
            t.status == ToolStatus::Succeeded && completed_tool_ids.contains(t.call_id.as_str())
        }) {
            let mut add_path = |path: String| {
                if !tracked_paths.contains(&normalize_path(&path)) {
                    write_paths.push((tool.call_id.clone(), path));
                }
            };

            // Check diff_paths first (set by ToolDiff events from ACP WriteTextFileRequest).
            // Only treat it as a real write when the preview has actual changed lines;
            // a path-only or context-only preview is often just a no-op edit notification.
            if let Some(preview) = tool.diff_previews.iter().find(|preview| {
                preview.hunks.iter().any(|hunk| {
                    hunk.lines.iter().any(|line| {
                        matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed)
                    })
                })
            }) {
                let path = preview.path.display().to_string();
                add_path(path);
            }

            // Check summary for "Editing <path>" pattern
            if tool.summary.starts_with("Editing ") {
                let path = tool.summary.trim_start_matches("Editing ").to_string();
                add_path(path);
            }

            // Check raw_input JSON for file_path/filePath/path fields in edit/write tools
            if is_file_write_tool_identity(&tool.kind, &tool.name) {
                if let Some(ref input) = tool.raw_input {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
                        let file_path = json
                            .get("file_path")
                            .or_else(|| json.get("filePath"))
                            .or_else(|| json.get("path"))
                            .and_then(|v| v.as_str());
                        if let Some(path) = file_path {
                            add_path(path.to_string());
                        }
                    }
                }
            }

            // Shell tools can write files via command text (Set-Content, Out-File,
            // redirects, etc.) without emitting ACP ToolDiff events.
            for path in tool_command_write_hint_paths(tool.raw_input.as_deref()) {
                add_path(path);
            }
        }
        write_paths.sort();
        write_paths.dedup();

        // For each detected file write, read current content and update only
        // already-tracked changes. Creating a new change without a baseline makes
        // the UI render the whole file as added, so the file tracker must be the
        // source of new session_changes.
        for (call_id, path) in write_paths {
            let normalized = normalize_path_for_storage(&path, &workspace_root);
            let abs_path = workspace_root.join(&normalized);
            if let Ok(new_text) = std::fs::read_to_string(&abs_path)
                && let Some(index) = self
                    .ui
                    .session_changes
                    .iter()
                    .position(|c| normalize_path(&c.path) == normalized)
            {
                let exact_edit = self.exact_edit_text_for_tool(&call_id, &normalized);
                let old_text = self.ui.session_changes[index].old_text.clone();
                let previous_session_new_text = self.ui.session_changes[index].new_text.clone();
                let tool_hunks = exact_edit
                    .as_ref()
                    .map(|edit| edit.hunks.clone())
                    .unwrap_or_else(|| {
                        tool_diff_hunks_for_detected_write(
                            Some(&previous_session_new_text),
                            None,
                            &new_text,
                        )
                    });
                if old_text.as_deref().unwrap_or_default() == new_text {
                    self.ui.session_changes.remove(index);
                    self.upsert_review_file_change_for_tool(
                        &normalized,
                        FileChangeType::Modified,
                        exact_edit.as_ref(),
                        Some(previous_session_new_text.clone()),
                        new_text.clone(),
                    );
                    changed = true;
                    self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
                    continue;
                }

                let session_hunks = diff_to_hunks(old_text.as_deref(), &new_text);
                let added = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Added)
                    .count();
                let removed = session_hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .filter(|l| l.kind == DiffLineKind::Removed)
                    .count();
                if added == 0 && removed == 0 {
                    continue;
                }

                let review_change_type = {
                    let existing = &mut self.ui.session_changes[index];
                    existing.new_text = new_text.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                    existing.change_type.clone()
                };
                self.upsert_review_file_change_for_tool(
                    &normalized,
                    review_change_type,
                    exact_edit.as_ref(),
                    Some(previous_session_new_text),
                    new_text,
                );
                changed = true;

                self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
            } else if let Some(change) = self.git_verified_file_change(&normalized) {
                changed |= self.apply_tracker_changes(&call_id, vec![change]);
            }
        }

        changed
    }

    pub(super) fn git_verified_file_change(
        &self,
        normalized_path: &str,
    ) -> Option<crate::file_tracker::VerifiedFileChange> {
        let record = GitService::file_diff_auto(&self.ui.workspace.root, normalized_path)
            .ok()
            .flatten()?;
        let new_text = if record.change_type == FileChangeType::Deleted {
            String::new()
        } else {
            record.new_text.clone()?
        };
        Some(crate::file_tracker::VerifiedFileChange {
            path: record.path,
            change_type: record.change_type,
            old_text: record.old_text,
            new_text,
            skipped_diff: record.quality != DiffQuality::Exact,
            quality: record.quality,
        })
    }
}
