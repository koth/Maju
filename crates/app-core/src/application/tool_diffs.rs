use super::diff_utils::{
    CanonicalTextDiff, ExactEditText, canonical_text_diff, edit_input_after_text,
    edit_input_before_text, edit_input_change_type_for_path, edit_input_content_for_path,
    edit_input_unified_diff_for_path, is_file_write_tool_identity, is_shell_command_tool_label,
    looks_like_fragment_to_full_file_text, looks_like_whole_file_addition_hunks,
    normalize_diff_text_for_session_change, reverse_apply_diff_hunks, reverse_apply_unified_diff,
    tool_command_write_hint_paths, tool_diff_hunks_for_detected_write, tool_event_change_paths,
    tool_event_hint_paths, tool_hunks_for_tracker_update,
};
use super::{
    Application, current_timestamp, is_codebuddy_agent_label, normalize_path_for_storage,
    normalize_tracked_path,
};
use acp_core::diff_to_hunks;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use workspace_model::{
    ChangeSetStatus, DiffHunk, DiffLineKind, DiffQuality, FileChangeType, SessionFileChange,
    ToolDiffPreview, ToolInvocation, ToolStatus,
};

impl Application {
    pub(super) fn completed_tool_has_detectable_write_hint(&self, call_id: &str) -> bool {
        let Some(tool) = self
            .ui
            .tools
            .iter()
            .find(|tool| tool.call_id == call_id && tool.status == ToolStatus::Succeeded)
        else {
            return false;
        };

        // Shell/Python/Node filesystem mutations never emit ACP ToolDiff. Once the
        // command text names a write target, keep retrying tracker verification so
        // the tool card and review panel can still attach a real before/after diff.
        if !tool_command_write_hint_paths(tool.raw_input.as_deref()).is_empty()
            || !tool_command_write_hint_paths(Some(tool.detail_text.as_str())).is_empty()
        {
            return Self::tool_looks_like_shell_command(tool)
                || is_file_write_tool_identity(&tool.kind, &tool.name)
                || self
                    .ui
                    .session
                    .agent_cli
                    .as_deref()
                    .is_some_and(is_codebuddy_agent_label);
        }

        if !is_file_write_tool_identity(&tool.kind, &tool.name) {
            return false;
        }
        !tool_event_hint_paths(tool.raw_input.as_deref()).is_empty()
            || !tool_event_hint_paths(Some(tool.detail_text.as_str())).is_empty()
    }

    fn tool_looks_like_shell_command(tool: &workspace_model::ToolInvocation) -> bool {
        [tool.kind.as_str(), tool.name.as_str()]
            .iter()
            .any(|value| is_shell_command_tool_label(value))
            || tool_raw_input_looks_like_shell_command(tool.raw_input.as_deref())
    }

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
            let tracker_tool_hunks = if change.skipped_diff {
                Vec::new()
            } else if change.change_type == FileChangeType::Created && change.old_text.is_none() {
                diff_to_hunks(None, &change.new_text)
            } else {
                diff_to_hunks(change.old_text.as_deref(), &change.new_text)
            };
            let exact_edit = self.exact_edit_text_for_tool(call_id, &normalized);
            let exact_edit_hunks = exact_edit.as_ref().map(|edit| edit.hunks.clone());
            let existing_tool_hunks = self.existing_tool_diff_hunks(call_id, &normalized);
            let landed_tool_hunks = compatible_landed_tool_hunks(
                existing_tool_hunks.clone(),
                change.old_text.as_deref(),
                change.change_type == FileChangeType::Created,
                &change.new_text,
            );
            let tool_hunks = landed_tool_hunks.clone().unwrap_or_else(|| {
                tool_hunks_for_tracker_update(
                    change.skipped_diff,
                    exact_edit_hunks,
                    existing_tool_hunks,
                    previous_session_new_text.as_deref(),
                    change.old_text.as_deref(),
                    &change.new_text,
                    &tracker_tool_hunks,
                )
            });

