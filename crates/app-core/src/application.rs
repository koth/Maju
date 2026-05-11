use crate::bootstrap::build_initial_ui;
use crate::file_tracker::FileChangeTracker;
use crate::paths::AppPaths;
use crate::reducer::apply_event;
use acp_core::{ClientEvent, PromptTask, SessionConfig, SessionHandle, diff_to_hunks};
use git_service::GitService;
use session_store::SessionStore;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use workspace_model::{
    AgentCliId, ChatMessage, ChatMessageDelta, DiffHunk, DiffLineKind, FileChangeType, MessageRole,
    SessionConfigSource, SessionFileChange, SessionListItem, SessionStatus, TimelineItem,
    ToolDiffPreview, ToolInvocation, ToolLogEntry, ToolStatus, UiSnapshotPatch, UserPromptContent,
};

const AGENT_DEFAULT_MODEL_LABEL: &str = "Agent default";
const RESTORED_INCOMPLETE_TOOL_REASON: &str = "上次会话结束前未完成";
const SNAPSHOT_TOOL_DETAIL_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_RAW_CHARS: usize = 4 * 1024;
const SNAPSHOT_TOOL_OUTPUT_CHARS: usize = 8 * 1024;
const SNAPSHOT_TOOL_LOG_CHARS: usize = 1024;
const SNAPSHOT_TOOL_LOG_ENTRIES: usize = 6;

fn make_log_id() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ts}")
}

struct InFlightPrompt {
    task: PromptTask,
}

pub struct Application {
    pub ui: workspace_model::UiSnapshot,
    session: SessionHandle,
    store: SessionStore,
    app_paths: AppPaths,
    pub agent_command: String,
    acp_port: u16,
    in_flight_prompt: Option<InFlightPrompt>,
    /// Tracks the current timeline sequence counter for SQLite persistence
    seq_counter: i64,
    /// Whether we're waiting to generate a title after the first turn
    needs_title: bool,
    /// Whether the agent has pushed a title via SessionTitleUpdated
    agent_title_received: bool,
    /// When true, discard replay events from session/load until user sends first prompt
    skip_replay: bool,
    pending_model_restore: Option<String>,
    file_tracker: FileChangeTracker,
    dirty_tool_call_ids: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct UiPatchCursor {
    revision: u64,
    workspace_id: Option<uuid::Uuid>,
    session_id: Option<uuid::Uuid>,
    timeline_len: usize,
    message_bodies: HashMap<uuid::Uuid, String>,
    known_tool_ids: HashSet<uuid::Uuid>,
}

pub enum UiSnapshotUpdate {
    Full(workspace_model::UiSnapshot),
    Patch(UiSnapshotPatch),
}

pub fn normalize_tracked_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("//?/")
        .or_else(|| normalized.strip_prefix("//./"))
        .unwrap_or(&normalized)
        .to_string();
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let mut chars: Vec<char> = normalized.chars().collect();
        chars[0] = chars[0].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        normalized
    }
}

pub fn normalize_path_for_storage(path: &str, workspace_root: &Path) -> String {
    let normalized = normalize_tracked_path(path);
    let ws_root = normalize_tracked_path(&workspace_root.display().to_string());
    let ws_prefix = if ws_root.ends_with('/') {
        ws_root
    } else {
        format!("{}/", ws_root)
    };
    normalized
        .strip_prefix(&ws_prefix)
        .unwrap_or(&normalized)
        .to_string()
}

fn current_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn normalize_diff_text_for_session_change(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn sanitize_session_file_changes(changes: &mut Vec<SessionFileChange>) -> bool {
    let original_len = changes.len();
    let mut changed = false;

    for change in changes.iter_mut() {
        let previous_added = change.added_lines;
        let previous_removed = change.removed_lines;
        let normalized_old = change
            .old_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let normalized_new = normalize_diff_text_for_session_change(&change.new_text);
        if change.old_text != normalized_old || change.new_text != normalized_new {
            change.old_text = normalized_old;
            change.new_text = normalized_new;
            changed = true;
        }

        let hunks = diff_to_hunks(change.old_text.as_deref(), &change.new_text);
        change.added_lines = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        change.removed_lines = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        if change.added_lines != previous_added || change.removed_lines != previous_removed {
            changed = true;
        }
    }

    changes.retain(|change| change.added_lines > 0 || change.removed_lines > 0);
    changed || changes.len() != original_len
}

fn tool_diff_hunks(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    diff_to_hunks(previous_session_new_text.or(tool_old_text), tool_new_text)
}

fn edit_input_before_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("before")
        .or_else(|| input.get("old_string"))
        .or_else(|| input.get("oldString"))
        .and_then(|value| value.as_str())
}

fn edit_input_after_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("after")
        .or_else(|| input.get("new_string"))
        .or_else(|| input.get("newString"))
        .and_then(|value| value.as_str())
}

fn tool_event_hint_paths(raw_input: Option<&str>) -> Vec<String> {
    let Some(raw_input) = raw_input else {
        return Vec::new();
    };

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_input) {
        let mut paths = Vec::new();
        collect_path_like_values(&value, &mut paths);
        paths.sort();
        paths.dedup();
        return paths;
    }

    if raw_input.contains('/') || raw_input.contains('\\') {
        vec![raw_input.to_string()]
    } else {
        Vec::new()
    }
}

fn collect_path_like_values(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if (key.contains("path") || key == "file" || key == "cwd" || key.ends_with("file"))
                    && let Some(path) = value.as_str()
                {
                    paths.push(path.to_string());
                    continue;
                }
                collect_path_like_values(value, paths);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_like_values(item, paths);
            }
        }
        _ => {}
    }
}

fn interrupt_incomplete_tools(tools: &mut [ToolInvocation]) -> Vec<String> {
    let mut updated_ids = Vec::new();

    for tool in tools
        .iter_mut()
        .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        tool.status = ToolStatus::Interrupted;
        if tool.summary.trim().is_empty()
            || tool.summary == "等待活动"
            || tool.summary.starts_with("等待权限")
        {
            tool.summary = RESTORED_INCOMPLETE_TOOL_REASON.into();
        }
        if tool.kind == "permission" && tool.permission_decision.is_none() {
            tool.permission_decision = Some("已中断".into());
        }
        if tool.error.is_none() {
            tool.error = Some(RESTORED_INCOMPLETE_TOOL_REASON.into());
        }
        if tool.logs.last().map(|entry| entry.body.as_str())
            != Some(RESTORED_INCOMPLETE_TOOL_REASON)
        {
            tool.logs.push(ToolLogEntry {
                title: "已中断".into(),
                body: RESTORED_INCOMPLETE_TOOL_REASON.into(),
            });
            if tool.logs.len() > 12 {
                let keep_from = tool.logs.len() - 12;
                tool.logs.drain(0..keep_from);
            }
        }
        updated_ids.push(tool.id.to_string());
    }

    updated_ids
}

fn lightweight_tool_invocation(tool: &ToolInvocation) -> ToolInvocation {
    let mut next = tool.clone();
    cap_string_in_place(&mut next.detail_text, SNAPSHOT_TOOL_DETAIL_CHARS);
    next.raw_input = next
        .raw_input
        .as_deref()
        .map(|value| capped_snapshot_string(value, SNAPSHOT_TOOL_RAW_CHARS));
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
    }
}

