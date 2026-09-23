use super::{Application, normalize_path_for_storage, normalize_tracked_path};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use workspace_model::{
    ChatMessageDelta, DiffLineKind, FileChangeType, RepositorySnapshot, SessionFileChange,
    ToolDiffPreview, ToolInvocation, UiSnapshotPatch,
};

const SNAPSHOT_TOOL_DETAIL_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_RAW_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_OUTPUT_CHARS: usize = 8 * 1024;
const SNAPSHOT_TOOL_LOG_CHARS: usize = 1024;
const SNAPSHOT_TOOL_LOG_ENTRIES: usize = 6;

#[derive(Debug, Default)]
pub struct UiPatchCursor {
    revision: u64,
    workspace_id: Option<uuid::Uuid>,
    session_id: Option<uuid::Uuid>,
    timeline_len: usize,
    message_bodies: HashMap<uuid::Uuid, String>,
    known_tool_ids: HashSet<uuid::Uuid>,
    repository: Option<RepositorySnapshot>,
}

pub enum UiSnapshotUpdate {
    Full(workspace_model::UiSnapshot),
    Patch(UiSnapshotPatch),
}

/// Metadata-only copy of a file change: keeps path/kind/line counts/timestamp
/// but drops the embedded file contents. Snapshots and patches never carry
/// diff text — diff views fetch `old_text`/`new_text` on demand instead, so
/// renderer memory and per-patch serialization stop scaling with edit volume.
fn metadata_only_change(change: &SessionFileChange) -> SessionFileChange {
    SessionFileChange {
        path: change.path.clone(),
        change_type: change.change_type.clone(),
        old_text: None,
        new_text: String::new(),
        added_lines: change.added_lines,
        removed_lines: change.removed_lines,
        timestamp: change.timestamp.clone(),
    }
}

fn metadata_only_turn_changes(
    turn_changes: &[workspace_model::TurnFileChanges],
) -> Vec<workspace_model::TurnFileChanges> {
    turn_changes
        .iter()
        .map(|turn| workspace_model::TurnFileChanges {
            message_id: turn.message_id,
            changes: turn.changes.iter().map(metadata_only_change).collect(),
        })
        .collect()
}

fn lightweight_tool_invocation(
    tool: &ToolInvocation,
    created_change_paths: &HashSet<String>,
    workspace_root: &Path,
) -> ToolInvocation {
    let mut next = tool.clone();
    cap_string_in_place(&mut next.detail_text, SNAPSHOT_TOOL_DETAIL_CHARS);
    next.raw_input = next
        .raw_input
        .as_deref()
        .map(|value| cap_snapshot_tool_raw_input(value, SNAPSHOT_TOOL_RAW_CHARS));
    next.raw_output = next
        .raw_output
        .as_deref()
        .map(|value| capped_snapshot_string(value, SNAPSHOT_TOOL_OUTPUT_CHARS));
    if let Some(output) = &mut next.terminal_output {
        cap_string_in_place(&mut output.output, SNAPSHOT_TOOL_OUTPUT_CHARS);
    }
    if next.logs.len() > SNAPSHOT_TOOL_LOG_ENTRIES {
        let keep_from = next.logs.len() - SNAPSHOT_TOOL_LOG_ENTRIES;
        next.logs.drain(0..keep_from);
    }
    for entry in &mut next.logs {
        cap_string_in_place(&mut entry.body, SNAPSHOT_TOOL_LOG_CHARS);
    }
    next.diff_previews.retain(|preview| {
        created_change_paths.contains(&snapshot_path_key(
            &preview.path.display().to_string(),
            workspace_root,
        )) || !looks_like_bogus_whole_file_preview(preview)
    });
    next
}

impl UiPatchCursor {
    fn reset_from_snapshot(&mut self, snapshot: &workspace_model::UiSnapshot) {
        self.revision = snapshot.revision;
        self.workspace_id = Some(snapshot.workspace.id);
        self.session_id = Some(snapshot.session.id);
        self.timeline_len = snapshot.timeline.len();
        self.message_bodies = snapshot
            .messages
            .iter()
            .map(|message| (message.id, message.body.clone()))
            .collect();
        self.known_tool_ids = snapshot.tools.iter().map(|tool| tool.id).collect();
        self.repository = Some(snapshot.repository.clone());
    }
}

fn capped_snapshot_string(value: &str, max_chars: usize) -> String {
    let mut output = value.to_string();
    cap_string_in_place(&mut output, max_chars);
    output
}

fn cap_snapshot_tool_raw_input(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let Ok(parsed) = serde_json::from_str::<Value>(value) else {
        return capped_snapshot_string(value, max_chars);
    };
    let Some(object) = parsed.as_object() else {
        return capped_snapshot_string(value, max_chars);
    };

    let mut retained = Map::new();
    for key in TOOL_RAW_INPUT_PRIORITY_KEYS {
        if let Some(field) = object.get(*key) {
            retained.insert((*key).to_string(), cap_json_value(field.clone(), 1024));
        }
    }

    for (key, field) in object {
        if retained.contains_key(key) {
            continue;
        }
        if should_keep_tool_raw_input_field(field) {
            retained.insert(key.clone(), cap_json_value(field.clone(), 512));
        }
    }

    retained.insert("_truncated".into(), Value::Bool(true));
    let serialized = serde_json::to_string(&Value::Object(retained));
    let Ok(serialized) = serialized else {
        return capped_snapshot_string(value, max_chars);
    };
    if serialized.chars().count() <= max_chars {
        return serialized;
    }

    let mut compact = Map::new();
    for key in TOOL_RAW_INPUT_PRIORITY_KEYS {
        if let Some(field) = object.get(*key) {
            compact.insert((*key).to_string(), cap_json_value(field.clone(), 256));
        }
    }
    compact.insert("_truncated".into(), Value::Bool(true));
    serde_json::to_string(&Value::Object(compact))
        .ok()
        .filter(|serialized| serialized.chars().count() <= max_chars)
        .unwrap_or_else(|| capped_snapshot_string(value, max_chars))
}