            if canonical.quality == DiffQuality::Exact {
                if canonical.added_lines == 0 && canonical.removed_lines == 0 {
                    if let Some(index) = existing_index {
                        self.ui.session_changes.remove(index);
                        changed = true;
                    }
                    if landed_tool_hunks
                        .as_ref()
                        .and_then(|hunks| {
                            self.upsert_review_file_change_from_landed_tool_hunks(
                                &change.path,
                                change.change_type.clone(),
                                change.old_text.as_deref(),
                                &change.new_text,
                                hunks,
                            )
                        })
                        .is_none()
                    {
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
                    }
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
                if landed_tool_hunks
                    .as_ref()
                    .and_then(|hunks| {
                        self.upsert_review_file_change_from_landed_tool_hunks(
                            &change.path,
                            change.change_type.clone(),
                            change.old_text.as_deref(),
                            &change.new_text,
                            hunks,
                        )
                    })
                    .is_none()
                {
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
                }
                changed = true;
            } else if change.change_type != FileChangeType::Deleted {
                // The tracker could not produce an exact diff (e.g. a
                // `MissingBaseline` because the ACP diff preview landed after
                // the write). The tool card still has a diff preview, so try
                // to reconstruct the old text from the landed hunks and record
                // the review/session change so the ChangesBar and review panel
                // appear.
                if landed_tool_hunks
                    .as_ref()
                    .and_then(|hunks| {
                        self.upsert_review_file_change_from_landed_tool_hunks(
                            &change.path,
                            change.change_type.clone(),
                            change.old_text.as_deref(),
                            &change.new_text,
                            hunks,
                        )
                    })
                    .is_none()
                {
                    self.upsert_review_file_change_for_tool(
                        &change.path,
                        change.change_type.clone(),
                        exact_edit.as_ref(),
                        change
                            .old_text
                            .clone()
                            .or(previous_session_new_text.clone())
                            .or(effective_old_text.clone()),
                        change.new_text.clone(),
                    );
                }
                if self
                    .ui
                    .session_changes
                    .iter()
                    .any(|existing| normalize_tracked_path(&existing.path) == normalized)
                {
                    changed = true;
                } else {
                    self.ui.session_changes.push(SessionFileChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        old_text: change.old_text.clone().or(effective_old_text),
                        new_text: change.new_text.clone(),
                        added_lines: 0,
                        removed_lines: 0,
                        timestamp: current_timestamp(),
                    });
                    changed = true;
                }
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
        self.exact_edit_text_from_tool_payloads(tool, normalized_path)
    }

    fn exact_edit_text_from_tool_payloads(
        &self,
        tool: &ToolInvocation,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        tool.raw_input
            .as_deref()
            .and_then(|payload| self.exact_edit_text_from_tool_payload(payload, normalized_path))
            .or_else(|| {
                tool.raw_output.as_deref().and_then(|payload| {
                    self.exact_edit_text_from_tool_payload(payload, normalized_path)
                })
            })
    }