fn capped_snapshot_string(value: &str, max_chars: usize) -> String {
    let mut output = value.to_string();
    cap_string_in_place(&mut output, max_chars);
    output
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

fn prompt_text(prompt: &[UserPromptContent]) -> Option<String> {
    let text = prompt
        .iter()
        .filter_map(|content| match content {
            UserPromptContent::Text { text } => Some(text.trim()),
            UserPromptContent::Image { .. } | UserPromptContent::File { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if text.is_empty() { None } else { Some(text) }
}

fn prompt_has_image(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::Image { .. }))
}

fn prompt_has_file(prompt: &[UserPromptContent]) -> bool {
    prompt
        .iter()
        .any(|content| matches!(content, UserPromptContent::File { .. }))
}

fn markdown_image_alt(name: Option<&str>) -> String {
    name.unwrap_or("attached image")
        .replace(['\n', '\r', '[', ']'], " ")
        .trim()
        .to_string()
}

fn prompt_display_body(prompt: &[UserPromptContent]) -> String {
    let mut parts = Vec::new();
    if let Some(text) = prompt_text(prompt) {
        parts.push(text);
    }
    parts.extend(prompt.iter().filter_map(|content| match content {
        UserPromptContent::Image {
            name,
            thumbnail_data,
            thumbnail_mime_type,
            ..
        } => {
            let alt = markdown_image_alt(name.as_deref());
            thumbnail_data.as_ref().map_or_else(
                || Some(format!("[Image: {alt}]")),
                |data| {
                    let mime_type = thumbnail_mime_type.as_deref().unwrap_or("image/png");
                    Some(format!("![Image: {alt}](data:{mime_type};base64,{data})"))
                },
            )
        }
        UserPromptContent::File { name, .. } => Some(format!("[File: {name}]")),
        UserPromptContent::Text { .. } => None,
    }));
    parts.join("\n\n")
}

impl Application {
    fn bump_revision(&mut self) {
        self.ui.revision = self.ui.revision.saturating_add(1);
    }

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
            thinking_status: self.ui.thinking_status.clone(),
        }))
    }

    pub fn bootstrap(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::bootstrap_with_app_paths(workspace_root, agent_command, AppPaths::resolve()?)
    }

    pub fn bootstrap_with_app_paths(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let workspace_root = workspace_root.as_ref();
        let agent_command = agent_command.into();
        crate::startup_perf::mark(
            "app/bootstrap/start",
            format!(
                "workspace={} agent_command_len={}",
                workspace_root.display(),
                agent_command.len()
            ),
        );
        crate::startup_perf::measure("app/bootstrap/ensure_dirs", "", || {
            app_paths.ensure_standard_dirs()
        })?;
        let mut ui = crate::startup_perf::measure(
            "app/bootstrap/build_initial_ui",
            workspace_root.display().to_string(),
            || build_initial_ui(workspace_root),
        )?;

        let store = crate::startup_perf::measure(
            "app/bootstrap/session_store_open",
            workspace_root.display().to_string(),
            || SessionStore::open(app_paths.root(), workspace_root),
        )?;

        // Read ACP port from settings.
        let settings = crate::startup_perf::measure("app/bootstrap/load_settings", "", || {
            crate::settings::load_app_settings(&app_paths)
        });
        let acp_port = settings.acp_port;

        let existing_sessions =
            crate::startup_perf::measure("app/bootstrap/list_sessions", "", || {
                store.list_sessions().unwrap_or_default()
            });
        crate::startup_perf::mark(
            "app/bootstrap/list_sessions_count",
            existing_sessions.len().to_string(),
        );
        let most_recent_session = existing_sessions.first();
        let requested_agent_label =
            crate::startup_perf::measure("app/bootstrap/agent_label_for_command", "", || {
                crate::settings::agent_label_for_command(&agent_command)
            });
        let persisted_agent_command = most_recent_session
            .and_then(|session| session.agent_cli.as_deref())
            .filter(|label| *label != requested_agent_label)
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &app_paths)
            });
        let agent_command = persisted_agent_command.unwrap_or(agent_command);

        // Check for existing session and its ACP session ID for --resume
        let resume_session_id = most_recent_session.and_then(|s| s.acp_session_id.clone());

        // If resuming an existing session, skip replay events from session/load
        let skip_replay = resume_session_id.is_some();

        let session = crate::startup_perf::measure(
            "app/bootstrap/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: ui.workspace.root.display().to_string(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: ui.session.model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: crate::settings::agent_env_for_command(&agent_command, &app_paths),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port,
                })
            },
        )?;

        // Try to restore the most recent session, otherwise create a new one
        let (needs_title, seq_counter, pending_model_restore) = match existing_sessions.as_slice() {
            [recent, ..] => {
                // list_sessions orders by updated_at DESC
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) =
                    crate::startup_perf::measure("app/bootstrap/load_session", session_id, || {
                        store.load_session(session_id)
                    })
                {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    let mut tools = tools;
                    let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
                    for tool_id in &interrupted_tool_ids {
                        if let Some(tool) =
                            tools.iter().find(|tool| tool.id.to_string() == *tool_id)
                        {
                            let _ = store.update_tool(
                                tool_id,
                                "Interrupted",
                                tool.raw_output.as_deref(),
                                tool.error.as_deref(),
                            );
                        }
                    }
                    let mut pending_model_restore = None;
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        pending_model_restore = Some(model.clone());
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    // Restore file changes from SQLite
                    ui.session_changes = crate::startup_perf::measure(
                        "app/bootstrap/load_file_changes",
                        session_id,
                        || store.load_file_changes(session_id).unwrap_or_default(),
                    );
                    if sanitize_session_file_changes(&mut ui.session_changes) {
                        let _ = crate::startup_perf::measure(
                            "app/bootstrap/replace_file_changes",
                            session_id,
                            || store.replace_file_changes(session_id, &ui.session_changes),
                        );
                    }
                    let seq =
                        crate::startup_perf::measure("app/bootstrap/next_seq", session_id, || {
                            store.next_seq(session_id).unwrap_or(1)
                        });
                    let needs_title = recent.title == "新会话";
                    (needs_title, seq, pending_model_restore)
                } else {
                    // Failed to load — create new session
                    let session_id = ui.session.id.to_string();
                    crate::startup_perf::measure(
                        "app/bootstrap/create_session_after_load_failed",
                        &session_id,
                        || store.create_session(&session_id, &ui.session.model),
                    )?;
                    (true, 1, None)
                }
            }
            _ => {
                // No sessions exist — create a new one
                let session_id = ui.session.id.to_string();
                crate::startup_perf::measure(
                    "app/bootstrap/create_session_empty",
                    &session_id,
                    || store.create_session(&session_id, &ui.session.model),
                )?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        // Determine which agent CLI this session is using. Preserve the per-session
        // persisted value when reopening, instead of overwriting it with the global
        // settings default.
        let agent_cli_label =
            crate::startup_perf::measure("app/bootstrap/resolve_session_agent_label", "", || {
                store
                    .get_session_agent_cli(&ui.session.id.to_string())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| crate::settings::agent_label_for_command(&agent_command))
            });
        ui.session.agent_cli = Some(agent_cli_label.clone());
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_agent_cli", "", || {
            store.update_session_agent_cli(&ui.session.id.to_string(), &agent_cli_label)
        });
        let _ = crate::startup_perf::measure("app/bootstrap/set_permission_mode", "", || {
            session.set_permission_mode(ui.session.mode.as_deref().unwrap_or("Build"))
        });
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_model_mode", "", || {
            store.update_session_model_mode(
                &ui.session.id.to_string(),
                &ui.session.model,
                ui.session.mode.as_deref(),
            )
        });

        let file_tracker = crate::startup_perf::measure(
            "app/bootstrap/file_tracker_new",
            workspace_root.display().to_string(),
            || FileChangeTracker::new(workspace_root),
        );
        crate::startup_perf::mark("app/bootstrap/end", "");

        Ok(Self {
            ui,
            session,
            store,
            app_paths,
            agent_command,
            acp_port,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            skip_replay,
            pending_model_restore,
            file_tracker,
            dirty_tool_call_ids: HashSet::new(),
        })
    }

    pub fn send_prompt(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;
        let events = self.session.send_prompt(prompt)?;
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.ui.session.status = SessionStatus::Idle;
        Ok(())
    }

    pub fn send_prompt_background(&mut self, prompt: impl Into<String>) -> anyhow::Result<()> {
        self.send_prompt_content_background(vec![UserPromptContent::text(prompt.into())])
    }

    pub fn send_prompt_content_background(
        &mut self,
        prompt: Vec<UserPromptContent>,
    ) -> anyhow::Result<()> {
        if self.in_flight_prompt.is_some() {
            let error = anyhow::anyhow!("提示请求已在运行中");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        let display_body = prompt_display_body(&prompt);
        let title_source = prompt_text(&prompt).unwrap_or_else(|| "图片提示".into());
        if display_body.is_empty() {
            let error = anyhow::anyhow!("提示内容不能为空");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_image(&prompt) && !self.ui.prompt_capabilities.image {
            let error = anyhow::anyhow!("当前智能体不支持图片提示");
            self.push_system_message(error.to_string());
            return Err(error);
        }
        if prompt_has_file(&prompt) && !self.ui.prompt_capabilities.embedded_context {
            let error = anyhow::anyhow!("当前智能体不支持文件附件");
            self.push_system_message(error.to_string());
            return Err(error);
        }

        if !self.session.is_alive() {
            if self.session.last_error().is_none() && self.should_auto_reconnect_after_clean_exit()
            {
                self.reconnect_session().map_err(anyhow::Error::msg)?;
            } else {
                let reason = self
                    .session
                    .last_error()
                    .unwrap_or_else(|| "ACP 子进程意外退出".to_string());
                let error = anyhow::anyhow!(reason);
                self.push_system_message(format!("会话已断开：{error}"));
                return Err(error);
            }
        }

        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::User,
            body: display_body,
        };

        // Persist user message to SQLite
        let seq = self.next_seq();
        let _ = self.store.insert_message(
            &self.ui.session.id.to_string(),
            &message.id.to_string(),
            "User",
            &message.body,
            seq,
        );

        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
        self.ui.agent_plan.clear();
        self.ui.session.status = SessionStatus::Streaming;

        // Step 1: Immediately set a truncated title from user prompt (no delay)
        if self.needs_title && self.ui.session.title == "新会话" {
            let title = extract_title_from_prompt(&title_source);
            self.ui.session.title = title.clone();
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }

        // User is sending a new prompt — drain any buffered replay events
        // from session/load before sending, so they don't mix with real responses.
        if self.skip_replay {
            self.session.drain_events();
            self.skip_replay = false;
        }

        let task = self.session.send_prompt_content_async(prompt)?;
        self.in_flight_prompt = Some(InFlightPrompt { task });
        self.bump_revision();
        Ok(())
    }

    pub fn poll_prompt_progress(&mut self) {
        // Detect subprocess crash even when no prompt is in flight
        if self.in_flight_prompt.is_none()
            && !self.session.is_alive()
            && self.ui.session.status != SessionStatus::Interrupted
        {
            let last_error = self.session.last_error();
            if last_error.is_none() && self.should_auto_reconnect_after_clean_exit() {
                if let Err(error) = self.reconnect_session() {
                    let reason = format!("ACP 子进程退出且重连失败：{error}");
                    self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                        reason: reason.clone(),
                    });
                    self.push_system_message(format!("会话已断开：{}", reason));
                    self.bump_revision();
                }
                return;
            }

            let reason = last_error.unwrap_or_else(|| "ACP 子进程意外退出".to_string());
            self.apply_event_with_dirty_tracking(&ClientEvent::Interrupted {
                reason: reason.clone(),
            });
            self.push_system_message(format!("会话已断开：{}", reason));
            self.bump_revision();
            return;
        }

        let Some(in_flight) = self.in_flight_prompt.as_mut() else {
            let events = self.session.collect_pending_events();
            self.session.update_session_id(&events);
            let has_events = !events.is_empty();
            for event in events {
                self.apply_event_and_restore_model(event);
            }
            if has_events {
                self.bump_revision();
            }
            return;
        };

        let events = match in_flight.task.collect_ready_events(&mut self.session) {
            Ok(events) => events,
            Err(error) => {
                self.ui.session.status = SessionStatus::Interrupted;
                self.ui.agent_plan.clear();
                self.push_system_message(format!(
                    "从 `{}` 读取 ACP 事件失败：{}",
                    self.agent_command, error
                ));
                self.in_flight_prompt = None;
                self.bump_revision();
                return;
            }
        };

        let is_finished = in_flight.task.is_finished();

        // If skip_replay is active, discard all events except SessionStarted and TurnFinished.
        // These are replay events from session/load that we already have in SQLite.
        if self.skip_replay {
            // Only keep SessionStarted (to update the ACP session ID) and check for TurnFinished
            for event in &events {
                if let ClientEvent::SessionStarted { .. } = event {
                    self.session.update_session_id(&[event.clone()]);
                    self.persist_event(event);
                    self.bump_revision();
                }
            }
            if is_finished {
                self.skip_replay = false;
                self.in_flight_prompt = None;
                self.ui.session.status = SessionStatus::Idle;
                self.bump_revision();
            }
            return;
        }

        // Preprocess ToolDiff events: fill in old_text from the correct baseline.
        // For the tool card diff, old_text should be "what was on disk when the tool started"
        // so the card shows what THIS tool changed.
        // For session-level changes, the reducer's upsert_session_change preserves the
        // first-ever baseline separately.
        let workspace_root = self.ui.workspace.root.clone();
        let mut events = events;
        let mut had_file_changes = false;
        let mut batch_file_versions = HashMap::<String, String>::new();

        // Events are collected in batches. Some agents emit ToolStarted and ToolDiff in
        // the same batch after the file has already been written. Start recording before
        // the ToolDiff preprocessing pass so `get_any_baseline_text` can still supply
        // a baseline instead of letting the card diff against an empty file.
        for event in &events {
            if let ClientEvent::ToolStarted { id, raw_input, .. } = event {
                self.file_tracker
                    .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
            }
        }

        for event in events.iter_mut() {
            if let ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
                ..
            } = event
            {
                had_file_changes = true;
                // Normalize path to workspace-relative with forward slashes
                let normalized = normalize_path_for_storage(path, &workspace_root);
                self.file_tracker.add_candidate(id, normalized.clone());
                let abs_path = workspace_root.join(&normalized);
                let old_text_is_missing_or_empty = old_text.as_deref().map_or(true, str::is_empty);
                if old_text_is_missing_or_empty {
                    // 1. For multiple ToolDiffs for the same file in one poll batch,
                    // use the previous diff's new_text. This keeps each ToolCard scoped
                    // to this tool's own edit instead of every card comparing against an
                    // empty/missing base and showing the whole file as added.
                    if let Some(previous_text) = batch_file_versions.get(&normalized) {
                        *old_text = Some(previous_text.clone());
                    }
                    // 2. file_tracker baseline: content at tool-start time (best when available)
                    else if let Some(baseline) =
                        self.file_tracker.get_any_baseline_text(&normalized)
                    {
                        *old_text = Some(baseline.to_string());
                    }
                    // 3. last resort requested by user: read the file directly.
                    else if let Ok(content) = std::fs::read_to_string(&abs_path)
                        && content.as_str() != new_text.as_str()
                    {
                        *old_text = Some(content);
                    }
                }
                batch_file_versions.insert(normalized.clone(), new_text.clone());
                *path = normalized;
            }
        }

        // Process events and track tool lifecycle for file change detection
        let mut ui_changed = !events.is_empty();
        let mut completed_tool_ids = Vec::new();
        for event in &events {
            match event {
                ClientEvent::ToolStarted { id, raw_input, .. } => {
                    self.file_tracker
                        .start_recording(id, tool_event_hint_paths(raw_input.as_deref()));
                }
                ClientEvent::ToolUpdated { id, raw_input, .. } => {
                    for path in tool_event_hint_paths(raw_input.as_deref()) {
                        self.file_tracker.add_candidate(id, path);
                    }
                }
                ClientEvent::ToolCompleted { id, .. } | ClientEvent::ToolFailed { id, .. } => {
                    completed_tool_ids.push(id.clone());
                    let changes = self.file_tracker.finish_recording(id);
                    had_file_changes |= self.apply_tracker_changes(id, changes);
                }
                _ => {}
            }
            self.apply_event_with_dirty_tracking(event);
        }
        self.session.update_session_id(&events);

        // Detect file writes from completed tool calls (CodeBuddy uses terminal commands)
        if !completed_tool_ids.is_empty() {
            had_file_changes |= self.detect_file_writes_from_tools(&completed_tool_ids);
        }

        // Persist session_changes to SQLite after all file-change sources have run.
        if had_file_changes {
            self.persist_file_changes();
        }

        if is_finished {
            if self.ui.session.status == SessionStatus::Streaming {
                self.ui.session.status = SessionStatus::Idle;
                ui_changed = true;
            }

            // Step 2: After first turn, try to refine title from assistant's response
            if self.needs_title && !self.agent_title_received {
                self.needs_title = false;
                self.refine_session_title();
                ui_changed = true;
            }

            self.in_flight_prompt = None;
        }

        if ui_changed || had_file_changes {
            self.bump_revision();
        }
    }

    pub fn has_in_flight_prompt(&self) -> bool {
        self.in_flight_prompt.is_some()
    }

    fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        false
    }

    pub fn cancel_prompt(&mut self) -> Result<(), String> {
        if self.in_flight_prompt.is_none() {
            return Ok(());
        }
        self.session
            .cancel_prompt()
            .map_err(|error| error.to_string())?;
        self.mark_current_turn_cancelled();
        self.bump_revision();
        Ok(())
    }

    fn mark_current_turn_cancelled(&mut self) {
        let session_id = self.ui.session.id.to_string();
        let mut cancelled_tools = Vec::new();
        let mut dirty_tool_call_ids = Vec::new();

        for tool in self
            .ui
            .tools
            .iter_mut()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
        {
            dirty_tool_call_ids.push(tool.call_id.clone());
            tool.status = ToolStatus::Interrupted;
            if tool.summary.trim().is_empty()
                || tool.summary == "等待活动"
                || tool.summary.starts_with("等待权限")
            {
                tool.summary = "已取消".into();
            }
            if tool.kind == "permission" && tool.permission_decision.is_none() {
                tool.permission_decision = Some("已取消".into());
            }
            tool.logs.push(ToolLogEntry {
                title: "已取消".into(),
                body: "客户端发送了 session/cancel 取消当前轮次".into(),
            });
            cancelled_tools.push(tool.clone());
        }
        self.dirty_tool_call_ids.extend(dirty_tool_call_ids);

        for tool in cancelled_tools {
            let seq = self.next_seq();
            let _ = self.store.insert_tool(&session_id, &tool, seq);
        }
    }

    pub fn set_session_config_control(
        &mut self,
        control_id: &str,
        value_id: &str,
    ) -> Result<workspace_model::SessionConfigState, String> {
        if self.in_flight_prompt.is_some() || self.ui.session.status != SessionStatus::Idle {
            return Err("会话控件只能在会话空闲时更改".into());
        }

        let control = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.id == control_id)
            .cloned()
            .ok_or_else(|| format!("未知的会话控件：{control_id}"))?;

        if !control.enabled {
            return Err(format!("会话控件不可用：{}", control.label));
        }
        if !control.choices.iter().any(|choice| choice.id == value_id) {
            return Err(format!("{} 的值未知：{value_id}", control.label));
        }

        let events = match control.source {
            SessionConfigSource::ConfigOption => self
                .session
                .set_config_option(control.id, value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LegacyMode => self
                .session
                .set_mode(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::SessionModel => self
                .session
                .set_model(value_id.to_string())
                .map_err(|error| error.to_string())?,
            SessionConfigSource::LocalMode => {
                self.session
                    .set_permission_mode(value_id)
                    .map_err(|error| error.to_string())?;
                vec![ClientEvent::SessionConfigValueChanged {
                    control_id: control.id,
                    value_id: value_id.to_string(),
                    value_label: control
                        .choices
                        .iter()
                        .find(|choice| choice.id == value_id)
                        .map(|choice| choice.label.clone()),
                }]
            }
        };

        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
        self.persist_session_model_mode();
        self.bump_revision();

        Ok(self.ui.session_config.clone())
    }

    pub fn resolve_tool_permission(
        &mut self,
        request_id: &str,
        option_id: Option<String>,
    ) -> Result<(), String> {
        self.session
            .resolve_permission(request_id, option_id)
            .map_err(|error| error.to_string())
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        self.store.list_sessions().map_err(|e| e.to_string())
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        // Load session data from SQLite
        let (messages, mut tools, timeline) =
            self.store.load_session(id).map_err(|e| e.to_string())?;
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = tools.iter().find(|tool| tool.id.to_string() == *tool_id) {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        let (model, mode) = self
            .store
            .get_session_model_mode(id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| (self.ui.session.model.clone(), self.ui.session.mode.clone()));
        let mode = mode.or_else(|| Some("Build".into()));
        let stored_agent_cli = self.store.get_session_agent_cli(id).unwrap_or(None);
        let session_agent_command = stored_agent_cli
            .as_deref()
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
            })
            .unwrap_or_else(|| self.agent_command.clone());

        // Get the stored ACP session ID for resume
        let resume_acp_id = self.store.get_acp_session_id(id).unwrap_or(None);

        // Start a new ACP session handle
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: session_agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(
                &session_agent_command,
                &self.app_paths,
            ),
            resume_session_id: resume_acp_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));

        // Update UI snapshot
        self.ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        self.ui.session.model = model;
        self.ui.session.mode = mode;
        self.agent_command = session_agent_command;
        self.ui.session.agent_cli = stored_agent_cli.or_else(|| {
            Some(crate::settings::agent_label_for_command(
                &self.agent_command,
            ))
        });
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages = messages;
        self.ui.tools = tools;
        self.ui.timeline = timeline;
        self.ui.session.status = SessionStatus::Idle;
        self.session = session;
        self.in_flight_prompt = None;
        // Load file changes from SQLite for the switched session
        self.ui.session_changes = self.store.load_file_changes(id).unwrap_or_default();
        if sanitize_session_file_changes(&mut self.ui.session_changes) {
            self.persist_file_changes();
        }

        // Compute seq counter from loaded data
        self.seq_counter = self.store.next_seq(id).unwrap_or(1);

        // Load session title
        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            self.ui.session.title = s.title.clone();
        }

        self.needs_title = self.ui.session.title == "新会话";
        self.agent_title_received = false;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    pub fn session_create(&mut self, agent: Option<AgentCliId>) -> Result<(), String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = AGENT_DEFAULT_MODEL_LABEL.to_string();
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let current_agent_command = match agent {
            Some(agent) => crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        };
        self.agent_command = current_agent_command;

        // Start a new ACP session handle (no resume for new session)
        let session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode("Build");

        self.ui.session.id = new_id;
        self.ui.session.title = "新会话".to_string();
        self.ui.session.model = initial_model;
        self.ui.session.mode = Some("Build".into());
        let agent_cli_label = crate::settings::agent_label_for_command(&self.agent_command);
        self.ui.session.agent_cli = Some(agent_cli_label);
        self.ui.session_config = Default::default();
        self.ui.prompt_capabilities = Default::default();
        self.ui.session.status = SessionStatus::Idle;
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        self.ui.messages.clear();
        self.ui.tools.clear();
        self.ui.timeline.clear();
        self.ui.session_changes.clear();
        self.session = session;
        self.in_flight_prompt = None;
        self.seq_counter = 1;
        self.needs_title = true;
        self.agent_title_received = false;
        self.pending_model_restore = None;
        self.persist_session_model_mode();
        let _ = self.store.update_session_agent_cli(
            &self.ui.session.id.to_string(),
            self.ui.session.agent_cli.as_deref().unwrap_or("CodeBuddy"),
        );

        self.bump_revision();
        Ok(())
    }

    pub fn session_delete(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            let replacement_id = self
                .store
                .list_sessions()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|session| session.id != id)
                .map(|session| session.id);

            if let Some(replacement_id) = replacement_id {
                self.session_switch(&replacement_id)?;
            } else {
                self.session_create(None)?;
            }
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        // Try to resume the current ACP session if we have its ID
        let resume_id = if !self.session.id.is_empty() {
            Some(self.session.id.clone())
        } else {
            self.store
                .get_acp_session_id(&self.ui.session.id.to_string())
                .unwrap_or(None)
        };

        let resume_id_for_handle = resume_id.clone();
        let has_resume_id = resume_id_for_handle.is_some();
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: self.ui.workspace.root.display().to_string(),
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: self.agent_command.clone(),
            agent_env: crate::settings::agent_env_for_command(&self.agent_command, &self.app_paths),
            resume_session_id: resume_id,
            log_id: make_log_id(),
            acp_port: self.acp_port,
        })
        .map_err(|e| e.to_string())?;
        if let Some(acp_id) = resume_id_for_handle {
            session.id = acp_id;
        }

        self.session = session;
        self.ui.session.status = SessionStatus::Idle;
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut self.ui.tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = self
                .ui
                .tools
                .iter()
                .find(|tool| tool.id.to_string() == *tool_id)
            {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        self.in_flight_prompt = None;
        self.agent_title_received = false;
        self.skip_replay = has_resume_id;
        self.pending_model_restore = Some(self.ui.session.model.clone());
        self.bump_revision();
        Ok(())
    }

    // ── Title refinement ──

    /// After the first turn completes, try to extract a better title from the
    /// assistant's response. The truncated user prompt is already set as Step 1.
    /// This Step 2 tries to improve it by looking at what the assistant actually did.
    fn refine_session_title(&mut self) {
        // Find first assistant message
        let assistant_body = match self
            .ui
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
        {
            Some(m) => m.body.clone(),
            None => return, // No assistant response yet, keep truncated title
        };

        // Try to extract a meaningful title from the assistant's first sentence.
        // Common patterns: "I'll help you X", "Let me X", "Here's how to X", etc.
        let refined = extract_title_from_response(&assistant_body);
        if let Some(title) = refined {
            self.ui.session.title = title.clone();
            let _ = self
                .store
                .update_session_title(&self.ui.session.id.to_string(), &title);
        }
        // If extraction fails, keep the truncated user prompt title from Step 1
    }

    // ── Internal helpers ──

    fn push_system_message(&mut self, body: impl Into<String>) {
        let message = ChatMessage {
            id: uuid::Uuid::new_v4(),
            role: MessageRole::System,
            body: body.into(),
        };
        self.ui.timeline.push(TimelineItem::Message(message.id));
        self.ui.messages.push(message);
    }

    pub fn refresh_repository(&mut self) {
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
        GitService::stage(&self.ui.workspace.root, paths).map_err(|e| e.to_string())?;
        self.refresh_repository();
        Ok(())
    }

    pub fn record_manual_editor_save(
        &mut self,
        path: &str,
        before_text: Option<String>,
        after_text: String,
    ) {
        let normalized = normalize_tracked_path(path);
        let existing_index = self
            .ui
            .session_changes
            .iter()
            .position(|change| normalize_tracked_path(&change.path) == normalized);
        let base_text = existing_index
            .and_then(|index| self.ui.session_changes[index].old_text.clone())
            .or(before_text);

        let normalized_base_text = base_text
            .as_deref()
            .map(normalize_diff_text_for_session_change);
        let normalized_after_text = normalize_diff_text_for_session_change(&after_text);

        if normalized_base_text.as_deref().unwrap_or_default() == normalized_after_text {
            if let Some(index) = existing_index {
                self.ui.session_changes.remove(index);
                self.persist_file_changes();
                self.bump_revision();
            }
            return;
        }

        let hunks = diff_to_hunks(normalized_base_text.as_deref(), &normalized_after_text);
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();

        if added == 0 && removed == 0 {
            if let Some(index) = existing_index {
                self.ui.session_changes.remove(index);
                self.persist_file_changes();
                self.bump_revision();
            }
            return;
        }

        let change_type = if normalized_base_text.is_none() {
            FileChangeType::Created
        } else {
            FileChangeType::Modified
        };
        let timestamp = current_timestamp();

        if let Some(index) = existing_index {
            let existing = &mut self.ui.session_changes[index];
            existing.old_text = normalized_base_text;
            existing.new_text = normalized_after_text;
            existing.change_type = change_type;
            existing.added_lines = added;
            existing.removed_lines = removed;
            existing.timestamp = timestamp;
        } else {
            self.ui.session_changes.push(SessionFileChange {
                path: normalize_path_for_storage(path, &self.ui.workspace.root),
                change_type,
                old_text: normalized_base_text,
                new_text: normalized_after_text,
                added_lines: added,
                removed_lines: removed,
                timestamp,
            });
        }

        self.persist_file_changes();
        self.bump_revision();
    }

    /// Persist current session_changes to SQLite.
    fn persist_file_changes(&self) {
        let session_id = self.ui.session.id.to_string();
        let _ = self
            .store
            .replace_file_changes(&session_id, &self.ui.session_changes);
    }

    fn next_seq(&mut self) -> i64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    fn persist_event(&mut self, event: &ClientEvent) {
        let session_id = self.ui.session.id.to_string();
        match event {
            ClientEvent::SessionStarted { session_id: acp_id } => {
                // Persist the ACP session ID for --resume on next startup
                let _ = self.store.update_acp_session_id(&session_id, acp_id);
            }
            ClientEvent::MessageChunk { .. } => {}
            ClientEvent::TurnFinished { .. } => {
                // Persist the final assistant message if not already persisted
                let msg_data = self
                    .ui
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::Assistant)
                    .map(|m| (m.id.to_string(), m.body.clone()));

                if let Some((id_str, body)) = msg_data {
                    let seq = self.next_seq();
                    if self
                        .store
                        .insert_message(&session_id, &id_str, "Assistant", &body, seq)
                        .is_err()
                    {
                        let _ = self.store.update_message_body(&id_str, &body);
                    }
                }
                let _ = self.store.update_session_status(&session_id, "Idle");
            }
            ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. } => {
                self.persist_session_model_mode();
            }
            ClientEvent::SessionTitleUpdated { title } => {
                self.agent_title_received = true;
                let _ = self.store.update_session_title(&session_id, title);
            }
            ClientEvent::ToolStarted { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. } => {
                // Find the tool in the UI snapshot and persist its latest display state
                let tool_clone = self
                    .ui
                    .tools
                    .iter()
                    .find(|t| t.id.to_string() == *id || t.call_id == *id)
                    .cloned();

                if let Some(tool) = tool_clone {
                    let seq = self.next_seq();
                    let _ = self.store.insert_tool(&session_id, &tool, seq);
                }
            }
            _ => {}
        }
    }

    fn apply_event_with_dirty_tracking(&mut self, event: &ClientEvent) {
        self.mark_event_tools_dirty(event);
        apply_event(&mut self.ui, event.clone());
        self.persist_event(event);
    }

    fn mark_tool_call_dirty(&mut self, call_id: &str) {
        self.dirty_tool_call_ids.insert(call_id.to_string());
    }

    fn mark_running_tools_dirty(&mut self) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    fn mark_running_child_tools_dirty(
        &mut self,
        parent_call_id: &str,
        except_call_id: Option<&str>,
    ) {
        let dirty = self
            .ui
            .tools
            .iter()
            .filter(|tool| {
                tool.parent_call_id.as_deref() == Some(parent_call_id)
                    && except_call_id != Some(tool.call_id.as_str())
                    && matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
            })
            .map(|tool| tool.call_id.clone())
            .collect::<Vec<_>>();
        self.dirty_tool_call_ids.extend(dirty);
    }

    fn mark_event_tools_dirty(&mut self, event: &ClientEvent) {
        match event {
            ClientEvent::ToolMessageChunk { id, .. }
            | ClientEvent::ToolPermissionRequest { id, .. }
            | ClientEvent::ToolPermissionResolved { id, .. }
            | ClientEvent::ToolProgress { id, .. }
            | ClientEvent::ToolCompleted { id, .. }
            | ClientEvent::ToolFailed { id, .. }
            | ClientEvent::ToolDiff { id, .. } => {
                self.mark_tool_call_dirty(id);
            }
            ClientEvent::ToolStarted { id, parent_id, .. }
            | ClientEvent::ToolUpdated { id, parent_id, .. } => {
                self.mark_tool_call_dirty(id);
                if let Some(parent_id) = parent_id.as_deref() {
                    self.mark_running_child_tools_dirty(parent_id, Some(id));
                }
            }
            ClientEvent::TurnFinished { .. } | ClientEvent::Interrupted { .. } => {
                self.mark_running_tools_dirty();
            }
            ClientEvent::SessionStarted { .. }
            | ClientEvent::ThinkingActivity { .. }
            | ClientEvent::MessageChunk { .. }
            | ClientEvent::SessionConfigUpdated { .. }
            | ClientEvent::PromptCapabilitiesUpdated { .. }
            | ClientEvent::AvailableCommandsUpdated { .. }
            | ClientEvent::SessionTitleUpdated { .. }
            | ClientEvent::SessionConfigValueChanged { .. }
            | ClientEvent::PlanUpdated { .. } => {}
        }
    }

    fn persist_session_model_mode(&self) {
        let _ = self.store.update_session_model_mode(
            &self.ui.session.id.to_string(),
            &self.ui.session.model,
            self.ui.session.mode.as_deref(),
        );
    }

    fn apply_event_and_restore_model(&mut self, event: ClientEvent) {
        let should_restore_model = matches!(event, ClientEvent::SessionConfigUpdated { .. });
        self.mark_event_tools_dirty(&event);
        apply_event(&mut self.ui, event.clone());
        if should_restore_model {
            self.restore_pending_model_selection();
        }
        self.persist_event(&event);
    }

    fn restore_pending_model_selection(&mut self) {
        let Some(saved_model) = self.pending_model_restore.clone() else {
            return;
        };
        let Some(model_control) = self
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == workspace_model::SessionConfigCategory::Model)
        else {
            return;
        };

        if model_control.current_value_id == saved_model
            || model_control.current_value_label == saved_model
        {
            self.pending_model_restore = None;
            return;
        }

        let Some(choice) = model_control
            .choices
            .iter()
            .find(|choice| choice.id == saved_model || choice.label == saved_model)
            .cloned()
        else {
            self.pending_model_restore = None;
            return;
        };

        self.pending_model_restore = None;
        let Ok(events) = self.session.set_model(choice.id) else {
            return;
        };
        for event in events {
            self.apply_event_with_dirty_tracking(&event);
        }
    }

    /// Detect file writes from completed tool calls by examining tool summaries/titles.
    /// Apply verified file changes from the tracker to session state and tool diff previews.
    fn apply_tracker_changes(
        &mut self,
        call_id: &str,
        changes: Vec<crate::file_tracker::VerifiedFileChange>,
    ) -> bool {
        use acp_core::diff_to_hunks;

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
            let can_compute_session_diff =
                effective_old_text.is_some() || change.change_type == FileChangeType::Created;
            let session_hunks = if change.skipped_diff || !can_compute_session_diff {
                Vec::new()
            } else {
                diff_to_hunks(effective_old_text.as_deref(), &change.new_text)
            };
            let exact_edit_hunks = self.exact_edit_hunks_for_tool(call_id, &normalized);
            let existing_tool_hunks = self.existing_tool_diff_hunks(call_id, &normalized);
            let tool_hunks = tool_hunks_for_tracker_update(
                change.skipped_diff,
                exact_edit_hunks,
                existing_tool_hunks,
                previous_session_new_text.as_deref(),
                change.old_text.as_deref(),
                &change.new_text,
                &change.hunks,
            );

            if !change.skipped_diff {
                if effective_old_text.as_deref().unwrap_or_default() == change.new_text {
                    if let Some(index) = existing_index {
                        self.ui.session_changes.remove(index);
                        changed = true;
                    }
                    self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
                    continue;
                }

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

                if let Some(index) = existing_index {
                    let existing = &mut self.ui.session_changes[index];
                    if existing.old_text.is_none() {
                        existing.old_text = effective_old_text.clone();
                    }
                    existing.new_text = change.new_text.clone();
                    existing.change_type = change.change_type.clone();
                    existing.added_lines = added;
                    existing.removed_lines = removed;
                    existing.timestamp = current_timestamp();
                } else {
                    self.ui.session_changes.push(SessionFileChange {
                        path: change.path.clone(),
                        change_type: change.change_type.clone(),
                        old_text: effective_old_text.clone(),
                        new_text: change.new_text.clone(),
                        added_lines: added,
                        removed_lines: removed,
                        timestamp: current_timestamp(),
                    });
                }
                changed = true;
            }

            // Attach only this tool's diff preview, not the cumulative session diff.
            self.attach_tool_diff_preview(call_id, &change.path, &normalized, tool_hunks);
        }
        changed
    }

    fn exact_edit_hunks_for_tool(
        &self,
        call_id: &str,
        normalized_path: &str,
    ) -> Option<Vec<DiffHunk>> {
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
        if normalize_path_for_storage(input_path, &self.ui.workspace.root) != normalized_path {
            return None;
        }
        if looks_like_fragment_to_full_file_text(before, after) {
            return None;
        }
        let hunks = diff_to_hunks(Some(before), after);
        (!hunks.is_empty()).then_some(hunks)
    }

    fn existing_tool_diff_hunks(
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

    fn attach_tool_diff_preview(
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
    fn detect_file_writes_from_tools(&mut self, completed_tool_ids: &[String]) -> bool {
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
        let write_paths: Vec<(String, String)> = self
            .ui
            .tools
            .iter()
            .filter(|t| {
                t.status == ToolStatus::Succeeded && completed_tool_ids.contains(t.call_id.as_str())
            })
            .filter_map(|tool| {
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
                    if !tracked_paths.contains(&normalize_path(&path)) {
                        return Some((tool.call_id.clone(), path));
                    }
                }

                // Check summary for "Editing <path>" pattern
                if tool.summary.starts_with("Editing ") {
                    let path = tool.summary.trim_start_matches("Editing ").to_string();
                    if !tracked_paths.contains(&normalize_path(&path)) {
                        return Some((tool.call_id.clone(), path));
                    }
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
                                if !tracked_paths.contains(&normalize_path(path)) {
                                    return Some((tool.call_id.clone(), path.to_string()));
                                }
                            }
                        }
                    }
                }

                None
            })
            .collect();

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
                let old_text = self.ui.session_changes[index].old_text.clone();
                let previous_session_new_text = self.ui.session_changes[index].new_text.clone();
                let tool_hunks = tool_diff_hunks_for_detected_write(
                    Some(&previous_session_new_text),
                    None,
                    &new_text,
                );
                if old_text.as_deref().unwrap_or_default() == new_text {
                    self.ui.session_changes.remove(index);
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

                let existing = &mut self.ui.session_changes[index];
                existing.new_text = new_text;
                existing.added_lines = added;
                existing.removed_lines = removed;
                existing.timestamp = current_timestamp();
                changed = true;

                self.attach_tool_diff_preview(&call_id, &normalized, &normalized, tool_hunks);
            }
        }

        changed
    }
}