const TOOL_RAW_INPUT_PRIORITY_KEYS: &[&str] = &[
    "description",
    "command",
    "cmd",
    "shell_command",
    "command_line",
    "args",
    "file_path",
    "filePath",
    "path",
    "pattern",
    "include",
    "url",
    "query",
    "prompt",
    "old_string",
    "oldString",
    "new_string",
    "newString",
    "before",
    "after",
    "oldText",
    "newText",
    "replacement",
    "parent_tool_call_id",
    "subagent_type",
];

fn should_keep_tool_raw_input_field(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => text.chars().count() <= 256,
        Value::Array(items) => {
            items.len() <= 16
                && items.iter().all(|item| {
                    matches!(item, Value::String(_) | Value::Number(_) | Value::Bool(_))
                })
        }
        Value::Object(_) => false,
    }
}

fn cap_json_value(value: Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let mut output = text;
            cap_string_in_place(&mut output, max_chars);
            Value::String(output)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(16)
                .map(|item| cap_json_value(item, max_chars / 2))
                .collect(),
        ),
        other => other,
    }
}

fn cap_string_in_place(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }
    let mut capped: String = value.chars().take(max_chars).collect();
    capped.push_str("\n...");
    *value = capped;
}

/// Replace a body's inline image blocks with a note, keeping the prose before
/// the first one.
///
/// Used when an image body cannot travel whole: a truncated `data:` URL is worse
/// than no image at all, because the phone renders the leftover base64 as text.
/// The desktop reads the same message from its own unprojected snapshot, so only
/// the phone sees the note.
fn omit_inline_images(body: &str) -> String {
    let Some(first) = body.find("![") else {
        return REMOTE_IMAGE_OMITTED_NOTE.to_string();
    };
    let prose = body[..first].trim_end();
    if prose.is_empty() {
        return REMOTE_IMAGE_OMITTED_NOTE.to_string();
    }
    format!("{prose}\n\n{REMOTE_IMAGE_OMITTED_NOTE}")
}