    fn exact_edit_text_from_tool_payload(
        &self,
        payload: &str,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        let json = serde_json::from_str::<serde_json::Value>(payload).ok()?;
        if let Some(unified_diff) =
            edit_input_unified_diff_for_path(&json, normalized_path, &self.ui.workspace.root)
            && let Some(exact_edit) =
                self.exact_edit_text_from_unified_diff(normalized_path, unified_diff)
        {
            return Some(exact_edit);
        }

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

    pub(super) fn apply_completed_tool_landed_edit_payload_with_raw_output(
        &mut self,
        call_id: &str,
        raw_output: Option<&str>,
    ) -> bool {
        let Some(tool) = self
            .ui
            .tools
            .iter()
            .find(|tool| tool.call_id == call_id && tool.status == ToolStatus::Succeeded)
            .cloned()
        else {
            return false;
        };

        if !is_file_write_tool_identity(&tool.kind, &tool.name) {
            return false;
        }

        let tool = if let Some(raw_output) = raw_output.filter(|value| !value.trim().is_empty()) {
            ToolInvocation {
                raw_output: Some(raw_output.to_string()),
                ..tool
            }
        } else {
            tool
        };

        let mut changes = Vec::new();
        for normalized_path in completed_tool_edit_candidate_paths(&tool, &self.ui.workspace.root) {
            let change_type = self
                .tool_payload_change_type_for_path(&tool, &normalized_path)
                .unwrap_or(FileChangeType::Modified);
            if change_type == FileChangeType::Deleted {
                let abs_path = self.ui.workspace.root.join(&normalized_path);
                if abs_path.exists() {
                    continue;
                }
                let Some(exact_edit) =
                    self.deleted_edit_text_from_tool_payloads(&tool, &normalized_path)
                else {
                    continue;
                };
                changes.push(crate::file_tracker::VerifiedFileChange {
                    path: normalized_path,
                    change_type,
                    old_text: Some(exact_edit.old_text),
                    new_text: String::new(),
                    skipped_diff: false,
                    quality: DiffQuality::Exact,
                });
                continue;
            }
            // dsh write/create shape: the arguments carry the full file
            // content (`content`/`file_text`) with no old text. When the
            // write landed before the app processed the tool-start event
            // (coalesced poll batch), the tracker baseline already reflects
            // the post-write file and the creation would be silently
            // dropped — recover it from the tool payload + disk check.
            // The payload alone cannot tell "created" from "rewrote the same
            // bytes in place", so independent confirmation is required:
            // either the result text reports the creation, or the tool card
            // carries a pure-addition diff for a file git HEAD does not know
            // (the dsh bridge only synthesizes whole-file-added previews for
            // create-shaped writes).
            if let Some(new_text) =
                write_tool_payload_created_content(&tool, &normalized_path, &self.ui.workspace.root)
                && self.completed_write_claims_creation(call_id, &tool, &normalized_path, &new_text)
            {
                let disk_matches =
                    std::fs::read_to_string(self.ui.workspace.root.join(&normalized_path))
                        .ok()
                        .map(|text| normalize_diff_text_for_session_change(&text))
                        .is_some_and(|disk| disk == new_text);
                if disk_matches {
                    changes.push(crate::file_tracker::VerifiedFileChange {
                        path: normalized_path,
                        change_type: FileChangeType::Created,
                        old_text: None,
                        new_text,
                        skipped_diff: false,
                        quality: DiffQuality::Exact,
                    });
                    continue;
                }
            }
            if change_type == FileChangeType::Created
                && let Some(new_text) =
                    self.created_edit_content_from_tool_payloads(&tool, &normalized_path)
            {
                let Ok(disk_text) =
                    std::fs::read_to_string(self.ui.workspace.root.join(&normalized_path))
                else {
                    continue;
                };
                let disk_text = normalize_diff_text_for_session_change(&disk_text);
                if disk_text != new_text {
                    continue;
                }
                changes.push(crate::file_tracker::VerifiedFileChange {
                    path: normalized_path,
                    change_type,
                    old_text: None,
                    new_text,
                    skipped_diff: false,
                    quality: DiffQuality::Exact,
                });
                continue;
            }
            let Some(exact_edit) = self.exact_edit_text_from_tool_payloads(&tool, &normalized_path)
            else {
                continue;
            };
            let Ok(disk_text) =
                std::fs::read_to_string(self.ui.workspace.root.join(&normalized_path))
            else {
                continue;
            };
            if normalize_diff_text_for_session_change(&disk_text) != exact_edit.new_text {
                continue;
            }
            let change_type = if change_type == FileChangeType::Modified
                && is_effectively_empty_text(&exact_edit.old_text)
            {
                FileChangeType::Created
            } else {
                change_type
            };
            let old_text = if change_type == FileChangeType::Created {
                None
            } else {
                Some(exact_edit.old_text)
            };
            changes.push(crate::file_tracker::VerifiedFileChange {
                path: normalized_path,
                change_type,
                old_text,
                new_text: exact_edit.new_text,
                skipped_diff: false,
                quality: DiffQuality::Exact,
            });
        }

        if changes.is_empty() {
            return false;
        }
        self.apply_tracker_changes(call_id, changes)
    }

    fn created_edit_content_from_tool_payloads(
        &self,
        tool: &ToolInvocation,
        normalized_path: &str,
    ) -> Option<String> {
        tool.raw_input
            .as_deref()
            .and_then(|payload| {
                self.created_edit_content_from_tool_payload(payload, normalized_path)
            })
            .or_else(|| {
                tool.raw_output.as_deref().and_then(|payload| {
                    self.created_edit_content_from_tool_payload(payload, normalized_path)
                })
            })
    }

    fn created_edit_content_from_tool_payload(
        &self,
        payload: &str,
        normalized_path: &str,
    ) -> Option<String> {
        let json = serde_json::from_str::<serde_json::Value>(payload).ok()?;
        if edit_input_change_type_for_path(&json, normalized_path, &self.ui.workspace.root)
            != Some(FileChangeType::Created)
        {
            return None;
        }
        edit_input_content_for_path(&json, normalized_path, &self.ui.workspace.root)
            .map(normalize_diff_text_for_session_change)
    }

    fn deleted_edit_text_from_tool_payloads(
        &self,
        tool: &ToolInvocation,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        tool.raw_input
            .as_deref()
            .and_then(|payload| self.deleted_edit_text_from_tool_payload(payload, normalized_path))
            .or_else(|| {
                tool.raw_output.as_deref().and_then(|payload| {
                    self.deleted_edit_text_from_tool_payload(payload, normalized_path)
                })
            })
    }

    fn deleted_edit_text_from_tool_payload(
        &self,
        payload: &str,
        normalized_path: &str,
    ) -> Option<ExactEditText> {
        let json = serde_json::from_str::<serde_json::Value>(payload).ok()?;
        if let Some(unified_diff) =
            edit_input_unified_diff_for_path(&json, normalized_path, &self.ui.workspace.root)
        {
            let old_text = normalize_diff_text_for_session_change(&reverse_apply_unified_diff(
                "",
                unified_diff,
            )?);
            if old_text.is_empty() {
                return None;
            }
            let hunks = diff_to_hunks(Some(&old_text), "");
            return (!hunks.is_empty()).then_some(ExactEditText {
                old_text,
                new_text: String::new(),
                hunks,
            });
        }

        let old_text = edit_input_before_text(&json).map(normalize_diff_text_for_session_change)?;
        let new_text = edit_input_after_text(&json)
            .map(normalize_diff_text_for_session_change)
            .unwrap_or_default();
        if old_text.is_empty() || !new_text.is_empty() {
            return None;
        }
        let hunks = diff_to_hunks(Some(&old_text), "");
        (!hunks.is_empty()).then_some(ExactEditText {
            old_text,
            new_text,
            hunks,
        })
    }

    fn tool_payload_change_type_for_path(
        &self,
        tool: &ToolInvocation,
        normalized_path: &str,
    ) -> Option<FileChangeType> {
        tool.raw_input
            .as_deref()
            .and_then(|payload| self.tool_payload_change_type(payload, normalized_path))
            .or_else(|| {
                tool.raw_output
                    .as_deref()
                    .and_then(|payload| self.tool_payload_change_type(payload, normalized_path))
            })
    }

    fn tool_payload_change_type(
        &self,
        payload: &str,
        normalized_path: &str,
    ) -> Option<FileChangeType> {
        let json = serde_json::from_str::<serde_json::Value>(payload).ok()?;
        edit_input_change_type_for_path(&json, normalized_path, &self.ui.workspace.root)
    }

    pub(super) fn exact_edit_text_from_unified_diff(
        &self,
        normalized_path: &str,
        unified_diff: &str,
    ) -> Option<ExactEditText> {
        let full_new = std::fs::read_to_string(self.ui.workspace.root.join(normalized_path))
            .ok()
            .map(|text| normalize_diff_text_for_session_change(&text))?;
        let full_old = reverse_apply_unified_diff(&full_new, unified_diff)?;
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

    pub(super) fn upsert_review_file_change_from_landed_tool_hunks(
        &mut self,
        path: &str,
        change_type: FileChangeType,
        expected_old_text: Option<&str>,
        new_text: &str,
        hunks: &[DiffHunk],
    ) -> Option<bool> {
        if hunks.is_empty() {
            return None;
        }

        let normalized_new_text = normalize_diff_text_for_session_change(new_text);
        let old_text = reverse_apply_diff_hunks(&normalized_new_text, hunks)?;
        if old_text == normalized_new_text {
            return None;
        }
        if !normalized_old_text_matches(
            &old_text,
            expected_old_text,
            change_type == FileChangeType::Created,
        ) {
            return None;
        }

        let canonical_old_text = if change_type == FileChangeType::Created {
            None
        } else {
            Some(old_text.as_str())
        };
        let canonical = canonical_text_diff(
            &change_type,
            canonical_old_text,
            Some(&normalized_new_text),
            None,
        );
        if canonical.quality != DiffQuality::Exact {
            return None;
        }
        let incoming_total = canonical.added_lines + canonical.removed_lines;
        if incoming_total == 0 {
            return None;
        }

        self.begin_review_changes_if_needed();
        let normalized_path = normalize_path_for_storage(path, &self.ui.workspace.root);
        let existing_index = self
            .ui
            .review_changes
            .iter()
            .position(|change| normalize_tracked_path(&change.path) == normalized_path);
        if let Some(index) = existing_index {
            let existing = &self.ui.review_changes[index];
            if existing.old_text.as_deref() != canonical.old_text.as_deref() {
                if Some(existing.new_text.as_str()) == canonical.old_text.as_deref() {
                    return None;
                }

                let cumulative = canonical_text_diff(
                    &existing.change_type,
                    existing.old_text.as_deref(),
                    Some(&normalized_new_text),
                    None,
                );
                if cumulative.quality == DiffQuality::Exact {
                    return None;
                }
            }
            let existing_canonical = canonical_text_diff(
                &existing.change_type,
                existing.old_text.as_deref(),
                Some(&existing.new_text),
                None,
            );
            if !should_replace_review_change_with_landed_hunks(
                existing,
                &existing_canonical,
                &canonical,
                hunks.len(),
            ) {
                return Some(false);
            }
        }

        let timestamp = current_timestamp();
        let stored_old_text = if change_type == FileChangeType::Created {
            None
        } else {
            canonical.old_text.clone()
        };
        if let Some(index) = existing_index {
            let existing = &mut self.ui.review_changes[index];
            existing.old_text = stored_old_text;
            existing.new_text = canonical.new_text.unwrap_or(normalized_new_text);
            existing.change_type = change_type;
            existing.added_lines = canonical.added_lines;
            existing.removed_lines = canonical.removed_lines;
            existing.timestamp = timestamp;
        } else {
            self.ui.review_changes.push(SessionFileChange {
                path: normalized_path,
                change_type,
                old_text: stored_old_text,
                new_text: canonical.new_text.unwrap_or(normalized_new_text),
                added_lines: canonical.added_lines,
                removed_lines: canonical.removed_lines,
                timestamp,
            });
        }
        self.persist_review_file_changes();
        self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
        Some(true)
    }

    pub(in crate::application) fn discard_failed_tool_speculative_diffs(
        &mut self,
        call_id: &str,
    ) -> bool {
        let paths = {
            let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == call_id) else {
                return false;
            };
            let mut paths = tool
                .diff_paths
                .iter()
                .map(|path| {
                    normalize_path_for_storage(&path.display().to_string(), &self.ui.workspace.root)
                })
                .collect::<Vec<_>>();
            paths.extend(tool.diff_previews.iter().map(|preview| {
                normalize_path_for_storage(
                    &preview.path.display().to_string(),
                    &self.ui.workspace.root,
                )
            }));
            paths.sort();
            paths.dedup();
            paths
        };
        let protected_paths = paths
            .iter()
            .filter(|path| self.has_other_successful_tool_diff_source(call_id, path))
            .cloned()
            .collect::<HashSet<_>>();

        let tool_clone = {
            let Some(tool) = self
                .ui
                .tools
                .iter_mut()
                .find(|tool| tool.call_id == call_id)
            else {
                return false;
            };
            if paths.is_empty() {
                return false;
            }

            tool.diff_paths.clear();
            tool.diff_previews.clear();
            tool.clone()
        };

        self.mark_tool_call_dirty(call_id);
        let session_id = self.ui.session.id.to_string();
        let seq = self.next_seq();
        let _ = self.store.insert_tool(&session_id, &tool_clone, seq);

        let before_review = self.ui.review_changes.len();
        self.ui.review_changes.retain(|change| {
            let path = normalize_path_for_storage(&change.path, &self.ui.workspace.root);
            !paths.contains(&path) || protected_paths.contains(&path)
        });
        let before_session = self.ui.session_changes.len();
        self.ui.session_changes.retain(|change| {
            let path = normalize_path_for_storage(&change.path, &self.ui.workspace.root);
            !paths.contains(&path) || protected_paths.contains(&path)
        });

        if before_review != self.ui.review_changes.len()
            || before_session != self.ui.session_changes.len()
        {
            self.persist_file_changes();
            self.persist_review_file_changes();
            self.persist_current_agent_turn_change_set(None, ChangeSetStatus::Pending);
        }
        true
    }

    fn has_other_successful_tool_diff_source(&self, call_id: &str, path: &str) -> bool {
        self.ui.tools.iter().any(|tool| {
            tool.call_id != call_id
                && tool.status == ToolStatus::Succeeded
                && tool_has_diff_source_for_path(tool, path, &self.ui.workspace.root)
        })
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

        let completed_tool_ids: HashSet<&str> =
            completed_tool_ids.iter().map(String::as_str).collect();

        // Look only at tools that completed in this poll batch. Scanning all historical
        // tools every 220ms makes long CodeBuddy sessions burn the desktop process.
        let mut write_paths: Vec<(String, String)> = Vec::new();
        for tool in self.ui.tools.iter().filter(|t| {
            t.status == ToolStatus::Succeeded && completed_tool_ids.contains(t.call_id.as_str())
        }) {
            let mut add_path = |path: String| {
                let normalized_storage = normalize_path_for_storage(&path, &workspace_root);
                let normalized = normalize_path(&normalized_storage);
                if let Some(existing) = write_paths.iter_mut().find(|(call_id, existing_path)| {
                    call_id == &tool.call_id && normalize_path(existing_path) == normalized
                }) {
                    existing.1 = path;
                } else {
                    write_paths.push((tool.call_id.clone(), path));
                }
            };

            // Check summary for common edit result patterns.
            for prefix in ["Editing ", "Edited ", "已编辑 "] {
                if let Some(path) = tool.summary.strip_prefix(prefix) {
                    add_path(path.to_string());
                    break;
                }
            }

            // Check raw_input JSON for file_path/filePath/path fields in edit/write tools
            if is_file_write_tool_identity(&tool.kind, &tool.name) {
                for path in tool_event_hint_paths(tool.raw_input.as_deref()) {
                    add_path(path);
                }
            }

            // Shell tools can write files via command text (Set-Content, Out-File,
            // redirects, etc.) without emitting ACP ToolDiff events.
            for path in tool_command_write_hint_paths(tool.raw_input.as_deref()) {
                add_path(path);
            }
            if !tool.detail_text.trim().is_empty() {
                for path in tool_command_write_hint_paths(Some(tool.detail_text.as_str())) {
                    add_path(path);
                }
            }
        }
        write_paths.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        write_paths.dedup_by(|a, b| a.0 == b.0 && normalize_path(&a.1) == normalize_path(&b.1));

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
                let normalized_new_text = normalize_diff_text_for_session_change(&new_text);
                let existing_tool_hunks = self.existing_tool_diff_hunks(&call_id, &normalized);
                if normalize_diff_text_for_session_change(&previous_session_new_text)
                    == normalized_new_text
                    && exact_edit.is_none()
                    && existing_tool_hunks.as_ref().is_none_or(Vec::is_empty)
                {
                    continue;
                }
                let landed_tool_hunks = compatible_landed_tool_hunks(
                    existing_tool_hunks,
                    Some(&previous_session_new_text),
                    false,
                    &new_text,
                );
                let tool_hunks = landed_tool_hunks.clone().unwrap_or_else(|| {
                    exact_edit
                        .as_ref()
                        .map(|edit| edit.hunks.clone())
                        .unwrap_or_else(|| {
                            tool_diff_hunks_for_detected_write(
                                Some(&previous_session_new_text),
                                None,
                                &new_text,
                            )
                        })
                });
                if old_text.as_deref().unwrap_or_default() == new_text {
                    self.ui.session_changes.remove(index);
                    if landed_tool_hunks
                        .as_ref()
                        .and_then(|hunks| {
                            self.upsert_review_file_change_from_landed_tool_hunks(
                                &normalized,
                                FileChangeType::Modified,
                                Some(&previous_session_new_text),
                                &new_text,
                                hunks,
                            )
                        })
                        .is_none()
                    {
                        self.upsert_review_file_change_for_tool(
                            &normalized,
                            FileChangeType::Modified,
                            exact_edit.as_ref(),
                            Some(previous_session_new_text.clone()),
                            new_text.clone(),
                        );
                    }
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
                if landed_tool_hunks
                    .as_ref()
                    .and_then(|hunks| {
                        self.upsert_review_file_change_from_landed_tool_hunks(
                            &normalized,
                            review_change_type.clone(),
                            Some(&previous_session_new_text),
                            &new_text,
                            hunks,
                        )
                    })
                    .is_none()
                {
                    self.upsert_review_file_change_for_tool(
                        &normalized,
                        review_change_type,
                        exact_edit.as_ref(),
                        Some(previous_session_new_text),
                        new_text,
                    );
                }
                changed = true;

                self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
            } else if let Some(change) = self.tracker_verified_file_change(&call_id, &normalized) {
                changed |= self.apply_tracker_changes(&call_id, vec![change]);
            } else if let Some(exact_edit) = self.exact_edit_text_for_tool(&call_id, &normalized) {
                changed |= self.apply_tracker_changes(
                    &call_id,
                    vec![crate::file_tracker::VerifiedFileChange {
                        path: normalized,
                        change_type: FileChangeType::Modified,
                        old_text: Some(exact_edit.old_text),
                        new_text: exact_edit.new_text,
                        skipped_diff: false,
                        quality: DiffQuality::Exact,
                    }],
                );
            } else if self
                .file_tracker
                .has_active_candidate(&call_id, &normalized)
            {
                continue;
            }
        }

        changed
    }