fn tool_diff_hunks_for_tracker_change(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    // Filesystem tracking captures the real on-disk baseline when a tool starts.
    // Prefer that baseline for the ToolCard diff. Using the cumulative session
    // new_text here makes the first tracker-confirmed edit diff against itself,
    // which produces no +/- stats for goose ACP edits.
    if previous_session_new_text.is_none() && tool_old_text.is_none() {
        return Vec::new();
    }
    tool_diff_hunks(None, tool_old_text, tool_new_text).or_else_non_empty(|| {
        tool_diff_hunks(previous_session_new_text, tool_old_text, tool_new_text)
    })
}

fn tool_hunks_for_tracker_update(
    skipped_diff: bool,
    exact_edit_hunks: Option<Vec<DiffHunk>>,
    existing_tool_hunks: Option<Vec<DiffHunk>>,
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
    tracker_hunks: &[DiffHunk],
) -> Vec<DiffHunk> {
    if skipped_diff {
        Vec::new()
    } else if let Some(hunks) = exact_edit_hunks {
        hunks
    } else if let Some(hunks) = existing_tool_hunks
        && !looks_like_whole_file_addition_hunks(&hunks)
    {
        hunks
    } else if previous_session_new_text.is_none() && !tracker_hunks.is_empty() {
        tracker_hunks.to_vec()
    } else {
        tool_diff_hunks_for_tracker_change(previous_session_new_text, tool_old_text, tool_new_text)
    }
}