fn looks_like_bogus_whole_file_preview(preview: &ToolDiffPreview) -> bool {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in preview.hunks.iter().flat_map(|hunk| &hunk.lines) {
        match line.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    added >= 100 && (removed == 0 || added > removed * 4)
}

fn created_change_path_keys(
    changes: &[SessionFileChange],
    workspace_root: &Path,
) -> HashSet<String> {
    changes
        .iter()
        .filter(|change| change.change_type == FileChangeType::Created)
        .map(|change| snapshot_path_key(&change.path, workspace_root))
        .collect()
}

fn snapshot_path_key(path: &str, workspace_root: &Path) -> String {
    normalize_tracked_path(&normalize_path_for_storage(path, workspace_root))
}

// ── Remote (mobile relay) projection ─────────────────────────────────────
//
// The phone renders only the conversation surface: timeline, messages, tool
// cards (incl. pending permission sheets). Everything else the desktop
// snapshot carries — git repository state, inspector sections, change lists,
// plan entries, available commands, session config controls, usage stats,
// thinking text — is dead weight on the relay link, and a Full snapshot for a
// long session can otherwise reach multiple megabytes. These projections cut
// the wire payload to the conversation window plus the fields the mobile
// reducer consumes.

/// Timeline entries kept in a remote Full snapshot. Older entries page in
/// through the desktop UI's history paging; the phone has no history pager,
/// so anything older than this window is unreachable there anyway.
const REMOTE_TIMELINE_WINDOW: usize = 200;
/// Total character budget for message bodies in one remote Full snapshot.
///
/// Bodies are kept **whole, newest first**, until this budget is spent; only
/// the older messages beyond it fall back to [`REMOTE_MESSAGE_BODY_CHARS`].
/// Two reasons the newest bodies must travel intact:
///
/// - The phone is a reading surface. A flat per-message cap silently cut the
///   tail off every long reply (the reported symptom: a 2138-char answer
///   rendered exactly up to char 2048 followed by the cap marker).
/// - `message_deltas` is an append-only chain the phone seeds from this body
///   and then validates against the backend's `base_len` (the length of the
///   *unprojected* body). A capped body made every later append mismatch and
///   the phone resynced instead of streaming the rest of the answer.
///
/// The budget keeps the frame bounded for pathological sessions while covering
/// every realistic conversation: the largest non-image body in a 28k-message
/// store is ~137 KB.
const REMOTE_MESSAGE_BODY_BUDGET: usize = 1024 * 1024;
/// Head cap for message bodies that no longer fit the budget. These are the
/// oldest messages in the window, which never receive streaming deltas again,
/// so truncating them cannot desync the append chain (matches the historical
/// behavior).
const REMOTE_MESSAGE_BODY_CHARS: usize = 2 * 1024;
/// Bodies carrying an inline image data URL embed base64 the phone must receive
/// **whole**: truncating mid-base64 leaves an unterminated markdown image the
/// phone can neither parse nor render, so it shows raw base64 text instead of a
/// picture.
///
/// Two producers share this shape and they are very different in size:
///
/// - a user attachment embeds a 64x64 canvas thumbnail (~2-8KB of base64) and
///   keeps the original file as a `file://` title, and
/// - a generated image (`extract_generated_image_markdown`) embeds the
///   provider's file, which is a full-size PNG — measured at 0.8-2.1MB on disk,
///   so 1.1-2.8MB of base64.
///
/// The allowance therefore has to cover the generated case; the old 16KB limit
/// truncated every generated image. Truncation is never the fallback anymore
/// (see [`REMOTE_IMAGE_BODY_BUDGET`]).
const REMOTE_IMAGE_BODY_CHARS: usize = 3 * 1024 * 1024;
/// Cumulative allowance for image bodies in one Full snapshot, newest first.
/// An image body that no longer fits is replaced by a note rather than being
/// truncated mid-base64. This keeps one snapshot bounded even in a session with
/// many generated images, while the picture the user is actually looking at
/// (the newest one) always travels intact.
const REMOTE_IMAGE_BODY_BUDGET: usize = 6 * 1024 * 1024;
/// Placeholder for an image body that had to be dropped from a remote payload.
const REMOTE_IMAGE_OMITTED_NOTE: &str = "[图片过大，请在电脑端查看]";
/// Per-tool free-text cap for remote payloads.
const REMOTE_TOOL_TEXT_CHARS: usize = 2 * 1024;

/// Trim a Full snapshot for the mobile relay path.
///
/// - Windows the timeline to the last [`REMOTE_TIMELINE_WINDOW`] entries and
///   keeps only the messages/tools those entries reference (plus tools with a
///   pending permission request, which the phone surfaces from `snapshot.tools`
///   even when their timeline entry was trimmed). This bounds the payload on
///   long-running turns where the in-memory timeline keeps growing.
/// - Caps message bodies and tool free-text fields.
/// - Zeroes the fields the mobile UI never reads (changes, plan, commands,
///   config controls, usage, repository, thinking text).
///
/// Patch cursor invariants: the caller must keep its `UiPatchCursor` reset
/// from the UNPROJECTED snapshot (as `lightweight_ui_update` does), so delta
/// chains keep tracking the real bodies while only the wire payload shrinks.
pub fn project_remote_snapshot(
    mut snapshot: workspace_model::UiSnapshot,
) -> workspace_model::UiSnapshot {
    // Aligned window: trim the timeline tail first, then keep only the
    // referenced entities so the phone renders no "(missing …)" placeholders.
    if snapshot.timeline.len() > REMOTE_TIMELINE_WINDOW {
        let start = snapshot.timeline.len() - REMOTE_TIMELINE_WINDOW;
        snapshot.timeline.drain(0..start);
    }
    let mut referenced_messages = HashSet::new();
    let mut referenced_tools = HashSet::new();
    for item in &snapshot.timeline {
        match item {
            workspace_model::TimelineItem::Message(id) => {
                referenced_messages.insert(*id);
            }
            workspace_model::TimelineItem::Tool(id) => {
                referenced_tools.insert(*id);
            }
            workspace_model::TimelineItem::Thinking(_) => {}
        }
    }
    snapshot.messages.retain(|m| referenced_messages.contains(&m.id));
    // Pending-permission tools must survive the trim: the phone derives the
    // approval sheet from `snapshot.tools`, not from the timeline.
    snapshot.tools.retain(|t| {
        referenced_tools.contains(&t.id)
            || (t.permission_input.is_some() && t.permission_decision.is_none())
    });

    // Bodies: keep the newest ones whole until the budget is spent; cap only
    // what is left over (oldest-first), so the message the user is actually
    // reading never arrives truncated.
    let mut remaining_budget = REMOTE_MESSAGE_BODY_BUDGET;
    let mut remaining_image_budget = REMOTE_IMAGE_BODY_BUDGET;
    for message in snapshot.messages.iter_mut().rev() {
        let len = message.body.chars().count();
        // Image bodies embed base64 the phone parses in full. They do not draw
        // on the text budget, but they have their own: a body that fits is sent
        // whole, and one that does not is dropped with a note — truncating it
        // would leave an unterminated markdown image, which is exactly the
        // "the phone shows raw base64" bug this allowance exists to prevent.
        if message.body.contains("data:image/") {
            if len <= REMOTE_IMAGE_BODY_CHARS && len <= remaining_image_budget {
                remaining_image_budget -= len;
            } else {
                message.body = omit_inline_images(&message.body);
            }
            continue;
        }
        // The newest body may spend whatever budget is left (a single huge
        // message still has to render); older ones fall back to the small
        // historical head once the budget is gone.
        let allow = remaining_budget.max(REMOTE_MESSAGE_BODY_CHARS);
        cap_string_in_place(&mut message.body, allow);
        remaining_budget = remaining_budget.saturating_sub(len.min(allow));
    }
    for tool in &mut snapshot.tools {
        cap_string_in_place(&mut tool.summary, REMOTE_TOOL_TEXT_CHARS);
        if let Some(err) = &mut tool.error {
            cap_string_in_place(err, REMOTE_TOOL_TEXT_CHARS);
        }
    }

    zero_remote_only_fields(&mut snapshot);
    snapshot
}

/// Project a patch for the mobile relay path: keeps the conversation delta
/// (messages/deltas/timeline/tools/session/status/steers) and zeroes the
/// heavyweight fields the phone ignores. Sending them as empty is
/// byte-compatible with the mobile reducer, which replaces those fields
/// verbatim; the desktop local bridge uses `lightweight_ui_update` directly
/// and is unaffected.
///
/// `thinking_text` is the worst offender: it is re-sent in full (uncapped) on
/// every patch while a turn streams, and the mobile reducer never applies it.
///
/// `live_turn_changes` carries the IN-FLIGHT turn's file changes (see
/// [`Application::live_turn_file_changes`]); without them the phone's
/// "本轮改动" bar only appears after the turn ends, because `ui.turn_changes`
/// is persisted at turn close. `session_config` is deliberately NOT zeroed:
/// the phone's model/provider switcher renders from its controls (it used to
/// be stripped to `hydrated: false`, which made the picker never appear).
pub fn project_remote_patch(
    mut patch: workspace_model::UiSnapshotPatch,
    live_turn_changes: Option<workspace_model::TurnFileChanges>,
) -> workspace_model::UiSnapshotPatch {
    patch.available_commands = Vec::new();
    patch.agent_plan = Vec::new();
    patch.repository = None;
    patch.inspector_sections = Vec::new();
    patch.session_changes = Vec::new();
    patch.review_changes = Vec::new();
    // Turn changes reach the phone as metadata only (path + line counts) for
    // the mobile "本轮改动" bar — the same projection the Full snapshot path
    // applies. The texts were already stripped at patch build time.
    patch.turn_changes = metadata_only_turn_changes(&patch.turn_changes);
    if let Some(live) = live_turn_changes {
        patch
            .turn_changes
            .retain(|entry| entry.message_id != live.message_id);
        patch.turn_changes.push(live);
    }
    patch.thinking_text = String::new();
    // Usage is a handful of numbers and feeds the phone's session-info sheet;
    // it is not zeroed.
    patch
}

fn zero_remote_only_fields(snapshot: &mut workspace_model::UiSnapshot) {
    // `session_config` stays: the phone's model/provider switcher renders
    // from its controls (stripping it to `hydrated: false` hid the picker).
    snapshot.available_commands = Vec::new();
    snapshot.agent_plan = Vec::new();
    snapshot.inspector_sections = Vec::new();
    snapshot.session_changes = Vec::new();
    snapshot.review_changes = Vec::new();
    // Turn changes reach the phone as metadata only (path + line counts) —
    // exactly what the patches already carry. The mobile "本轮改动" bar
    // renders this summary; the full old/new texts are desktop-only and
    // would dominate the payload.
    snapshot.turn_changes = metadata_only_turn_changes(&snapshot.turn_changes);
    // The phone never renders the thinking body — only the status indicator.
    snapshot.thinking_text = String::new();
    // Usage is small (a context snapshot plus per-model summaries) and feeds
    // the phone's session-info sheet; it is not zeroed.
    snapshot.repository = workspace_model::RepositorySnapshot {
        branch: String::new(),
        head: String::new(),
        changed_files: Vec::new(),
        ahead_count: 0,
        behind_count: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_raw_input_cap_preserves_structured_fields() {
        let raw_input = serde_json::json!({
            "content": "x".repeat(SNAPSHOT_TOOL_RAW_CHARS + 2048),
            "file_path": "openspec/changes/accelerate-pipeline-execution/tasks.md",
            "command": "openspec instructions tasks --change \"accelerate-pipeline-execution\" --json",
            "description": "Generate tasks",
        })
        .to_string();

        let capped = cap_snapshot_tool_raw_input(&raw_input, SNAPSHOT_TOOL_RAW_CHARS);
        assert!(capped.len() < SNAPSHOT_TOOL_RAW_CHARS);

        let parsed: Value = serde_json::from_str(&capped).expect("capped raw input stays JSON");
        assert_eq!(
            parsed.get("file_path").and_then(Value::as_str),
            Some("openspec/changes/accelerate-pipeline-execution/tasks.md")
        );
        assert_eq!(
            parsed.get("command").and_then(Value::as_str),
            Some("openspec instructions tasks --change \"accelerate-pipeline-execution\" --json")
        );
        assert_eq!(
            parsed.get("description").and_then(Value::as_str),
            Some("Generate tasks")
        );
        assert_eq!(
            parsed.get("_truncated").and_then(Value::as_bool),
            Some(true)
        );
        assert!(parsed.get("content").is_none());
    }

    // ── Remote projection ──

    fn remote_fixture(entries: usize) -> workspace_model::UiSnapshot {
        // Build a timeline of `entries` message+tool pairs; only the tail is
        // kept by the projection. Messages/tools carry the referenced ids.
        let mut messages = Vec::new();
        let mut tools = Vec::new();
        let mut timeline = Vec::new();
        for i in 0..entries {
            let message_id = uuid::Uuid::from_u128(1000 + i as u128);
            let tool_id = uuid::Uuid::from_u128(2000 + i as u128);
            messages.push(workspace_model::ChatMessage {
                id: message_id,
                role: workspace_model::MessageRole::Assistant,
                body: "b".repeat(REMOTE_MESSAGE_BODY_CHARS * 2),
                created_at: String::new(),
                is_steer: false,
            });
            tools.push(workspace_model::ToolInvocation {
                id: tool_id,
                call_id: format!("call-{i}"),
                parent_call_id: None,
                name: "shell".into(),
                kind: "shell".into(),
                summary: "s".repeat(REMOTE_TOOL_TEXT_CHARS * 2),
                status: workspace_model::ToolStatus::Succeeded,
                is_subagent: false,
                detail_text: String::new(),
                logs: Vec::new(),
                diff_paths: Vec::new(),
                diff_previews: Vec::new(),
                raw_input: None,
                raw_output: None,
                terminal_output: None,
                error: None,
                permission_options: Vec::new(),
                permission_input: None,
                permission_decision: None,
                can_stop: false,
                stop_kind: None,
                stop_status: None,
            });
            timeline.push(workspace_model::TimelineItem::Message(message_id));
            timeline.push(workspace_model::TimelineItem::Tool(tool_id));
        }
        workspace_model::UiSnapshot {
            revision: 7,
            workspace: workspace_model::WorkspaceDescriptor {
                id: uuid::Uuid::nil(),
                name: "w".into(),
                root: "/w".into(),
                location: workspace_model::WorkspaceLocation::Local,
                kind: workspace_model::WorkspaceKind::Project,
            },
            workspace_connected: true,
            session: workspace_model::SessionSummary {
                id: uuid::Uuid::nil(),
                workspace_id: uuid::Uuid::nil(),
                title: "t".into(),
                model: "m".into(),
                mode: None,
                agent_cli: None,
                status: workspace_model::SessionStatus::Idle,
            },
            session_config: workspace_model::SessionConfigState {
                hydrated: true,
                controls: Vec::new(),
            },
            prompt_capabilities: workspace_model::PromptInputCapabilities::default(),
            image_capabilities: workspace_model::ImageCapabilities::default(),
            available_commands: Vec::new(),
            agent_plan: Vec::new(),
            messages,
            timeline,
            tools,
            repository: workspace_model::RepositorySnapshot {
                branch: "main".into(),
                head: "abc".into(),
                changed_files: Vec::new(),
                ahead_count: 1,
                behind_count: 0,
            },
            inspector_tab: workspace_model::InspectorTab::Activity,
            inspector_sections: Vec::new(),
            session_changes: Vec::new(),
            review_changes: Vec::new(),
            turn_changes: Vec::new(),
            thinking_status: Some(workspace_model::ThinkingStatus::Active),
            thinking_text: "thinking".into(),
            usage: workspace_model::SessionUsageSnapshot::default(),
            pending_steers: Vec::new(),
            history_total: entries as i64,
            history_earliest_seq: Some(1),
        }
    }

    #[test]
    fn remote_projection_windows_timeline_and_referenced_entities() {
        // 150 pairs = 300 timeline entries → keep the last 200 entries, which
        // must cover exactly the last 100 pairs.
        let snapshot = remote_fixture(150);
        let projected = project_remote_snapshot(snapshot);
        assert_eq!(projected.timeline.len(), REMOTE_TIMELINE_WINDOW);
        // Every referenced message/tool survives; the trimmed head is dropped.
        let referenced: HashSet<uuid::Uuid> = projected
            .timeline
            .iter()
            .filter_map(|item| match item {
                workspace_model::TimelineItem::Message(id) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(referenced.len(), projected.messages.len());
        for message in &projected.messages {
            assert!(referenced.contains(&message.id), "message must be referenced");
        }
        // Oldest message pair (ids 1000/2000) fell out of the window.
        let oldest_message = uuid::Uuid::from_u128(1000);
        assert!(
            projected
                .messages
                .iter()
                .all(|m| m.id != oldest_message)
        );
    }

    #[test]
    fn remote_projection_caps_tool_text_and_zeroes_desktop_only_fields() {
        let snapshot = remote_fixture(3);
        let projected = project_remote_snapshot(snapshot);

        assert!(projected
            .tools
            .iter()
            .all(|t| t.summary.len() <= REMOTE_TOOL_TEXT_CHARS + "\n...".len()));

        assert!(projected.session_changes.is_empty());
        assert!(projected.review_changes.is_empty());
        assert!(projected.turn_changes.is_empty());
        assert!(projected.inspector_sections.is_empty());
        assert!(projected.available_commands.is_empty());
        assert!(projected.agent_plan.is_empty());
        assert!(projected.thinking_text.is_empty());
        assert!(projected.repository.changed_files.is_empty());
        assert!(projected.repository.branch.is_empty());
        assert!(projected.session_config.hydrated);
        // Conversation-relevant fields survive.
        assert!(projected.thinking_status.is_some());
        assert_eq!(projected.revision, 7);
        assert_eq!(projected.history_total, 3);
    }

    #[test]
    fn remote_projection_keeps_the_newest_message_body_whole() {
        // Regression: the phone rendered long replies up to exactly
        // `REMOTE_MESSAGE_BODY_CHARS` followed by the cap marker — the tail of
        // the answer never reached the device. Real message from the report:
        // 2138 chars, cut right after "**都改".
        let long_body = "改".repeat(REMOTE_MESSAGE_BODY_CHARS + 90);
        let mut snapshot = remote_fixture(2);
        let newest_id = snapshot.messages.last().unwrap().id;
        snapshot.messages.last_mut().unwrap().body = long_body.clone();

        let projected = project_remote_snapshot(snapshot);
        let newest = projected
            .messages
            .iter()
            .find(|message| message.id == newest_id)
            .expect("the newest message survives the window");

        assert_eq!(
            newest.body, long_body,
            "the newest body must reach the phone intact"
        );
        assert!(!newest.body.contains("\n..."), "no cap marker on the tail");
    }

    #[test]
    fn remote_projection_budget_caps_only_the_oldest_bodies() {
        // Past the budget the projection falls back to the historical head cap,
        // oldest-first: those messages are no longer growing, so truncating
        // them cannot desync the phone's append-only delta chain.
        let chunk = "x".repeat(REMOTE_MESSAGE_BODY_BUDGET / 2);
        let mut snapshot = remote_fixture(3);
        for message in &mut snapshot.messages {
            message.body = chunk.clone();
        }
        let ids: Vec<uuid::Uuid> = snapshot.messages.iter().map(|m| m.id).collect();

        let projected = project_remote_snapshot(snapshot);
        let body_of = |id: uuid::Uuid| {
            projected
                .messages
                .iter()
                .find(|message| message.id == id)
                .expect("message survives the window")
                .body
                .clone()
        };

        // Newest: whole (half the budget).
        assert_eq!(body_of(ids[2]), chunk);
        // Second: fits the remaining half exactly, so still whole.
        assert_eq!(body_of(ids[1]), chunk);
        // Oldest: budget spent, falls back to the head cap + marker.
        let oldest = body_of(ids[0]);
        assert!(
            oldest.starts_with(&"x".repeat(REMOTE_MESSAGE_BODY_CHARS))
                && oldest.ends_with("\n..."),
            "the oldest body must fall back to the head cap: {}",
            oldest.len()
        );
    }

    #[test]
    fn remote_projection_keeps_image_bodies_intact_for_thumbnails() {
        // A user image prompt embeds the attachment thumbnail (a 64x64 canvas
        // PNG, ~2-8KB of base64). Capping the body at 2K truncated it
        // mid-base64, leaving an unterminated markdown image the phone
        // rendered as raw base64 text.
        let image_body = format!(
            "看看这个\n\n![Image: shot.jpg](data:image/png;base64,{} \"file:///tmp/shot.jpg\")",
            "iVBOR".repeat(1600) // 8000 chars of base64
        );
        let mut snapshot = remote_fixture(1);
        snapshot.messages[0].body = image_body.clone();
        let projected = project_remote_snapshot(snapshot);
        assert_eq!(projected.messages.len(), 1);
        assert_eq!(projected.messages[0].body, image_body);
    }

    #[test]
    fn remote_projection_keeps_a_generated_image_body_intact() {
        // The reported bug: a generated image is embedded as the provider's
        // full-size PNG (0.8-2.1MB on disk in real sessions), and the old 16KB
        // image allowance truncated it mid-base64 — an unterminated markdown
        // image, so the phone rendered raw base64 text instead of the picture.
        let png_base64 = "iVBORw0KGgo".repeat(180_000); // ~1.8MB of base64
        let image_body = format!("画好了\n\n![生成的图片](data:image/png;base64,{png_base64})");
        let mut snapshot = remote_fixture(1);
        snapshot.messages[0].body = image_body.clone();

        let projected = project_remote_snapshot(snapshot);
        assert_eq!(projected.messages[0].body, image_body);
        assert!(
            projected.messages[0].body.len() > 16 * 1024,
            "a generated image must not be truncated to the old thumbnail allowance"
        );
    }

    #[test]
    fn remote_projection_omits_images_beyond_the_image_budget_instead_of_truncating() {
        // Three generated images at ~2.5MB of base64 each: the newest two fit
        // the image budget and travel whole, the oldest is dropped with a note —
        // never a half data URL, which the phone would render as base64 text.
        let png_base64 = "A".repeat(2_500_000);
        let image = |label: &str| format!("{label}\n\n![生成的图片](data:image/png;base64,{png_base64})");
        let mut snapshot = remote_fixture(3);
        snapshot.messages[0].body = image("第一张");
        snapshot.messages[1].body = image("第二张");
        snapshot.messages[2].body = image("第三张");

        let projected = project_remote_snapshot(snapshot);
        assert!(projected.messages[2].body.contains("data:image/"), "newest stays whole");
        assert!(projected.messages[1].body.contains("data:image/"), "second stays whole");
        let omitted = &projected.messages[0].body;
        assert!(!omitted.contains("data:image/"), "no half data URL may ship");
        assert!(omitted.contains(REMOTE_IMAGE_OMITTED_NOTE));
        assert!(omitted.starts_with("第一张"), "the prose survives");
    }

    #[test]
    fn remote_projection_still_caps_pathological_image_bodies() {
        // Larger than one image is ever allowed to be: the whole block is
        // dropped rather than truncated mid-base64.
        let image_body = format!(
            "![Image: huge.jpg](data:image/png;base64,{})",
            "A".repeat(REMOTE_IMAGE_BODY_CHARS + 1024)
        );
        let mut snapshot = remote_fixture(1);
        snapshot.messages[0].body = image_body;
        let projected = project_remote_snapshot(snapshot);
        assert_eq!(projected.messages[0].body, REMOTE_IMAGE_OMITTED_NOTE);
    }

    #[test]
    fn remote_projection_keeps_turn_change_metadata_only() {
        // The mobile "本轮改动" bar renders per-file line counts; the full
        // old/new texts stay desktop-only.
        let mut snapshot = remote_fixture(1);
        snapshot.turn_changes = vec![workspace_model::TurnFileChanges {
            message_id: uuid::Uuid::from_u128(1000),
            changes: vec![workspace_model::SessionFileChange {
                path: "src/lib.rs".into(),
                change_type: workspace_model::FileChangeType::Modified,
                old_text: Some("old".repeat(1000)),
                new_text: "new".repeat(1000),
                added_lines: 12,
                removed_lines: 4,
                timestamp: "2026-01-01T00:00:00Z".into(),
            }],
        }];
        let projected = project_remote_snapshot(snapshot);
        assert_eq!(projected.turn_changes.len(), 1);
        let change = &projected.turn_changes[0].changes[0];
        assert_eq!(change.path, "src/lib.rs");
        assert_eq!(change.added_lines, 12);
        assert_eq!(change.removed_lines, 4);
        assert_eq!(change.old_text, None);
        assert_eq!(change.new_text, "");
    }

    #[test]
    fn remote_projection_keeps_pending_permission_tools() {
        let mut snapshot = remote_fixture(2);
        snapshot.timeline.truncate(1); // drop the tool entries from the timeline
        snapshot.messages.retain(|m| {
            snapshot
                .timeline
                .iter()
                .any(|item| matches!(item, workspace_model::TimelineItem::Message(id) if *id == m.id))
        });
        let pending_id = uuid::Uuid::from_u128(9999);
        snapshot.tools.insert(
            0,
            workspace_model::ToolInvocation {
                id: pending_id,
                call_id: "pending-call".into(),
                permission_input: Some(workspace_model::PermissionInputRequest::default()),
                permission_decision: None,
                ..snapshot.tools[0].clone()
            },
        );
        let projected = project_remote_snapshot(snapshot);
        assert!(
            projected.tools.iter().any(|t| t.id == pending_id),
            "pending-permission tool must survive the timeline trim"
        );
    }

    #[test]
    fn remote_patch_projection_zeroes_desktop_only_fields() {
        let patch = workspace_model::UiSnapshotPatch {
            revision: 9,
            base_revision: 8,
            session: workspace_model::SessionSummary {
                id: uuid::Uuid::nil(),
                workspace_id: uuid::Uuid::nil(),
                title: "t".into(),
                model: "m".into(),
                mode: None,
                agent_cli: None,
                status: workspace_model::SessionStatus::Streaming,
            },
            session_config: workspace_model::SessionConfigState {
                hydrated: true,
                controls: Vec::new(),
            },
            prompt_capabilities: workspace_model::PromptInputCapabilities::default(),
            available_commands: Vec::new(),
            agent_plan: Vec::new(),
            messages: Vec::new(),
            message_deltas: Vec::new(),
            timeline_start: 0,
            timeline: Vec::new(),
            tools: Vec::new(),
            repository: Some(workspace_model::RepositorySnapshot {
                branch: "main".into(),
                head: "abc".into(),
                changed_files: Vec::new(),
                ahead_count: 0,
                behind_count: 0,
            }),
            inspector_tab: workspace_model::InspectorTab::Activity,
            inspector_sections: Vec::new(),
            session_changes: Vec::new(),
            review_changes: Vec::new(),
            turn_changes: vec![workspace_model::TurnFileChanges {
                message_id: uuid::Uuid::from_u128(42),
                changes: vec![workspace_model::SessionFileChange {
                    path: "src/main.rs".into(),
                    change_type: workspace_model::FileChangeType::Modified,
                    old_text: Some("old".repeat(500)),
                    new_text: "new".repeat(500),
                    added_lines: 7,
                    removed_lines: 2,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                }],
            }],
            thinking_status: None,
            thinking_text: "lots of reasoning".into(),
            usage: workspace_model::SessionUsageSnapshot {
                context: workspace_model::UsageContextSnapshot {
                    used_tokens: Some(1234),
                    window_tokens: Some(500_000),
                    updated_at: None,
                },
                ..workspace_model::SessionUsageSnapshot::default()
            },
            pending_steers: Vec::new(),
        };
        let projected = project_remote_patch(patch, None);
        assert!(projected.thinking_text.is_empty(), "thinking text is phone-dead weight");
        assert!(projected.repository.is_none());
        assert!(projected.session_changes.is_empty());
        assert!(projected.review_changes.is_empty());
        // Turn changes survive as metadata: counts kept, texts stripped.
        assert_eq!(projected.turn_changes.len(), 1);
        let change = &projected.turn_changes[0].changes[0];
        assert_eq!(change.path, "src/main.rs");
        assert_eq!(change.added_lines, 7);
        assert_eq!(change.removed_lines, 2);
        assert_eq!(change.old_text, None);
        assert_eq!(change.new_text, "");
        // Usage feeds the phone's session-info sheet and is not zeroed.
        assert_eq!(projected.usage.context.used_tokens, Some(1234));
        assert!(projected.session_config.hydrated);
        // Conversation delta fields are untouched.
        assert_eq!(projected.revision, 9);
        assert_eq!(projected.timeline_start, 0);
    }
}

impl Application {
    /// Remote GetState with an incremental-resume short-circuit.
    ///
    /// The phone sends the (session id, revision) it already holds on
    /// reconnect. When both still match the PC's active session and revision,
    /// resyncing would re-serialize the entire (trimmed) snapshot over the
    /// relay for zero information — instead answer [`RemoteGetState::UpToDate`]
    /// and let the phone keep its held state. Any mismatch falls back to a
    /// full remote snapshot.
    pub fn remote_get_state(
        &mut self,
        known: Option<(String, u64)>,
    ) -> Result<crate::RemoteGetState, String> {
        use crate::RemoteGetState;
        self.poll_prompt_progress();
        if let Some((known_session_id, known_revision)) = known {
            if self.ui.session.id.to_string() == known_session_id
                && self.ui.revision == known_revision
            {
                return Ok(RemoteGetState::UpToDate);
            }
        }
        Ok(RemoteGetState::Snapshot(self.remote_ui_snapshot()))
    }

    pub fn lightweight_ui_snapshot(&self) -> workspace_model::UiSnapshot {
        let mut created_change_paths =
            created_change_path_keys(&self.ui.session_changes, &self.ui.workspace.root);
        created_change_paths.extend(created_change_path_keys(
            &self.ui.review_changes,
            &self.ui.workspace.root,
        ));
        workspace_model::UiSnapshot {
            revision: self.ui.revision,
            workspace: self.ui.workspace.clone(),
            workspace_connected: true,
            session: self.ui.session.clone(),
            session_config: self.ui.session_config.clone(),
            prompt_capabilities: self.ui.prompt_capabilities.clone(),
            image_capabilities: self.ui.image_capabilities,
            available_commands: self.ui.available_commands.clone(),
            agent_plan: self.ui.agent_plan.clone(),
            messages: self.ui.messages.clone(),
            timeline: self.ui.timeline.clone(),
            tools: self
                .ui
                .tools
                .iter()
                .map(|tool| {
                    lightweight_tool_invocation(
                        tool,
                        &created_change_paths,
                        &self.ui.workspace.root,
                    )
                })
                .collect(),
            repository: self.ui.repository.clone(),
            inspector_tab: self.ui.inspector_tab.clone(),
            inspector_sections: self.ui.inspector_sections.clone(),
            session_changes: self
                .ui
                .session_changes
                .iter()
                .map(metadata_only_change)
                .collect(),
            review_changes: self
                .ui
                .review_changes
                .iter()
                .map(metadata_only_change)
                .collect(),
            turn_changes: metadata_only_turn_changes(&self.ui.turn_changes),
            thinking_status: self.ui.thinking_status.clone(),
            thinking_text: self.ui.thinking_text.clone(),
            usage: self.ui.usage.clone(),
            pending_steers: self.ui.pending_steers.clone(),
            history_total: self.history_total_count,
            history_earliest_seq: self.history_earliest_seq,
        }
    }

    /// Remote-control variant of [`Application::lightweight_ui_snapshot`].
    ///
    /// The relay path sends this over a mobile WebSocket. A full snapshot can
    /// be multiple megabytes (conversation bodies, tool outputs, repository
    /// diffs), which is enough to break the phone's WS connection before the
    /// response can be processed. The projection trims it to what the phone
    /// actually renders (see [`project_remote_snapshot`]).
    pub fn remote_ui_snapshot(&self) -> workspace_model::UiSnapshot {
        let mut snapshot = project_remote_snapshot(self.lightweight_ui_snapshot());
        // 边跑边看的"本轮改动"：进行中轮次的实时文件改动并入下发（收尾后
        // 持久化条目自然接管，`live_turn_file_changes` 返回 None）。
        if let Some(live) = self.live_turn_file_changes() {
            snapshot
                .turn_changes
                .retain(|entry| entry.message_id != live.message_id);
            snapshot.turn_changes.push(live);
        }
        snapshot
    }

    pub fn lightweight_ui_update(
        &mut self,
        cursor: &mut UiPatchCursor,
    ) -> Option<UiSnapshotUpdate> {
        let same_target = cursor.workspace_id == Some(self.ui.workspace.id)
            && cursor.session_id == Some(self.ui.session.id);

        if same_target && self.ui.revision == cursor.revision {
            return None;
        }

        if cursor.revision == 0 || !same_target {
            let snapshot = self.lightweight_ui_snapshot();
            cursor.reset_from_snapshot(&snapshot);
            self.dirty_tool_call_ids.clear();
            return Some(UiSnapshotUpdate::Full(snapshot));
        }

        let mut messages = Vec::new();
        let mut message_deltas = Vec::new();
        let mut current_message_ids = HashSet::new();
        for message in &self.ui.messages {
            current_message_ids.insert(message.id);
            match cursor.message_bodies.get(&message.id) {
                Some(previous_body) if previous_body == &message.body => {}
                Some(previous_body)
                    if message.body.starts_with(previous_body)
                        && message.body.is_char_boundary(previous_body.len()) =>
                {
                    message_deltas.push(ChatMessageDelta {
                        id: message.id,
                        append: message.body[previous_body.len()..].to_string(),
                        // UTF-16 code units: the frontend compares this against
                        // the JS string `.length` of its local stream-store
                        // body to detect a desynced append-only store.
                        base_len: previous_body.encode_utf16().count() as u64,
                    });
                    cursor
                        .message_bodies
                        .insert(message.id, message.body.clone());
                }
                _ => {
                    messages.push(message.clone());
                    cursor
                        .message_bodies
                        .insert(message.id, message.body.clone());
                }
            }
        }
        cursor
            .message_bodies
            .retain(|message_id, _| current_message_ids.contains(message_id));

        let timeline_start = cursor.timeline_len.min(self.ui.timeline.len());
        let timeline = self.ui.timeline[timeline_start..].to_vec();
        cursor.timeline_len = self.ui.timeline.len();

        let mut tools = Vec::new();
        let mut created_change_paths =
            created_change_path_keys(&self.ui.session_changes, &self.ui.workspace.root);
        created_change_paths.extend(created_change_path_keys(
            &self.ui.review_changes,
            &self.ui.workspace.root,
        ));
        let dirty_tool_call_ids = std::mem::take(&mut self.dirty_tool_call_ids);
        let mut emitted_tool_ids = HashSet::new();
        for call_id in dirty_tool_call_ids {
            if let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == call_id) {
                cursor.known_tool_ids.insert(tool.id);
                emitted_tool_ids.insert(tool.id);
                tools.push(lightweight_tool_invocation(
                    tool,
                    &created_change_paths,
                    &self.ui.workspace.root,
                ));
            }
        }
        for tool in &self.ui.tools {
            if cursor.known_tool_ids.insert(tool.id) && emitted_tool_ids.insert(tool.id) {
                tools.push(lightweight_tool_invocation(
                    tool,
                    &created_change_paths,
                    &self.ui.workspace.root,
                ));
            }
        }
        let current_tool_ids = self
            .ui
            .tools
            .iter()
            .map(|tool| tool.id)
            .collect::<HashSet<_>>();
        cursor
            .known_tool_ids
            .retain(|tool_id| current_tool_ids.contains(tool_id));

        let repository = if cursor.repository.as_ref() == Some(&self.ui.repository) {
            None
        } else {
            let repository = self.ui.repository.clone();
            cursor.repository = Some(repository.clone());
            Some(repository)
        };

        // The patch diffs from the cursor's current revision — stamp it so the
        // frontend can verify continuity (coalesced jumps are self-contained;
        // a mismatch means an emitted patch event was lost).
        let base_revision = cursor.revision;
        cursor.revision = self.ui.revision;
        cursor.workspace_id = Some(self.ui.workspace.id);
        cursor.session_id = Some(self.ui.session.id);

        Some(UiSnapshotUpdate::Patch(UiSnapshotPatch {
            revision: self.ui.revision,
            base_revision,
            session: self.ui.session.clone(),
            session_config: self.ui.session_config.clone(),
            prompt_capabilities: self.ui.prompt_capabilities.clone(),
            available_commands: self.ui.available_commands.clone(),
            agent_plan: self.ui.agent_plan.clone(),
            messages,
            message_deltas,
            timeline_start,
            timeline,
            tools,
            repository,
            inspector_tab: self.ui.inspector_tab.clone(),
            inspector_sections: self.ui.inspector_sections.clone(),
            session_changes: self
                .ui
                .session_changes
                .iter()
                .map(metadata_only_change)
                .collect(),
            review_changes: self
                .ui
                .review_changes
                .iter()
                .map(metadata_only_change)
                .collect(),
            turn_changes: metadata_only_turn_changes(&self.ui.turn_changes),
            thinking_status: self.ui.thinking_status.clone(),
            thinking_text: self.ui.thinking_text.clone(),
            usage: self.ui.usage.clone(),
            pending_steers: self.ui.pending_steers.clone(),
        }))
    }
}