    pub(super) fn apply_verified_fs_write_tool_diff(
        &mut self,
        call_id: &str,
        path: &str,
        old_text: Option<&str>,
        new_text: &str,
    ) -> bool {
        if !call_id.starts_with("fs_write:") {
            return false;
        }

        let normalized = normalize_path_for_storage(path, &self.ui.workspace.root);
        let abs_path = self.ui.workspace.root.join(&normalized);
        let Ok(disk_text) = std::fs::read_to_string(abs_path) else {
            return false;
        };
        let disk_text = normalize_diff_text_for_session_change(&disk_text);
        let new_text = normalize_diff_text_for_session_change(new_text);
        if disk_text != new_text {
            return false;
        }

        let old_text = old_text.map(normalize_diff_text_for_session_change);
        let change_type = if old_text.is_some() {
            FileChangeType::Modified
        } else {
            FileChangeType::Created
        };
        self.apply_tracker_changes(
            call_id,
            vec![crate::file_tracker::VerifiedFileChange {
                path: normalized,
                change_type,
                old_text,
                new_text,
                skipped_diff: false,
                quality: DiffQuality::Exact,
            }],
        )
    }

    /// Land a real tool-call `ToolDiff` (non-`fs_write:` id) whose `old_text`
    /// carries a trustworthy before-content. The dsh harness computes a write's
    /// result diff card against the pre-write file, so when the whole tool
    /// lifecycle lands in one coalesced poll batch (ToolStarted processed only
    /// after the write hit the disk) the tracker baseline equals the post-write
    /// content and `finish_recording` sees "no change" — but the diff event
    /// still holds the genuine old text. That is exactly how an overwrite of a
    /// tracked file (`dsh write` rewriting `mesh_env.py`/`rollout.py`) used to
    /// vanish from the review panel while other writes survived by batch
    /// timing. Land the modification directly when the tool is write-identity,
    /// its payload path matches and its content equals the diff's new text,
    /// and the disk actually holds the new text — every guard anti-spoofs a
    /// fabricated diff. An empty/absent `old_text` (a creation) stays owned by
    /// the payload-created recovery.
    pub(super) fn apply_landed_write_tool_diff(
        &mut self,
        call_id: &str,
        path: &str,
        old_text: Option<&str>,
        new_text: &str,
    ) -> bool {
        if call_id.starts_with("fs_write:") {
            // `apply_verified_fs_write_tool_diff` owns the host-mediated
            // fs-write ids.
            return false;
        }
        let Some(old_text) = old_text.filter(|text| !text.trim().is_empty()) else {
            return false;
        };
        let Some(tool) = self
            .ui
            .tools
            .iter()
            .find(|tool| tool.call_id == call_id)
            .cloned()
        else {
            return false;
        };
        if !is_file_write_tool_identity(&tool.kind, &tool.name) {
            return false;
        }
        let normalized = normalize_path_for_storage(path, &self.ui.workspace.root);
        let payload_confirms =
            write_tool_payload_created_content(&tool, &normalized, &self.ui.workspace.root)
                .is_some_and(|content| content == normalize_diff_text_for_session_change(new_text));
        if !payload_confirms {
            return false;
        }
        let new_text = normalize_diff_text_for_session_change(new_text);
        let old_text = normalize_diff_text_for_session_change(old_text);
        if old_text == new_text {
            return false;
        }
        let disk_matches = std::fs::read_to_string(self.ui.workspace.root.join(&normalized))
            .ok()
            .map(|text| normalize_diff_text_for_session_change(&text) == new_text)
            .unwrap_or(false);
        if !disk_matches {
            return false;
        }
        self.apply_tracker_changes(
            call_id,
            vec![crate::file_tracker::VerifiedFileChange {
                path: normalized,
                change_type: FileChangeType::Modified,
                old_text: Some(old_text),
                new_text,
                skipped_diff: false,
                quality: DiffQuality::Exact,
            }],
        )
    }