fn looks_like_fragment_to_full_file_text(old_text: &str, new_text: &str) -> bool {
    let old_lines = old_text.lines().count();
    let new_lines = new_text.lines().count();
    old_lines > 0 && new_lines >= 100 && old_lines * 4 < new_lines
}

fn looks_like_whole_file_addition_hunks(hunks: &[DiffHunk]) -> bool {
    let mut added = 0;
    let mut removed = 0;
    for line in hunks.iter().flat_map(|hunk| &hunk.lines) {
        match line.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    added >= 100 && (removed == 0 || added > removed * 4)
}

fn tool_diff_hunks_for_detected_write(
    previous_session_new_text: Option<&str>,
    tool_old_text: Option<&str>,
    tool_new_text: &str,
) -> Vec<DiffHunk> {
    tool_diff_hunks(previous_session_new_text, tool_old_text, tool_new_text)
        .or_else_non_empty(|| tool_diff_hunks(None, tool_old_text, tool_new_text))
}

trait NonEmptyFallback {
    fn or_else_non_empty<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self;
}

impl<T> NonEmptyFallback for Vec<T> {
    fn or_else_non_empty<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self,
    {
        if self.is_empty() { fallback() } else { self }
    }
}

fn is_file_write_tool_identity(kind: &str, name: &str) -> bool {
    kind_and_name_tokens(kind, name).any(|token| {
        matches!(
            token.as_str(),
            "edit" | "write" | "patch" | "applypatch" | "apply_patch" | "apply-patch"
        )
    })
}

fn kind_and_name_tokens<'a>(kind: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    kind.split(|ch: char| !ch.is_ascii_alphanumeric())
        .chain(name.split(|ch: char| !ch.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
}

/// Extract a concise session title from the user's first prompt.
/// Takes the first line, strips common prefixes, and truncates to 60 chars.
fn extract_title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();

    // Strip common conversational prefixes
    let stripped = first_line
        .strip_prefix("Please ")
        .or_else(|| first_line.strip_prefix("please "))
        .or_else(|| first_line.strip_prefix("Help me "))
        .or_else(|| first_line.strip_prefix("help me "))
        .or_else(|| first_line.strip_prefix("Can you "))
        .or_else(|| first_line.strip_prefix("can you "))
        .or_else(|| first_line.strip_prefix("I want to "))
        .or_else(|| first_line.strip_prefix("I need to "))
        .unwrap_or(first_line)
        .trim();

    let text = if stripped.is_empty() {
        first_line
    } else {
        stripped
    };

    if text.chars().count() <= 60 {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        edit_input_after_text, edit_input_before_text, is_file_write_tool_identity,
        looks_like_fragment_to_full_file_text, looks_like_whole_file_addition_hunks,
        tool_diff_hunks, tool_diff_hunks_for_tracker_change, tool_hunks_for_tracker_update,
    };
    use acp_core::diff_to_hunks;
    use workspace_model::{DiffHunk, DiffLine, DiffLineKind};

    #[test]
    fn tool_diff_uses_previous_session_new_text_for_repeated_file_edits() {
        let hunks = tool_diff_hunks(Some("one\ntwo\n"), Some("one\n"), "one\nthree\n");
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();

        assert_eq!(added, vec!["three"]);
        assert_eq!(removed, vec!["two"]);
    }

    #[test]
    fn tracker_tool_diff_prefers_tool_start_baseline_over_session_new_text() {
        let hunks = tool_diff_hunks_for_tracker_change(
            Some("one\ntwo\n"),
            Some("one\ntwo\n"),
            "one\nthree\n",
        );
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();

        assert_eq!(added, vec!["three"]);
        assert_eq!(removed, vec!["two"]);
    }

    #[test]
    fn tracker_tool_diff_preserves_existing_acp_preview() {
        let existing = vec![DiffHunk {
            heading: "@@ -1,1 +1,1 @@".into(),
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                content: "'react-refresh/only-export-components': [".into(),
            }],
        }];
        let tracker_full_file = vec![DiffHunk {
            heading: "@@ -0,0 +1,27 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "module.exports = {".into(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "  root: true,".into(),
                },
            ],
        }];

        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            Some(existing.clone()),
            None,
            None,
            "module.exports = {\n  root: true,\n",
            &tracker_full_file,
        );

        assert_eq!(hunks, existing);
    }

    #[test]
    fn tracker_tool_diff_prefers_codebuddy_old_new_string_exact_hunks() {
        let input = serde_json::json!({
            "file_path": "D:/work/InfiniteCanvasOL/smokeTest/tests/app-smoke.spec.ts",
            "old_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}",
            "new_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await page.waitForFunction(() => Boolean(document.querySelector('[data-testid=\"prompt-shell\"]')), undefined, { timeout: 10_000 });\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}"
        });
        let exact = diff_to_hunks(
            edit_input_before_text(&input),
            edit_input_after_text(&input).unwrap(),
        );
        let tracker_full_file = vec![DiffHunk {
            heading: "@@ -0,0 +1,847 @@".into(),
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                content: "import { test, expect, Page, TestInfo } from '@playwright/test';".into(),
            }],
        }];

        let hunks = tool_hunks_for_tracker_update(
            false,
            Some(exact.clone()),
            None,
            None,
            None,
            "full file content",
            &tracker_full_file,
        );

        assert_eq!(hunks, exact);
        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        assert_eq!(added, 1);
    }

    #[test]
    fn tracker_tool_diff_rejects_fragment_to_full_file_existing_preview() {
        let bad_existing = vec![DiffHunk {
            heading: "@@ -1,3 +1,901 @@".into(),
            lines: (1..=901)
                .map(|line| DiffLine {
                    kind: DiffLineKind::Added,
                    content: format!("line {line}"),
                })
                .collect(),
        }];
        let full_old = (1..=901)
            .map(|line| {
                if line == 42 {
                    "old target".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let full_new = full_old.replace("old target", "new target\nextra target");

        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            Some(bad_existing),
            None,
            Some(&full_old),
            &full_new,
            &[],
        );

        let added = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        let removed = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn fragment_to_full_file_text_is_not_trusted_as_exact_edit() {
        let old_fragment = "function target() {\n  return 1;\n}\n";
        let new_whole_file = (1..=901)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(looks_like_fragment_to_full_file_text(
            old_fragment,
            &new_whole_file
        ));
    }

    #[test]
    fn whole_file_addition_hunks_are_not_preserved_as_existing_preview() {
        let hunks = vec![DiffHunk {
            heading: "@@ -1,3 +1,901 @@".into(),
            lines: (1..=901)
                .map(|line| DiffLine {
                    kind: DiffLineKind::Added,
                    content: format!("line {line}"),
                })
                .collect(),
        }];

        assert!(looks_like_whole_file_addition_hunks(&hunks));
    }

    #[test]
    fn tracker_tool_diff_without_any_baseline_does_not_render_full_file() {
        let hunks = tool_hunks_for_tracker_update(
            false,
            None,
            None,
            None,
            None,
            "module.exports = {\n  root: true,\n",
            &[],
        );

        assert!(hunks.is_empty());
    }

    #[test]
    fn write_tool_detection_does_not_match_editor_paths() {
        assert!(!is_file_write_tool_identity(
            "read",
            "docs\\editor-subsystem-design.md"
        ));
        assert!(!is_file_write_tool_identity(
            "read",
            "D:/work/kodex/docs/editor-subsystem-design.md"
        ));
        assert!(is_file_write_tool_identity("edit", "docs/architecture.md"));
        assert!(is_file_write_tool_identity("tool", "mcp__codebuddy__write"));
    }
}

/// Try to extract a refined title from the assistant's first response.
/// Returns None if no good title can be extracted (keeps the prompt-based title).
fn extract_title_from_response(response: &str) -> Option<String> {
    // Get the first meaningful line (skip empty lines and markdown headers)
    let first_line = response
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))?;

    // Strip common assistant prefixes to get the action description
    let prefixes = [
        "I'll help you ",
        "I'll ",
        "I will ",
        "Let me ",
        "Sure, I'll ",
        "Sure! I'll ",
        "OK, I'll ",
        "Alright, I'll ",
        "Here's how to ",
        "I can help with ",
        "I can help you ",
        // Chinese prefixes
        "我来帮你",
        "让我",
        "好的，我来",
        "好的，让我",
        "我会",
        "我将",
    ];

    let mut text = first_line;
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
            break;
        }
    }

    let text = text.trim_end_matches('.');
    let text = text.trim();

    // If too short or same as just a function word, not useful
    if text.len() < 5 {
        return None;
    }

    // Capitalize first letter
    let title = if text.starts_with(|c: char| c.is_lowercase()) {
        let mut chars = text.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => return None,
        }
    } else {
        text.to_string()
    };

    // Truncate to 60 chars
    if title.chars().count() <= 60 {
        Some(title)
    } else {
        let truncated: String = title.chars().take(57).collect();
        Some(format!("{truncated}..."))
    }
}
