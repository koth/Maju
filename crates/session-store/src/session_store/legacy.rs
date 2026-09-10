use uuid::Uuid;
use workspace_model::{
    ChangeSetSource, ChangeSetStatus, ChangeSetSummary, DiffQuality, FileChangeRecord,
    FileChangeSummary, FileChangeType, SessionFileChange,
};

use super::util::normalize_change_path;

pub(super) const LEGACY_AGENT_CONVERSATION_PREFIX: &str = "legacy:agent-conversation:";
pub(super) const LEGACY_AGENT_RECENT_PREFIX: &str = "legacy:agent-recent:";
pub(super) const LEGACY_AGENT_TURN_PREFIX: &str = "legacy:agent-turn:";

pub(super) fn legacy_agent_conversation_id(session_id: &str) -> String {
    format!("{LEGACY_AGENT_CONVERSATION_PREFIX}{session_id}")
}

pub(super) fn legacy_agent_recent_id(session_id: &str) -> String {
    format!("{LEGACY_AGENT_RECENT_PREFIX}{session_id}")
}

pub(super) fn legacy_agent_turn_id(session_id: &str, message_id: &Uuid) -> String {
    format!("{LEGACY_AGENT_TURN_PREFIX}{session_id}:{message_id}")
}

pub(super) fn legacy_records_from_session_changes(
    change_set_id: &str,
    changes: Vec<SessionFileChange>,
) -> Vec<FileChangeRecord> {
    changes
        .into_iter()
        .map(|change| {
            let quality = if change.old_text.is_some()
                || matches!(change.change_type, FileChangeType::Created)
            {
                DiffQuality::Exact
            } else {
                DiffQuality::LegacyIncomplete
            };
            FileChangeRecord {
                change_set_id: change_set_id.to_string(),
                path: normalize_change_path(&change.path),
                change_type: change.change_type,
                old_text: change.old_text,
                new_text: Some(change.new_text),
                added_lines: change.added_lines,
                removed_lines: change.removed_lines,
                quality,
                updated_at: change.timestamp,
            }
        })
        .collect()
}

/// Summary built from a `COUNT`/`SUM`/`MAX` aggregate instead of the records
/// themselves: the legacy fallback needs nothing but file count, line totals
/// and the newest timestamp, so the summary list never has to materialize
/// every historical diff text.
#[allow(clippy::too_many_arguments)]
pub(super) fn summarize_change_aggregate(
    id: String,
    source: ChangeSetSource,
    session_id: &str,
    message_id: Option<Uuid>,
    label: &str,
    status: ChangeSetStatus,
    workspace_root: &str,
    file_count: usize,
    added_lines: usize,
    removed_lines: usize,
    updated_at: String,
) -> ChangeSetSummary {
    ChangeSetSummary {
        id,
        source,
        session_id: Uuid::parse_str(session_id).ok(),
        workspace_root: workspace_root.to_string(),
        message_id,
        tool_call_id: None,
        owner_key: None,
        label: label.to_string(),
        added_lines,
        removed_lines,
        file_count,
        updated_at,
        status,
    }
}

pub(super) fn file_summary_from_record(record: &FileChangeRecord) -> FileChangeSummary {
    FileChangeSummary {
        change_set_id: record.change_set_id.clone(),
        path: record.path.clone(),
        change_type: record.change_type.clone(),
        added_lines: record.added_lines,
        removed_lines: record.removed_lines,
        quality: record.quality.clone(),
        updated_at: record.updated_at.clone(),
    }
}