    fn tracker_verified_file_change(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<crate::file_tracker::VerifiedFileChange> {
        let abs_path = self.ui.workspace.root.join(normalized_path);
        let baseline = self
            .file_tracker
            .get_baseline_text(call_id, normalized_path);
        if let Some(baseline) = baseline {
            let new_text = std::fs::read_to_string(abs_path).ok()?;
            let old_text = normalize_diff_text_for_session_change(baseline);
            let new_text = normalize_diff_text_for_session_change(&new_text);
            if old_text == new_text {
                return None;
            }
            return Some(crate::file_tracker::VerifiedFileChange {
                path: normalized_path.to_string(),
                change_type: FileChangeType::Modified,
                old_text: Some(old_text),
                new_text,
                skipped_diff: false,
                quality: DiffQuality::Exact,
            });
        }

        if self
            .file_tracker
            .was_missing_at_start(call_id, normalized_path)
            .is_some_and(|missing| missing)
        {
            let new_text = std::fs::read_to_string(abs_path).ok()?;
            return Some(crate::file_tracker::VerifiedFileChange {
                path: normalized_path.to_string(),
                change_type: FileChangeType::Created,
                old_text: None,
                new_text: normalize_diff_text_for_session_change(&new_text),
                skipped_diff: false,
                quality: DiffQuality::Exact,
            });
        }

        None
    }
}

fn tool_raw_input_looks_like_shell_command(raw_input: Option<&str>) -> bool {
    let Some(raw_input) = raw_input else {
        return false;
    };
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        return json_has_shell_command_field(&value);
    }
    false
}

