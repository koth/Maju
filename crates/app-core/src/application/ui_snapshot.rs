use super::Application;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use workspace_model::{
    ChatMessageDelta, DiffLineKind, RepositorySnapshot, SessionFileChange, ToolDiffPreview,
    ToolInvocation, UiSnapshotPatch,
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

fn lightweight_tool_invocation(tool: &ToolInvocation) -> ToolInvocation {
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
    next.diff_previews
        .retain(|preview| !looks_like_bogus_whole_file_preview(preview));
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
}

impl Application {
    pub fn lightweight_ui_snapshot(&self) -> workspace_model::UiSnapshot {
        workspace_model::UiSnapshot {
            revision: self.ui.revision,
            workspace: self.ui.workspace.clone(),
            session: self.ui.session.clone(),
            session_config: self.ui.session_config.clone(),
            prompt_capabilities: self.ui.prompt_capabilities.clone(),
            available_commands: self.ui.available_commands.clone(),
            agent_plan: self.ui.agent_plan.clone(),
            messages: self.ui.messages.clone(),
            timeline: self.ui.timeline.clone(),
            tools: self
                .ui
                .tools
                .iter()
                .map(lightweight_tool_invocation)
                .collect(),
            repository: self.ui.repository.clone(),
            inspector_tab: self.ui.inspector_tab.clone(),
            inspector_sections: self.ui.inspector_sections.clone(),
            session_changes: self
                .ui
                .session_changes
                .iter()
                .map(|change| SessionFileChange {
                    path: change.path.clone(),
                    change_type: change.change_type.clone(),
                    old_text: None,
                    new_text: String::new(),
                    added_lines: change.added_lines,
                    removed_lines: change.removed_lines,
                    timestamp: change.timestamp.clone(),
                })
                .collect(),
            review_changes: self.ui.review_changes.clone(),
            turn_changes: self.ui.turn_changes.clone(),
            thinking_status: self.ui.thinking_status.clone(),
        }
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
        let dirty_tool_call_ids = std::mem::take(&mut self.dirty_tool_call_ids);
        let mut emitted_tool_ids = HashSet::new();
        for call_id in dirty_tool_call_ids {
            if let Some(tool) = self.ui.tools.iter().find(|tool| tool.call_id == call_id) {
                cursor.known_tool_ids.insert(tool.id);
                emitted_tool_ids.insert(tool.id);
                tools.push(lightweight_tool_invocation(tool));
            }
        }
        for tool in &self.ui.tools {
            if cursor.known_tool_ids.insert(tool.id) && emitted_tool_ids.insert(tool.id) {
                tools.push(lightweight_tool_invocation(tool));
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

        cursor.revision = self.ui.revision;
        cursor.workspace_id = Some(self.ui.workspace.id);
        cursor.session_id = Some(self.ui.session.id);

        Some(UiSnapshotUpdate::Patch(UiSnapshotPatch {
            revision: self.ui.revision,
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
                .map(|change| SessionFileChange {
                    path: change.path.clone(),
                    change_type: change.change_type.clone(),
                    old_text: None,
                    new_text: String::new(),
                    added_lines: change.added_lines,
                    removed_lines: change.removed_lines,
                    timestamp: change.timestamp.clone(),
                })
                .collect(),
            review_changes: self.ui.review_changes.clone(),
            turn_changes: self.ui.turn_changes.clone(),
            thinking_status: self.ui.thinking_status.clone(),
        }))
    }
}