fn json_has_shell_command_field(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                let key = key.trim_matches('"').to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "command" | "cmd" | "shell_command" | "shellcommand" | "script"
                ) && !child.is_null()
                {
                    return true;
                }
                if json_has_shell_command_field(child) {
                    return true;
                }
            }
            false
        }
        serde_json::Value::Array(items) => items.iter().any(json_has_shell_command_field),
        _ => false,
    }
}

fn tool_has_diff_source_for_path(tool: &ToolInvocation, path: &str, workspace_root: &Path) -> bool {
    tool.diff_paths.iter().any(|diff_path| {
        normalize_path_for_storage(&diff_path.display().to_string(), workspace_root) == path
    }) || tool.diff_previews.iter().any(|preview| {
        normalize_path_for_storage(&preview.path.display().to_string(), workspace_root) == path
            && preview.hunks.iter().any(|hunk| {
                hunk.lines
                    .iter()
                    .any(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
            })
    })
}

/// Content of a write-shaped tool payload targeting `normalized_path`: the
/// arguments carry the full file content (`content`/`file_text`/
/// `new_content`) and no old text (no `old_string`/`before`). This is the
/// dsh `write` tool and `str_replace_editor` `create` shape.
fn write_tool_payload_created_content(
    tool: &ToolInvocation,
    normalized_path: &str,
    workspace_root: &Path,
) -> Option<String> {
    let payload = tool.raw_input.as_deref()?;
    let json = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let payload_path = json
        .get("file_path")
        .or_else(|| json.get("path"))
        .or_else(|| json.get("filePath"))
        .and_then(|value| value.as_str())?;
    if normalize_path_for_storage(payload_path, workspace_root) != normalized_path {
        return None;
    }
    if edit_input_before_text(&json).is_some() {
        return None;
    }
    let content = json
        .get("content")
        .or_else(|| json.get("file_text"))
        .or_else(|| json.get("new_content"))
        .or_else(|| json.get("newContent"))
        .and_then(|value| value.as_str())?;
    (!content.trim().is_empty()).then(|| normalize_diff_text_for_session_change(content))
}

/// Whether the tool result text reports that a new file was created (dsh
/// `write` replies with "Created file"; `str_replace_editor` `create` replies
/// with "New file created successfully at: ...").
fn tool_result_reports_created_file(tool: &ToolInvocation) -> bool {
    let Some(raw_output) = tool.raw_output.as_deref() else {
        return false;
    };
    let lower = raw_output.to_ascii_lowercase();
    lower.contains("created file") || lower.contains("new file created successfully")
}

/// Whether the tool card's diff preview for the path only adds lines (no
/// removals). The dsh bridge emits such a preview only for create-shaped
/// writes — a `content`/`file_text` payload with no old text produces a
/// whole-file-added synthetic diff, and a result diff card with a null old
/// text means the same — so a pure-addition preview is the tool itself
/// claiming "this call created the file".
fn tool_preview_only_adds_lines(
    tool: &ToolInvocation,
    normalized_path: &str,
    workspace_root: &Path,
) -> bool {
    tool.diff_previews.iter().any(|preview| {
        normalize_path_for_storage(&preview.path.display().to_string(), workspace_root)
            == normalized_path
            && !preview.hunks.is_empty()
            && preview.hunks.iter().all(|hunk| {
                hunk.lines
                    .iter()
                    .all(|line| !matches!(line.kind, DiffLineKind::Removed))
            })
            && preview
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| matches!(line.kind, DiffLineKind::Added))
    })
}

impl Application {
    /// Independent confirmation that a completed write-shaped tool created the
    /// target file. The write-shaped payload (`content`/`file_path`, no old
    /// text) alone is not proof — the agent may have rewritten a pre-existing
    /// file — so creation is trusted only when the result text reports it, or
    /// when the tracker cannot produce a believable before/after for this
    /// call AND the tool card only adds lines AND the file is not tracked in
    /// git HEAD. The "baseline equals the payload" case is exactly what a
    /// coalesced poll batch looks like: ToolStarted was processed after the
    /// write had already landed, so the captured baseline equals the payload.
    /// With a real distinct pre-write baseline the normal completion path
    /// records the authentic change — never claim a whole-file creation there.
    fn completed_write_claims_creation(
        &self,
        call_id: &str,
        tool: &ToolInvocation,
        normalized_path: &str,
        payload_text: &str,
    ) -> bool {
        if tool_result_reports_created_file(tool) {
            return true;
        }
        if !tool_preview_only_adds_lines(tool, normalized_path, &self.ui.workspace.root) {
            return false;
        }
        let tracker_cannot_verify = match self
            .file_tracker
            .get_baseline_text(call_id, normalized_path)
        {
            Some(baseline) => normalize_diff_text_for_session_change(baseline) == payload_text,
            None => self
                .file_tracker
                .was_missing_at_start(call_id, normalized_path)
                .unwrap_or(false),
        };
        tracker_cannot_verify && self.git_head_text_for_path(normalized_path).is_none()
    }
}

fn completed_tool_edit_candidate_paths(
    tool: &ToolInvocation,
    workspace_root: &Path,
) -> Vec<String> {
    let mut paths = Vec::new();
    paths.extend(tool_event_hint_paths(tool.raw_input.as_deref()));
    paths.extend(tool_event_change_paths(tool.raw_output.as_deref()));
    paths.extend(
        tool.diff_paths
            .iter()
            .map(|path| path.display().to_string()),
    );
    paths.extend(
        tool.diff_previews
            .iter()
            .map(|preview| preview.path.display().to_string()),
    );
    paths = paths
        .into_iter()
        .map(|path| normalize_path_for_storage(&path, workspace_root))
        .filter(|path| !path.trim().is_empty())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn compatible_landed_tool_hunks(
    hunks: Option<Vec<DiffHunk>>,
    expected_old_text: Option<&str>,
    allow_empty_old_text: bool,
    new_text: &str,
) -> Option<Vec<DiffHunk>> {
    let hunks = hunks?;
    if hunks.is_empty() || looks_like_whole_file_addition_hunks(&hunks) {
        return None;
    }
    let normalized_new_text = normalize_diff_text_for_session_change(new_text);
    let old_text = reverse_apply_diff_hunks(&normalized_new_text, &hunks)?;
    if old_text == normalized_new_text
        || !normalized_old_text_matches(&old_text, expected_old_text, allow_empty_old_text)
    {
        return None;
    }
    Some(hunks)
}

fn normalized_old_text_matches(
    actual_old_text: &str,
    expected_old_text: Option<&str>,
    allow_empty_old_text: bool,
) -> bool {
    if let Some(expected_old_text) = expected_old_text {
        return actual_old_text == normalize_diff_text_for_session_change(expected_old_text);
    }
    // No baseline is available (e.g. the tracker could not anchor a tool-start
    // snapshot because the ACP diff preview landed after the write). Accept the
    // old text reconstructed from the tool's own diff hunks as long as it is
    // non-empty — this lets the review change set pick up edits that would
    // otherwise only appear on the tool card. An empty reconstructed baseline
    // is only trustworthy for created files.
    if is_effectively_empty_text(actual_old_text) {
        return allow_empty_old_text;
    }
    true
}

fn is_effectively_empty_text(text: &str) -> bool {
    normalize_diff_text_for_session_change(text)
        .trim_matches('\n')
        .is_empty()
}

fn should_replace_review_change_with_landed_hunks(
    existing_change: &SessionFileChange,
    existing_diff: &CanonicalTextDiff,
    incoming: &CanonicalTextDiff,
    incoming_hunk_count: usize,
) -> bool {
    if incoming.quality != DiffQuality::Exact {
        return false;
    }

    let incoming_total = incoming.added_lines + incoming.removed_lines;
    if incoming_total == 0 {
        return false;
    }

    if existing_change.old_text.as_deref() == incoming.old_text.as_deref()
        && Some(existing_change.new_text.as_str()) == incoming.new_text.as_deref()
    {
        return false;
    }

    if existing_diff.quality != DiffQuality::Exact {
        return true;
    }

    let existing_total = existing_diff.added_lines + existing_diff.removed_lines;
    if incoming_total > existing_total {
        return true;
    }

    if incoming_hunk_count > existing_diff.hunks.len() {
        return true;
    }

    let existing_text_size = existing_change
        .old_text
        .as_deref()
        .map(str::len)
        .unwrap_or_default()
        + existing_change.new_text.len();
    let incoming_text_size = incoming
        .old_text
        .as_deref()
        .map(str::len)
        .unwrap_or_default()
        + incoming
            .new_text
            .as_deref()
            .map(str::len)
            .unwrap_or_default();

    incoming_hunk_count >= existing_diff.hunks.len() && incoming_text_size > existing_text_size
}
