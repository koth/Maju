use super::*;
use workspace_model::{AutomationScheduleKind, DiffQuality};

fn make_change_set_summary(
    store: &SessionStore,
    id: &str,
    session_id: &str,
    source: ChangeSetSource,
    message_id: Option<Uuid>,
    label: &str,
) -> ChangeSetSummary {
    ChangeSetSummary {
        id: id.to_string(),
        source,
        session_id: Uuid::parse_str(session_id).ok(),
        workspace_root: store.workspace_root().to_string(),
        message_id,
        tool_call_id: None,
        owner_key: Some(format!("test:{id}")),
        label: label.to_string(),
        added_lines: 0,
        removed_lines: 0,
        file_count: 0,
        updated_at: "10".to_string(),
        status: ChangeSetStatus::Complete,
    }
}

fn make_file_record(
    change_set_id: &str,
    path: &str,
    old_text: Option<&str>,
    new_text: Option<&str>,
    added_lines: usize,
    removed_lines: usize,
) -> FileChangeRecord {
    FileChangeRecord {
        change_set_id: change_set_id.to_string(),
        path: path.to_string(),
        change_type: if old_text.is_none() {
            FileChangeType::Created
        } else if new_text.is_none() {
            FileChangeType::Deleted
        } else {
            FileChangeType::Modified
        },
        old_text: old_text.map(str::to_string),
        new_text: new_text.map(str::to_string),
        added_lines,
        removed_lines,
        quality: DiffQuality::Exact,
        updated_at: "20".to_string(),
    }
}

fn make_usage_event(model: &str, total_tokens: u64, timestamp: &str) -> UsageEvent {
    UsageEvent {
        scope: UsageEventScope::TurnDelta,
        model: Some(model.into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some(timestamp.into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(total_tokens),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(total_tokens),
            window_tokens: Some(128000),
            updated_at: Some(timestamp.into()),
        },
        raw_json: None,
    }
}
#[test]
fn test_create_and_list_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store.create_session("s2", "claude-3").unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].title, "新会话");
}

#[test]
fn test_archive_session_hides_from_lists_without_deleting_data() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store.create_session("s2", "claude-3").unwrap();
    store
        .insert_message("s1", "m1", "User", "keep me", 1)
        .unwrap();

    store.archive_session("s1").unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s2");

    let summaries = store.list_session_summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "s2");

    let (messages, _tools, _timeline) = store.load_session("s1").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "keep me");
}

#[test]
fn test_list_restore_and_delete_archived_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace = dir.path().join("kodex");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = SessionStore::open(&app_data, &workspace).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store.create_session("s2", "claude-3").unwrap();
    store
        .insert_message("s1", "m1", "User", "archived", 1)
        .unwrap();
    store.archive_session("s1").unwrap();

    let global = SessionStore::open_global(&app_data).unwrap();
    let archived = global.list_archived_sessions().unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, "s1");
    assert_eq!(archived[0].workspace_root, store.workspace_root());
    assert_eq!(archived[0].message_count, 1);

    global.unarchive_session("s1").unwrap();
    assert!(global.list_archived_sessions().unwrap().is_empty());
    assert_eq!(store.list_sessions().unwrap().len(), 2);

    global.archive_session("s1").unwrap();
    global.delete_archived_session("s1").unwrap();
    assert!(global.get_session_model_mode("s1").unwrap().is_none());

    global.archive_session("s2").unwrap();
    assert_eq!(global.list_archived_sessions().unwrap().len(), 1);
    global.delete_all_archived_sessions().unwrap();
    assert!(global.list_archived_sessions().unwrap().is_empty());
}

#[test]
fn test_archive_workspace_sessions_hides_only_that_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace_a = dir.path().join("a");
    let workspace_b = dir.path().join("b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();

    let store_a = SessionStore::open(&app_data, &workspace_a).unwrap();
    store_a.create_session("a1", "gpt-4").unwrap();
    store_a.create_session("a2", "claude-3").unwrap();
    store_a
        .insert_message("a1", "m1", "User", "archived but retained", 1)
        .unwrap();
    let store_b = SessionStore::open(&app_data, &workspace_b).unwrap();
    store_b.create_session("b1", "gpt-4").unwrap();

    store_a.archive_workspace_sessions().unwrap();

    assert!(store_a.list_sessions().unwrap().is_empty());
    assert_eq!(store_b.list_sessions().unwrap().len(), 1);
    let (messages, _tools, _timeline) = store_a.load_session("a1").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "archived but retained");
}

#[test]
fn test_get_session_workspace_root_reads_across_workspace_stores() {
    // Regression for the remote-switch `session-conflict`: the database is
    // global, so a store opened on workspace B must still report the owning
    // workspace root of a session created in workspace A — the app-core
    // resume path uses this to rebuild the session with its OWN cwd.
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace_a = dir.path().join("a");
    let workspace_b = dir.path().join("b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();

    let store_a = SessionStore::open(&app_data, &workspace_a).unwrap();
    store_a.create_session("a1", "gpt-4").unwrap();

    let store_b = SessionStore::open(&app_data, &workspace_b).unwrap();
    let root = store_b.get_session_workspace_root("a1").unwrap();
    assert_eq!(root.as_deref(), Some(store_a.workspace_root()));

    // Unknown session ids yield None, not an error.
    assert_eq!(store_b.get_session_workspace_root("nope").unwrap(), None);
}

#[test]
fn test_update_session_title() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store.update_session_title("s1", "Fix login bug").unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions[0].title, "Fix login bug");
}

#[test]
fn test_list_session_summaries_omits_message_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store
        .update_session_title("s1", "Lightweight title")
        .unwrap();
    store
        .insert_message("s1", "m1", "User", "hello", 1)
        .unwrap();
    store
        .insert_message("s1", "m2", "Assistant", "hi", 2)
        .unwrap();

    let full = store.list_sessions().unwrap();
    assert_eq!(full[0].message_count, 2);

    let summaries = store.list_session_summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].title, "Lightweight title");
    assert_eq!(summaries[0].message_count, 0);
}

#[test]
fn cap_string_does_not_split_utf8_characters() {
    let value = format!("{}试", "a".repeat(32_767));
    let capped = cap_string(&value, 32_768);

    assert_eq!(capped.len(), 32_767);
    assert!(capped.ends_with('a'));
}

#[test]
fn test_insert_and_load_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store
        .insert_message("s1", "m1", "User", "hello", 1)
        .unwrap();
    store
        .insert_message("s1", "m2", "Assistant", "hi there", 2)
        .unwrap();

    let (messages, _tools, timeline) = store.load_session("s1").unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].body, "hello");
    assert_eq!(messages[1].body, "hi there");
    assert_eq!(timeline.len(), 2);
}

#[test]
fn test_replace_and_load_turn_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let first_message = Uuid::new_v4();
    let second_message = Uuid::new_v4();

    store.create_session("s1", "gpt-4").unwrap();
    store
        .insert_message("s1", &first_message.to_string(), "Assistant", "first", 1)
        .unwrap();
    store
        .insert_message("s1", &second_message.to_string(), "Assistant", "second", 2)
        .unwrap();

    store
        .replace_turn_file_changes(
            "s1",
            &first_message,
            &[SessionFileChange {
                path: "src\\first.ts".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("old".into()),
                new_text: "new".into(),
                added_lines: 1,
                removed_lines: 1,
                timestamp: "1".into(),
            }],
        )
        .unwrap();
    store
        .replace_turn_file_changes(
            "s1",
            &second_message,
            &[SessionFileChange {
                path: "src/second.ts".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("before".into()),
                new_text: "after".into(),
                added_lines: 2,
                removed_lines: 0,
                timestamp: "2".into(),
            }],
        )
        .unwrap();

    let loaded = store.load_turn_file_changes("s1").unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].message_id, first_message);
    assert_eq!(loaded[0].changes[0].path, "src/first.ts");
    assert_eq!(loaded[1].message_id, second_message);
    assert_eq!(loaded[1].changes[0].added_lines, 2);

    store
        .replace_turn_file_changes("s1", &first_message, &[])
        .unwrap();
    let loaded = store.load_turn_file_changes("s1").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].message_id, second_message);
}

#[test]
fn test_insert_and_load_tool_diff_preview() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    let tool_id = Uuid::new_v4();
    let path = std::path::PathBuf::from("d:/work/kodex/AGENTS.md");
    let tool = ToolInvocation {
        id: tool_id,
        call_id: "edit-1".into(),
        parent_call_id: None,
        name: "Edit".into(),
        kind: "edit".into(),
        summary: "Editing AGENTS.md".into(),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: vec![path.clone()],
        diff_previews: vec![ToolDiffPreview {
            path: path.clone(),
            hunks: vec![workspace_model::DiffHunk {
                heading: "ACP diff".into(),
                lines: vec![workspace_model::DiffLine {
                    kind: workspace_model::DiffLineKind::Added,
                    content: "new line".into(),
                }],
            }],
        }],
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
    };

    store.insert_tool("s1", &tool, 1).unwrap();

    let (_messages, tools, timeline) = store.load_session("s1").unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].diff_paths, vec![path.clone()]);
    assert_eq!(tools[0].diff_previews.len(), 1);
    assert_eq!(tools[0].diff_previews[0].path, path);
    assert_eq!(
        tools[0].diff_previews[0].hunks[0].lines[0].content,
        "new line"
    );
    assert!(matches!(timeline[0], TimelineItem::Tool(id) if id == tool_id));
}

#[test]
fn test_delete_session_cascades() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store
        .insert_message("s1", "m1", "User", "hello", 1)
        .unwrap();
    store.delete_session("s1").unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 0);

    let (messages, _tools, _timeline) = store.load_session("s1").unwrap();
    assert_eq!(messages.len(), 0);
}

#[test]
fn test_message_count_in_list() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("s1", "gpt-4").unwrap();
    store.insert_message("s1", "m1", "User", "a", 1).unwrap();
    store
        .insert_message("s1", "m2", "Assistant", "b", 2)
        .unwrap();
    store.insert_message("s1", "m3", "User", "c", 3).unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions[0].message_count, 3);
}

#[test]
fn test_open_uses_home_sessions_dir_and_leaves_workspace_clean() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let store = SessionStore::open(&app_data, &workspace).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    assert!(SessionStore::db_path(&app_data).is_file());
    assert!(!workspace.join(".kodex").exists());
}

#[test]
fn test_list_sessions_filters_by_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace_a = dir.path().join("a");
    let workspace_b = dir.path().join("b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();

    let store_a = SessionStore::open(&app_data, &workspace_a).unwrap();
    store_a.create_session("session-a", "gpt-4").unwrap();
    let store_b = SessionStore::open(&app_data, &workspace_b).unwrap();
    store_b.create_session("session-b", "gpt-4").unwrap();

    let sessions_a = store_a.list_sessions().unwrap();
    let sessions_b = store_b.list_sessions().unwrap();
    assert_eq!(sessions_a.len(), 1);
    assert_eq!(sessions_a[0].id, "session-a");
    assert_eq!(sessions_b.len(), 1);
    assert_eq!(sessions_b[0].id, "session-b");
}

#[test]
fn test_import_legacy_workspace_db_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("home").join(".kodex");
    let workspace = dir.path().join("workspace");
    let legacy_dir = workspace.join(".kodex");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy_db = legacy_dir.join("sessions.db");

    let legacy = Connection::open(&legacy_db).unwrap();
    legacy
        .execute_batch(
            "
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '新会话',
                    model TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'Idle',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    body TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    created_at TEXT NOT NULL
                );
                INSERT INTO sessions (id, title, model, status, created_at, updated_at)
                VALUES ('legacy-session', 'Legacy', 'gpt-4', 'Idle', '1', '2');
                INSERT INTO messages (id, session_id, role, body, seq, created_at)
                VALUES ('legacy-message', 'legacy-session', 'User', 'hello', 1, '2');
                ",
        )
        .unwrap();
    drop(legacy);

    let store = SessionStore::open(&app_data, &workspace).unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "legacy-session");
    assert_eq!(sessions[0].message_count, 1);
    assert!(legacy_db.is_file());

    let reopened = SessionStore::open(&app_data, &workspace).unwrap();
    assert_eq!(reopened.list_sessions().unwrap().len(), 1);
}

#[test]
fn test_upsert_and_load_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    // Insert a file change with base_text
    store
        .upsert_file_change(
            "s1",
            "/src/main.rs",
            "Modified",
            Some("old content"),
            "new content",
            5,
            2,
        )
        .unwrap();

    let changes = store.load_file_changes("s1").unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "/src/main.rs");
    assert_eq!(changes[0].old_text.as_deref(), Some("old content"));
    assert_eq!(changes[0].new_text, "new content");
    assert_eq!(changes[0].added_lines, 5);
    assert_eq!(changes[0].removed_lines, 2);
}

#[test]
fn test_upsert_preserves_base_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    // First insert with base_text
    store
        .upsert_file_change(
            "s1",
            "/src/main.rs",
            "Modified",
            Some("original"),
            "v1",
            1,
            0,
        )
        .unwrap();

    // Second upsert with None base_text — should NOT overwrite existing
    store
        .upsert_file_change("s1", "/src/main.rs", "Modified", None, "v2", 3, 1)
        .unwrap();

    let changes = store.load_file_changes("s1").unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].old_text.as_deref(), Some("original")); // preserved!
    assert_eq!(changes[0].new_text, "v2"); // updated
    assert_eq!(changes[0].added_lines, 3);
}

#[test]
fn test_file_changes_normalize_windows_verbatim_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    store
        .upsert_file_change(
            "s1",
            "d:/work/kodex/AGENTS.md",
            "Modified",
            Some("old"),
            "new",
            1,
            1,
        )
        .unwrap();
    store
        .upsert_file_change(
            "s1",
            "\\\\?\\D:\\work\\kodex\\AGENTS.md",
            "Modified",
            Some("new"),
            "newer",
            2,
            1,
        )
        .unwrap();

    let changes = store.load_file_changes("s1").unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "d:/work/kodex/AGENTS.md");
    assert_eq!(changes[0].old_text.as_deref(), Some("old"));
    assert_eq!(changes[0].new_text, "newer");
}

#[test]
fn test_file_changes_cascade_delete() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    store
        .upsert_file_change("s1", "/a.rs", "Created", None, "content", 10, 0)
        .unwrap();
    store
        .upsert_file_change("s1", "/b.rs", "Modified", Some("old"), "new", 2, 1)
        .unwrap();

    // Delete session — file changes should cascade
    store.delete_session("s1").unwrap();

    let changes = store.load_file_changes("s1").unwrap();
    assert_eq!(changes.len(), 0);
}

#[test]
fn test_replace_file_changes_removes_stale_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    store
        .upsert_file_change("s1", "/a.rs", "Modified", Some("old"), "new", 1, 1)
        .unwrap();
    store
        .upsert_file_change("s1", "/b.rs", "Modified", Some("old"), "new", 1, 1)
        .unwrap();

    store
        .replace_file_changes(
            "s1",
            &[SessionFileChange {
                path: "/b.rs".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("old".into()),
                new_text: "newer".into(),
                added_lines: 2,
                removed_lines: 1,
                timestamp: "now".into(),
            }],
        )
        .unwrap();

    let changes = store.load_file_changes("s1").unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "/b.rs");
    assert_eq!(changes[0].new_text, "newer");

    store.replace_file_changes("s1", &[]).unwrap();
    assert!(store.load_file_changes("s1").unwrap().is_empty());
}

#[test]
fn test_change_set_crud_upsert_cleanup_and_session_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let message_id = Uuid::new_v4();
    let change_set_id = format!("agent-turn:{session_id}:{message_id}");

    store.create_session(&session_id, "gpt-4").unwrap();
    store
        .insert_message(&session_id, &message_id.to_string(), "Assistant", "done", 1)
        .unwrap();

    let summary = make_change_set_summary(
        &store,
        &change_set_id,
        &session_id,
        ChangeSetSource::AgentTurn,
        Some(message_id),
        "本轮对话",
    );
    store
        .replace_change_set(
            &summary,
            &[
                make_file_record(
                    &change_set_id,
                    "src\\main.rs",
                    Some("old"),
                    Some("new"),
                    1,
                    1,
                ),
                make_file_record(&change_set_id, "src/lib.rs", None, Some("created"), 3, 0),
            ],
        )
        .unwrap();

    let summaries = store
        .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, change_set_id);
    assert_eq!(summaries[0].file_count, 2);
    assert_eq!(summaries[0].added_lines, 4);
    assert_eq!(summaries[0].removed_lines, 1);

    let files = store.list_change_set_files(&change_set_id).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].path, "src/main.rs");

    let main_diff = store
        .load_change_set_file_diff(&change_set_id, "src\\main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(main_diff.old_text.as_deref(), Some("old"));
    assert_eq!(main_diff.new_text.as_deref(), Some("new"));

    store
        .upsert_change_set_file(&make_file_record(
            &change_set_id,
            "src/main.rs",
            Some("old"),
            Some("newer"),
            2,
            2,
        ))
        .unwrap();
    let summaries = store
        .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
        .unwrap();
    assert_eq!(summaries[0].file_count, 2);
    assert_eq!(summaries[0].added_lines, 5);
    assert_eq!(summaries[0].removed_lines, 2);

    store.replace_change_set(&summary, &[]).unwrap();
    assert!(
        store
            .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_change_set_files(&change_set_id)
            .unwrap()
            .is_empty()
    );

    store
        .replace_change_set(
            &summary,
            &[make_file_record(
                &change_set_id,
                "src/main.rs",
                Some("base"),
                Some("target"),
                1,
                1,
            )],
        )
        .unwrap();
    store.delete_session(&session_id).unwrap();
    assert!(
        store
            .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_change_set_files(&change_set_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_change_set_snapshots_survive_workspace_drift() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    let store = SessionStore::open(dir.path(), &workspace).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let change_set_id = format!("manual:{session_id}");

    store.create_session(&session_id, "gpt-4").unwrap();
    std::fs::write(workspace.join("src/main.rs"), "current disk").unwrap();

    let summary = make_change_set_summary(
        &store,
        &change_set_id,
        &session_id,
        ChangeSetSource::ManualEdit,
        None,
        "手工修改",
    );
    store
        .replace_change_set(
            &summary,
            &[make_file_record(
                &change_set_id,
                "src/main.rs",
                Some("historical base"),
                Some("historical target"),
                1,
                1,
            )],
        )
        .unwrap();

    std::fs::remove_file(workspace.join("src/main.rs")).unwrap();
    let stored = store
        .load_change_set_file_diff(&change_set_id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(stored.old_text.as_deref(), Some("historical base"));
    assert_eq!(stored.new_text.as_deref(), Some("historical target"));
    assert_eq!(stored.quality, DiffQuality::Exact);
}

#[test]
fn test_usage_events_round_trip_and_summarize() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();

    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("10".into()),
                tokens: UsageTokenBreakdown {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    reasoning_tokens: Some(5),
                    total_tokens: Some(125),
                    ..Default::default()
                },
                context: UsageContextSnapshot {
                    used_tokens: Some(125),
                    window_tokens: Some(128000),
                    updated_at: Some("10".into()),
                },
                raw_json: Some("{\"ok\":true}".into()),
            },
            Some("fallback-model"),
            Some("fallback-agent"),
        )
        .unwrap();

    let snapshot = store.load_session_usage_snapshot("s1").unwrap();
    assert_eq!(snapshot.context.used_tokens, Some(125));
    assert_eq!(snapshot.session_total.total_tokens, Some(125));
    assert_eq!(snapshot.by_model.len(), 1);
    assert_eq!(snapshot.by_model[0].label, "gpt-5.1");

    let by_model = store
        .query_usage_summary(UsageSummaryRequest::default())
        .unwrap();
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].tokens.total_tokens, Some(125));
    assert_eq!(by_model[0].session_count, 1);

    let by_agent = store
        .query_usage_summary(UsageSummaryRequest {
            group_by: UsageSummaryGroupBy::Agent,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_agent.len(), 1);
    assert_eq!(by_agent[0].label, "codex-acp");
}

#[test]
fn test_session_usage_snapshot_aggregates_session_total_and_turn_delta() {
    // codex-acp emits one SessionTotal (cumulative) plus one TurnDelta
    // (per-request) per token-count event. The aggregated snapshot must
    // use the SessionTotal for the running total and the latest TurnDelta
    // for current_turn, ignoring any ContextSnapshot token fields.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();

    let session_total_1 = UsageEvent {
        scope: UsageEventScope::SessionTotal,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("10".into()),
        tokens: UsageTokenBreakdown {
            input_tokens: Some(900),
            output_tokens: Some(200),
            cache_read_tokens: Some(400),
            reasoning_tokens: Some(100),
            total_tokens: Some(1_600),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(1_700),
            window_tokens: Some(200_000),
            updated_at: Some("10".into()),
        },
        raw_json: None,
    };
    let turn_delta_1 = UsageEvent {
        scope: UsageEventScope::TurnDelta,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("11".into()),
        tokens: UsageTokenBreakdown {
            input_tokens: Some(100),
            output_tokens: Some(30),
            total_tokens: Some(180),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(1_700),
            window_tokens: Some(200_000),
            updated_at: Some("11".into()),
        },
        raw_json: None,
    };
    // A ContextSnapshot carrying token fields must NOT pollute the totals.
    let context_snapshot = UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("12".into()),
        tokens: UsageTokenBreakdown {
            input_tokens: Some(9_999),
            output_tokens: Some(9_999),
            total_tokens: Some(9_999),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(1_900),
            window_tokens: Some(200_000),
            updated_at: Some("12".into()),
        },
        raw_json: None,
    };
    store
        .append_usage_event("s1", &session_total_1, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &turn_delta_1, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &context_snapshot, None, None)
        .unwrap();

    let snapshot = store.load_session_usage_snapshot("s1").unwrap();
    assert_eq!(snapshot.session_total.input_tokens, Some(900));
    assert_eq!(snapshot.session_total.output_tokens, Some(200));
    assert_eq!(snapshot.session_total.cache_read_tokens, Some(400));
    assert_eq!(snapshot.session_total.reasoning_tokens, Some(100));
    assert_eq!(snapshot.session_total.total_tokens, Some(1_600));
    assert_eq!(snapshot.current_turn.total_tokens, Some(180));
    assert_eq!(snapshot.context.used_tokens, Some(1_900));
    assert_eq!(snapshot.context.window_tokens, Some(200_000));

    // The per-model summary must reflect the SessionTotal, not the
    // ContextSnapshot noise.
    assert_eq!(snapshot.by_model.len(), 1);
    assert_eq!(snapshot.by_model[0].tokens.total_tokens, Some(1_600));
    assert_eq!(snapshot.by_model[0].context_peak_tokens, Some(1_900));
}

#[test]
fn test_session_usage_snapshot_compatible_with_legacy_context_snapshot_rows() {
    // Pre-fix Kodex sessions only have ContextSnapshot rows with a
    // total_tokens field. When no SessionTotal or TurnDelta events exist,
    // the snapshot must surface the latest ContextSnapshot total as a
    // best-effort session total so historical sessions still display
    // something on reload.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("legacy", "gpt-5.1").unwrap();

    let legacy = UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("1".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(800),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(800),
            window_tokens: Some(128_000),
            updated_at: Some("1".into()),
        },
        raw_json: None,
    };
    let legacy_2 = UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("2".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(1_200),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(1_200),
            window_tokens: Some(128_000),
            updated_at: Some("2".into()),
        },
        raw_json: None,
    };
    store.append_usage_event("legacy", &legacy, None, None).unwrap();
    store.append_usage_event("legacy", &legacy_2, None, None).unwrap();

    let snapshot = store.load_session_usage_snapshot("legacy").unwrap();
    assert_eq!(
        snapshot.session_total.total_tokens,
        Some(1_200),
        "latest legacy context_snapshot total must surface as best-effort session_total"
    );
    assert_eq!(snapshot.context.used_tokens, Some(1_200));
    assert!(snapshot.current_turn.total_tokens.is_none());
}

#[test]
fn test_usage_summary_filters_workspace_and_archived_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("app");
    let workspace_a = dir.path().join("workspace-a");
    let workspace_b = dir.path().join("workspace-b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();

    let store_a = SessionStore::open(&app_data, &workspace_a).unwrap();
    let store_b = SessionStore::open(&app_data, &workspace_b).unwrap();

    store_a.create_session("a1", "gpt-5.1").unwrap();
    store_a
        .append_usage_event(
            "a1",
            &make_usage_event("gpt-5.1", 10, "1751328000"),
            None,
            None,
        )
        .unwrap();
    store_a.create_session("a2", "gpt-5.1").unwrap();
    store_a
        .append_usage_event(
            "a2",
            &make_usage_event("gpt-5.1", 20, "1751414400"),
            None,
            None,
        )
        .unwrap();
    store_a.archive_session("a2").unwrap();

    store_b.create_session("b1", "claude-opus-4.7").unwrap();
    store_b
        .append_usage_event(
            "b1",
            &make_usage_event("claude-opus-4.7", 30, "1751500800"),
            None,
            None,
        )
        .unwrap();

    let current_workspace = store_a
        .query_usage_summary(UsageSummaryRequest::default())
        .unwrap();
    assert_eq!(current_workspace.len(), 1);
    assert_eq!(current_workspace[0].tokens.total_tokens, Some(10));

    let with_archived = store_a
        .query_usage_summary(UsageSummaryRequest {
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(with_archived.len(), 1);
    assert_eq!(with_archived[0].tokens.total_tokens, Some(30));

    let all_workspaces = store_a
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            group_by: UsageSummaryGroupBy::Workspace,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all_workspaces.len(), 2);
    assert!(
        all_workspaces
            .iter()
            .any(
                |row| row.workspace_root.as_deref() == Some(store_a.workspace_root())
                    && row.tokens.total_tokens == Some(10)
            )
    );
    assert!(
        all_workspaces
            .iter()
            .any(
                |row| row.workspace_root.as_deref() == Some(store_b.workspace_root())
                    && row.tokens.total_tokens == Some(30)
            )
    );

    // Date filter is now a numeric comparison: stored `created_at` is cast to
    // INTEGER, and the bound is parsed to epoch seconds. Use ISO bounds
    // (matching what the desktop UI sends) that bracket the middle row.
    let date_filtered = store_a
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: true,
            from: Some("2025-07-02T00:00:00Z".into()),
            to: Some("2025-07-02T23:59:59Z".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(date_filtered.len(), 1);
    assert_eq!(date_filtered[0].tokens.total_tokens, Some(20));
}
#[test]
fn test_legacy_change_sets_wrap_existing_tables() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let message_id = Uuid::new_v4();

    store.create_session("legacy-session", "gpt-4").unwrap();
    store
        .insert_message(
            "legacy-session",
            &message_id.to_string(),
            "Assistant",
            "legacy done",
            1,
        )
        .unwrap();
    store
        .replace_file_changes(
            "legacy-session",
            &[SessionFileChange {
                path: "src\\conversation.rs".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("A".into()),
                new_text: "B".into(),
                added_lines: 1,
                removed_lines: 1,
                timestamp: "100".into(),
            }],
        )
        .unwrap();
    store
        .replace_review_file_changes(
            "legacy-session",
            &[SessionFileChange {
                path: "src/recent.rs".into(),
                change_type: FileChangeType::Modified,
                old_text: None,
                new_text: "recent".into(),
                added_lines: 2,
                removed_lines: 0,
                timestamp: "101".into(),
            }],
        )
        .unwrap();
    store
        .replace_turn_file_changes(
            "legacy-session",
            &message_id,
            &[SessionFileChange {
                path: "src/turn.rs".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("turn base".into()),
                new_text: "turn target".into(),
                added_lines: 3,
                removed_lines: 1,
                timestamp: "102".into(),
            }],
        )
        .unwrap();

    let summaries = store
        .list_change_sets_with_legacy("legacy-session", None)
        .unwrap();
    assert_eq!(summaries.len(), 3);
    assert!(
        summaries
            .iter()
            .any(|summary| summary.source == ChangeSetSource::AgentConversation)
    );
    assert_eq!(
        summaries
            .iter()
            .filter(|summary| summary.source == ChangeSetSource::AgentTurn)
            .count(),
        2
    );

    let turn_id = legacy_agent_turn_id("legacy-session", &message_id);
    let turn_diff = store
        .load_change_set_file_diff_with_legacy(&turn_id, "src\\turn.rs")
        .unwrap()
        .unwrap();
    assert_eq!(turn_diff.path, "src/turn.rs");
    assert_eq!(turn_diff.old_text.as_deref(), Some("turn base"));
    assert_eq!(turn_diff.new_text.as_deref(), Some("turn target"));

    let recent_id = legacy_agent_recent_id("legacy-session");
    let recent_files = store.list_change_set_files_with_legacy(&recent_id).unwrap();
    assert_eq!(recent_files[0].quality, DiffQuality::LegacyIncomplete);
}

/// Build a `ContextSnapshot`-only usage event the way `acp-core` emits for
/// third-party agents that never attach `kodex.ai/usage` meta. The
/// `agent_cli` field on the event itself stays `None` and is later filled by
/// `append_usage_event` from the owning session's `agent_cli`, which is what
/// happens in production for CodeBuddy.
fn make_codebuddy_context_snapshot(timestamp: &str) -> UsageEvent {
    UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: None,
        provider: None,
        agent_cli: None,
        timestamp: Some(timestamp.into()),
        tokens: UsageTokenBreakdown::default(),
        context: UsageContextSnapshot {
            used_tokens: Some(640),
            window_tokens: Some(200_000),
            updated_at: Some(timestamp.into()),
        },
        raw_json: None,
    }
}

/// `load_usage_events_for_summary` filters out usage events whose owning
/// session is a third-party agent that cannot report detailed token usage
/// (CodeBuddy). Codex/Claude rows in the same workspace and date range must
/// remain present and unchanged.
#[test]
fn query_usage_summary_excludes_codebuddy_sessions_by_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("codex-session", "gpt-5.1").unwrap();
    store
        .update_session_agent_cli("codex-session", "codex-acp")
        .unwrap();
    store
        .append_usage_event(
            "codex-session",
            &make_usage_event("gpt-5.1", 500, "1751328000"),
            None,
            None,
        )
        .unwrap();

    store.create_session("codebuddy-session", "codebuddy-model").unwrap();
    store
        .update_session_agent_cli("codebuddy-session", "codebuddy")
        .unwrap();
    store
        .append_usage_event(
            "codebuddy-session",
            &make_codebuddy_context_snapshot("1751414400"),
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: false,
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "CodeBuddy row must be excluded, got: {rows:?}");
    assert_eq!(rows[0].model.as_deref(), Some("gpt-5.1"));
    assert_eq!(rows[0].agent_cli.as_deref(), Some("codex-acp"));
    assert_eq!(rows[0].tokens.total_tokens, Some(500));
    assert!(
        rows.iter().all(|row| row.agent_cli.as_deref() != Some("codebuddy")),
        "no summary row may come from a CodeBuddy session: {rows:?}"
    );

    // Single-session snapshot must still see the CodeBuddy context usage
    // because dock occupancy is read from `load_session_usage_snapshot`,
    // which intentionally does not apply the summary filter.
    let snapshot = store
        .load_session_usage_snapshot("codebuddy-session")
        .unwrap();
    assert_eq!(snapshot.context.used_tokens, Some(640));
    assert_eq!(snapshot.context.window_tokens, Some(200_000));
}

/// `agent_cli` grouping must not produce a `codebuddy` group, even though
/// CodeBuddy sessions have usage events written to `usage_events`.
#[test]
fn query_usage_summary_group_by_agent_omits_codebuddy_group() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store.create_session("c1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("c1", "codex-acp").unwrap();
    store
        .append_usage_event(
            "c1",
            &make_usage_event("gpt-5.1", 100, "1751328000"),
            None,
            None,
        )
        .unwrap();

    store.create_session("b1", "codebuddy-model").unwrap();
    store.update_session_agent_cli("b1", "codebuddy").unwrap();
    store
        .append_usage_event(
            "b1",
            &make_codebuddy_context_snapshot("1751414400"),
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: false,
            group_by: UsageSummaryGroupBy::Agent,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    assert_eq!(rows[0].label, "codex-acp");
    assert_eq!(rows[0].agent_cli.as_deref(), Some("codex-acp"));
    assert!(
        !rows.iter().any(|row| row.label == "codebuddy"),
        "no CodeBuddy group may appear: {rows:?}"
    );
}

/// Even if a usage event's `agent_cli` is written directly (e.g. via raw SQL
/// in a future migration) without going through `update_session_agent_cli`,
/// the summary filter must still exclude it via the
/// `COALESCE(s.agent_cli, u.agent_cli, '')` fallback.
#[test]
fn query_usage_summary_excludes_codebuddy_via_event_agent_cli_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    // Build an event whose `agent_cli` is explicitly set to "codebuddy"
    // (bypassing the session-level fallback path) so we exercise the
    // `u.agent_cli` arm of the COALESCE.
    let event = UsageEvent {
        scope: UsageEventScope::SessionTotal,
        model: Some("codebuddy-model".into()),
        provider: None,
        agent_cli: Some("codebuddy".into()),
        timestamp: Some("1751328000".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(7),
            ..Default::default()
        },
        context: UsageContextSnapshot::default(),
        raw_json: None,
    };

    store.create_session("b1", "codebuddy-model").unwrap();
    // Intentionally do NOT call `update_session_agent_cli("b1", "codebuddy")`
    // so `s.agent_cli` stays NULL in this row; the filter must still exclude
    // the event via the `u.agent_cli` fallback.
    store
        .append_usage_event("b1", &event, None, None)
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();
    assert!(rows.is_empty(), "CodeBuddy event must be excluded, got: {rows:?}");
}

#[test]
fn usage_total_tokens_excludes_cache_to_avoid_double_count() {
    use workspace_model::UsageTokenBreakdown;
    // cache_read (80) is a subset of input (100); including it in the fallback
    // would yield 200 and double-count the same input tokens.
    let tokens = UsageTokenBreakdown {
        input_tokens: Some(100),
        output_tokens: Some(20),
        cache_read_tokens: Some(80),
        cache_write_tokens: Some(0),
        reasoning_tokens: None,
        total_tokens: None,
        ..Default::default()
    };
    assert_eq!(super::usage_total_tokens(&tokens), 120);

    // Authoritative total_tokens wins even when it differs from the sum.
    let with_total = UsageTokenBreakdown {
        total_tokens: Some(150),
        ..tokens
    };
    assert_eq!(super::usage_total_tokens(&with_total), 150);
}

/// P2: daily series must bucket events by calendar day (UTC by default,
/// i.e. when `utc_offset_minutes` is `None`), applying the same
/// SessionTotal-overwrites / TurnDelta-accumulates rules per day, and
/// sum each day's per-model totals into the bucket total.
#[test]
fn usage_daily_series_buckets_by_utc_day_and_applies_session_total_rules() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store
        .update_session_agent_cli("s1", "codex-acp")
        .unwrap();

    // Day A (ts "1751328000" == midnight UTC of day N): an authoritative
    // SessionTotal(1000) followed by a TurnDelta(200) later the same day
    // (ts "1751330000"). Historical summaries prefer request-scoped
    // TurnDeltas, so the day total is the TurnDelta sum (200), not the
    // cumulative SessionTotal.
    let day_a_session_total = UsageEvent {
        scope: UsageEventScope::SessionTotal,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("1751328000".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(1_000),
            ..Default::default()
        },
        context: UsageContextSnapshot::default(),
        raw_json: None,
    };
    let day_a_turn_delta = make_usage_event("gpt-5.1", 200, "1751330000");
    // Day B (ts "1751414400" == midnight UTC of day N+1): a lone TurnDelta
    // (300) with no SessionTotal, so it accumulates into the day total.
    let day_b_turn_delta = make_usage_event("gpt-5.1", 300, "1751414400");

    store
        .append_usage_event("s1", &day_a_session_total, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &day_a_turn_delta, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &day_b_turn_delta, None, None)
        .unwrap();

    let buckets = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(buckets.len(), 2, "two UTC days expected, got: {buckets:?}");
    // BTreeMap keeps days sorted ascending.
    assert!(
        buckets[0].date < buckets[1].date,
        "buckets must be sorted by date ascending: {buckets:?}"
    );
    assert_eq!(
        buckets[0].date.len(),
        10,
        "date must be YYYY-MM-DD: {}",
        buckets[0].date
    );

    // Day A: TurnDelta(200) is preferred over SessionTotal(1000).
    assert_eq!(buckets[0].tokens.total_tokens, Some(200));
    assert_eq!(buckets[0].by_model.len(), 1);
    assert_eq!(buckets[0].by_model[0].tokens.total_tokens, Some(200));
    assert_eq!(buckets[0].by_model[0].event_count, 2);
    // request_count counts one per real model-API request. The ACP mapping
    // splits a single usage meta into SessionTotal + TurnDelta for the SAME
    // request, so only the TurnDelta (the per-request increment) is counted:
    // Day A has 1 SessionTotal + 1 TurnDelta -> request_count = 1.
    assert_eq!(buckets[0].by_model[0].request_count, 1);

    // Day B: lone TurnDelta(300) accumulates.
    assert_eq!(buckets[1].tokens.total_tokens, Some(300));
    assert_eq!(buckets[1].by_model.len(), 1);
    assert_eq!(buckets[1].by_model[0].tokens.total_tokens, Some(300));
    assert_eq!(buckets[1].by_model[0].event_count, 1);
    // Day B has only a TurnDelta (no SessionTotal), so request_count = 1.
    assert_eq!(buckets[1].by_model[0].request_count, 1);
}

/// Regression: daily series must report INCREMENTS, not cumulative
/// SessionTotals. A session with SessionTotal(1000) on day A and
/// SessionTotal(1500) on day B must show day A = 1000 (first day, no
/// baseline) and day B = 500 (1500 - 1000 baseline), NOT day B = 1500.
#[test]
fn usage_daily_series_reports_incremental_not_cumulative() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Day A: SessionTotal 1000 (first day → no baseline → 1000).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1751328000".into()), // day A
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_000),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    // Day B: SessionTotal 1500 (increment = 1500 - 1000 = 500).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1751414400".into()), // day B
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_500),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let buckets = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(buckets.len(), 2, "two days expected, got: {buckets:?}");
    // Day A = 1000 (first day, no baseline to subtract).
    assert_eq!(buckets[0].tokens.total_tokens, Some(1_000));
    // Day B = 500 (1500 - 1000 carry-over baseline from day A).
    assert_eq!(
        buckets[1].tokens.total_tokens,
        Some(500),
        "day B must report the increment (1500 - 1000 = 500), not cumulative 1500"
    );
}

/// Local-timezone bucketing: with `utc_offset_minutes = -480` (Asia/Shanghai,
/// UTC+8), an event at UTC 16:00 of day N-1 (== local 00:00 of day N) and an
/// event at UTC 00:00 of day N (== local 08:00 of day N) must land in the
/// SAME local day, whereas the default UTC bucketing splits them across two
/// days. Regression guard for the settings "每日用量" chart showing usage in
/// the user's local timezone instead of UTC.
#[test]
fn usage_daily_series_bounded_window_keeps_pre_window_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // The first SessionTotal is outside the requested window. The bounded
    // query must still load it as the carry-over baseline; otherwise the
    // in-window 1,500 would be reported as a cumulative total.
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1751328000".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_000),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1751414400".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_500),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let bounded = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            from: Some("1751414400".into()),
            to: Some("1751500800".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(bounded.len(), 1, "only the requested day should render");
    assert_eq!(bounded[0].tokens.total_tokens, Some(500));

    // The unbounded compatibility path still returns both days.
    let all = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].tokens.total_tokens, Some(500));
}

#[test]
fn usage_daily_series_buckets_by_local_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // 1751328000 == 00:00 UTC of day N. Shanghai (UTC+8) local midnight of
    // day N is 8h earlier: 1751328000 - 28800 == 1751299200 (16:00 UTC of
    // day N-1). Both wall-clock instants are the SAME local day (day N). Two
    // distinct models avoid the per-(session,model) "first TurnDelta only"
    // rule so each event contributes its own total.
    let local_midnight = make_usage_event("gpt-5.1", 100, "1751299200");
    let local_morning = make_usage_event("gpt-5.2", 200, "1751328000");
    store
        .append_usage_event("s1", &local_midnight, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &local_morning, None, None)
        .unwrap();

    // Local bucketing (Shanghai, UTC+8): both events share one local day.
    let local_buckets = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            utc_offset_minutes: Some(-480),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        local_buckets.len(),
        1,
        "both events share one local day: {local_buckets:?}"
    );
    assert_eq!(local_buckets[0].tokens.total_tokens, Some(300));

    // Default UTC bucketing (None): the two events straddle a UTC midnight.
    let utc_buckets = store
        .query_usage_daily_series(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        utc_buckets.len(),
        2,
        "UTC bucketing must split across midnight: {utc_buckets:?}"
    );
}

/// Timing rolling-average must use per-field counters. When an event carries
/// `latency_ms` but no `ttft_ms`/`tokens_per_second` (a model call that
/// produced no output tokens), the absent fields must not be divided by an
/// inflated shared counter — each field tracks its own sample count.
/// Regression guard for the settings "LATENCY / TTFT / SPEED" columns.
#[test]
fn usage_summary_timing_averages_use_per_field_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Three timed TurnDelta events for the same model. Event 2 carries only
    // latency (no ttft/tps — e.g. a zero-output turn). With a shared counter
    // the ttft/tps averages would be divided by 3 instead of 2.
    let timed_event = |total: u64,
                       ts: &str,
                       latency: u64,
                       ttft: Option<u64>,
                       tps: Option<f64>| {
        UsageEvent {
            scope: UsageEventScope::TurnDelta,
            model: Some("gpt-5.1".into()),
            provider: Some("openai".into()),
            agent_cli: Some("codex-acp".into()),
            timestamp: Some(ts.into()),
            tokens: UsageTokenBreakdown {
                total_tokens: Some(total),
                latency_ms: Some(latency),
                ttft_ms: ttft,
                tokens_per_second: tps,
                ..Default::default()
            },
            context: UsageContextSnapshot::default(),
            raw_json: None,
        }
    };
    store
        .append_usage_event(
            "s1",
            &timed_event(100, "1751328000", 1000, Some(200), Some(50.0)),
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &timed_event(50, "1751328060", 2000, None, None),
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &timed_event(200, "1751328120", 3000, Some(400), Some(60.0)),
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "one model row: {rows:?}");
    let row = &rows[0];
    // latency present on all 3 events → (1000+2000+3000)/3 = 2000
    assert_eq!(row.avg_latency_ms, Some(2000.0));
    // ttft present on 2 of 3 events → (200+400)/2 = 300
    assert_eq!(row.avg_ttft_ms, Some(300.0));
    // tps present on 2 of 3 events → (50+60)/2 = 55
    assert_eq!(row.avg_tokens_per_second, Some(55.0));
}

/// P5: `request_count` counts one per real model-API request. The ACP
/// mapping layer splits a single `kodex.ai/usage` meta into a
/// `SessionTotal` event plus a `TurnDelta` event describing the SAME
/// request, so counting both double-counts (verified against platform
/// billing: exactly 2× the billed request count). Only the `TurnDelta`
/// scope — the per-request increment — is counted; `SessionTotal` is the
/// cumulative overwrite of the same request and is excluded, as is
/// `ContextSnapshot` occupancy-only telemetry. With 1×SessionTotal +
/// 1×TurnDelta + 3×ContextSnapshot we therefore expect event_count=5 and
/// request_count=1.
#[test]
fn usage_summary_request_count_excludes_context_snapshot_events() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();

    let session_total = UsageEvent {
        scope: UsageEventScope::SessionTotal,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("10".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(1_600),
            ..Default::default()
        },
        context: UsageContextSnapshot::default(),
        raw_json: None,
    };
    let turn_delta = UsageEvent {
        scope: UsageEventScope::TurnDelta,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("11".into()),
        tokens: UsageTokenBreakdown {
            total_tokens: Some(180),
            ..Default::default()
        },
        context: UsageContextSnapshot::default(),
        raw_json: None,
    };
    let context_snapshot = UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: Some("gpt-5.1".into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some("12".into()),
        tokens: UsageTokenBreakdown::default(),
        context: UsageContextSnapshot {
            used_tokens: Some(1_900),
            window_tokens: Some(200_000),
            updated_at: Some("12".into()),
        },
        raw_json: None,
    };

    store
        .append_usage_event("s1", &session_total, None, None)
        .unwrap();
    store
        .append_usage_event("s1", &turn_delta, None, None)
        .unwrap();
    for _ in 0..3 {
        store
            .append_usage_event("s1", &context_snapshot, None, None)
            .unwrap();
    }

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: false,
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(rows[0].event_count, 5, "event_count must count all rows");
    assert_eq!(
        rows[0].request_count, 1,
        "request_count counts one per real request (only the TurnDelta; the \
         SessionTotal describes the same request and is excluded), excluding \
         ContextSnapshot"
    );
}

/// Regression ("today" range + default scope): two codex-acp sessions in the
/// same workspace, both with usage events timestamped "today", must both be
/// counted. The user reported seeing only 1 session despite working in 2+.
#[test]
fn usage_summary_today_range_counts_both_same_day_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    // Two sessions today. Use a fixed "today" epoch second and a from/to
    // window that brackets it (mirroring the UI's "today" date range).
    let today_secs = 1752200000i64;
    let from_secs = today_secs - 3_600; // 1h before
    let to_secs = today_secs + 3_600; // 1h after

    for sid in ["s1", "s2"] {
        store.create_session(sid, "gpt-5.1").unwrap();
        store.update_session_agent_cli(sid, "codex-acp").unwrap();
        store
            .append_usage_event(
                sid,
                &UsageEvent {
                    scope: UsageEventScope::TurnDelta,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(today_secs.to_string()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(50),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some(from_secs.to_string()),
            to: Some(to_secs.to_string()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(
        rows[0].session_count,
        2,
        "both today's sessions must be counted"
    );
    assert_eq!(rows[0].tokens.total_tokens, Some(100));
}

/// Regression ("today" must reflect INCREMENTAL usage, not cumulative
/// SessionTotal). A session created yesterday with SessionTotal(1000) at
/// yesterday-23:00, continuing today with SessionTotal(1500) at today-08:00.
/// The "today" range must report an INCREMENT of 500 (= 1500 − 1000), not the
/// cumulative 1500. Taking the last SessionTotal inside the range as the
/// absolute total wrongly folds yesterday's consumption into today.
#[test]
fn usage_summary_today_range_reports_incremental_not_cumulative() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Yesterday: SessionTotal 1000 (cumulative baseline before today).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752134400".into()), // 2026-07-10 00:00:00 UTC
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_000),
                    input_tokens: Some(800),
                    output_tokens: Some(200),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    // Today: SessionTotal 1500 (cumulative; +500 since yesterday).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752220800".into()), // 2026-07-11 00:00:00 UTC
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_500),
                    input_tokens: Some(1_200),
                    output_tokens: Some(300),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    // "Today" range starts at midnight UTC of today; from is after yesterday.
    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()), // today 00:00 UTC
            to: Some("1752307200".into()),   // tomorrow 00:00 UTC
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(
        rows[0].tokens.total_tokens,
        Some(500),
        "today must report the INCREMENT (1500 - 1000 = 500), not the cumulative 1500"
    );
    assert_eq!(
        rows[0].tokens.input_tokens,
        Some(400),
        "component increments must also be 1200 - 800 = 400"
    );
    assert_eq!(
        rows[0].tokens.output_tokens,
        Some(100),
        "output increment 300 - 200 = 100"
    );
    assert_eq!(rows[0].session_count, 1);
    assert_eq!(
        rows[0].request_count, 0,
        "baseline SessionTotal must not inflate request_count (only TurnDelta is a \
         request; a lone SessionTotal baseline is not a request)"
    );
    assert_eq!(
        rows[0].event_count, 1,
        "baseline SessionTotal must not inflate event_count"
    );
}

/// Regression: carry-over baselines loaded for incremental "today" totals
/// must not surface models that only had activity before the range.
///
/// Scenario:
/// - yesterday: model A SessionTotal (baseline only)
/// - today: model B SessionTotal (actual in-range activity)
///
/// "今天" must list only model B. Before the fix, merge_baseline_events
/// re-injected model A's pre-range SessionTotal into the summary loop, so
/// Settings → 用量 showed unused models with request_count=1.
#[test]
fn usage_summary_today_range_excludes_baseline_only_models() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Yesterday: model A only (carry-over baseline; no in-range activity).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("model-a-yesterday".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752134400".into()), // 2026-07-10 00:00:00 UTC
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_000),
                    input_tokens: Some(800),
                    output_tokens: Some(200),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    // Today: model B only (actual activity inside the selected range).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("model-b-today".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752220800".into()), // 2026-07-11 00:00:00 UTC
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(250),
                    input_tokens: Some(200),
                    output_tokens: Some(50),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()), // today 00:00 UTC
            to: Some("1752307200".into()),   // tomorrow 00:00 UTC
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        rows.len(),
        1,
        "baseline-only models must not appear in the today range, got: {rows:?}"
    );
    assert_eq!(rows[0].model.as_deref(), Some("model-b-today"));
    assert_eq!(rows[0].tokens.total_tokens, Some(250));
    assert_eq!(
        rows[0].request_count, 0,
        "only TurnDelta is a request; a lone in-range SessionTotal is the \
         cumulative overwrite, not a separate request"
    );
    assert_eq!(rows[0].event_count, 1);
    assert_eq!(rows[0].session_count, 1);
}

/// Regression: baseline subtraction must preserve "unknown" token fields.
/// dsh SessionTotals carry component tokens (input/output/cache) but no
/// `total_tokens`. When a session had a pre-range baseline and only
/// SessionTotal activity in range, `sub_optional_u64` used to manufacture
/// `total_tokens = Some(0)` for the missing field; merged into the group row
/// via `add_usage_tokens`, that fabricated zero suppressed the
/// input+output fallback in `usage_total_tokens`, so Settings → 用量 showed
/// TOKENS = 0 while the breakdown chips displayed real millions.
#[test]
fn usage_summary_baseline_subtract_preserves_unknown_total_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "k3").unwrap();
    store.create_session("s2", "k3").unwrap();

    // s1: pre-range SessionTotal (carry-over baseline), then an in-range
    // SessionTotal. dsh shape: component tokens only, no `total_tokens`, and
    // no in-range TurnDelta (the session idled after its last request).
    for (timestamp, input, output) in [
        ("1752134400", 1_000, 100), // 2026-07-10 — baseline
        ("1752220800", 1_500, 150), // 2026-07-11 — in range
    ] {
        store
            .append_usage_event(
                "s1",
                &UsageEvent {
                    scope: UsageEventScope::SessionTotal,
                    model: Some("k3".into()),
                    provider: None,
                    agent_cli: Some("DeepSeek Harness".into()),
                    timestamp: Some(timestamp.into()),
                    tokens: UsageTokenBreakdown {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }
    // s2: one real request in range (TurnDelta, also without total_tokens).
    store
        .append_usage_event(
            "s2",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("k3".into()),
                provider: None,
                agent_cli: Some("DeepSeek Harness".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    input_tokens: Some(500),
                    output_tokens: Some(50),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()),
            to: Some("1752307200".into()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(rows.len(), 1, "single k3 row expected, got: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.request_count, 1);
    // (1500 - 1000) + 500 = 1000 input; (150 - 100) + 50 = 100 output.
    assert_eq!(row.tokens.input_tokens, Some(1_000));
    assert_eq!(row.tokens.output_tokens, Some(100));
    assert_eq!(
        row.tokens.total_tokens, None,
        "a field no event reported must stay unknown, not materialize as Some(0)"
    );
    assert_eq!(
        usage_total_tokens(&row.tokens),
        1_100,
        "with total unknown, the effective total falls back to input + output"
    );
}

/// Regression: the model-grouped view must never label a row with the agent
/// name. dsh usage events persist with `model = NULL` when they arrive before
/// the session model is known; the summary used to fall back to the agent
/// label, surfacing "DeepSeek Harness" (an agent) as a model name.
#[test]
fn usage_summary_model_group_does_not_label_rows_with_agent_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "").unwrap();

    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: None,
                provider: None,
                agent_cli: Some("DeepSeek Harness".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    input_tokens: Some(100),
                    output_tokens: Some(10),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "Unknown model");
    assert_eq!(rows[0].model, None);
    assert_eq!(rows[0].agent_cli.as_deref(), Some("DeepSeek Harness"));
}

/// Regression: rows with zero requests, zero tokens and no context/timing
/// signal are noise. Historically these came from dsh's session-start
/// all-zero `tokenUsage` projection; existing databases keep those rows, so
/// the aggregate filters them at read time.
#[test]
fn usage_summary_drops_zero_signal_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "").unwrap();
    store.create_session("s2", "k3").unwrap();

    // s1: all-zero SessionTotal (session-start projection, model unknown).
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: None,
                provider: None,
                agent_cli: Some("DeepSeek Harness".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    // s2: real usage.
    store
        .append_usage_event(
            "s2",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("k3".into()),
                provider: None,
                agent_cli: Some("DeepSeek Harness".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    input_tokens: Some(100),
                    output_tokens: Some(10),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        rows.len(),
        1,
        "the zero-signal row must be dropped, got: {rows:?}"
    );
    assert_eq!(rows[0].model.as_deref(), Some("k3"));
}

/// Regression: when a session switches models mid-day, each model's token
/// total must come from its own in-range TurnDeltas. Preferring SessionTotal
/// would dump the whole-session cumulative total onto whichever model was
/// current when the latest SessionTotal arrived, so model B would steal model
/// A's historical consumption under "今天".
#[test]
fn usage_summary_today_prefers_turn_deltas_after_model_switch() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "model-a").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Today morning: two requests on model A.
    // codex-acp emits SessionTotal (session-wide cumulative) + TurnDelta
    // (request-scoped) for every token-count frame.
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("model-a".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(100),
                    input_tokens: Some(80),
                    output_tokens: Some(20),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("model-a".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752220800".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(100),
                    input_tokens: Some(80),
                    output_tokens: Some(20),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("model-a".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752224400".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(250),
                    input_tokens: Some(200),
                    output_tokens: Some(50),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("model-a".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752224400".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(150),
                    input_tokens: Some(120),
                    output_tokens: Some(30),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    // Today afternoon: switch to model B for one request.
    // SessionTotal is still the whole-session cumulative (250 + 80 = 330),
    // but it is stamped with model B (the current model). The old logic would
    // therefore report model B = 330 and model A = 0/250 incorrectly.
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("model-b".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752231600".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(330),
                    input_tokens: Some(260),
                    output_tokens: Some(70),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::TurnDelta,
                model: Some("model-b".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752231600".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(80),
                    input_tokens: Some(60),
                    output_tokens: Some(20),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    let mut rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()),
            to: Some("1752307200".into()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    rows.sort_by(|a, b| a.model.cmp(&b.model));

    assert_eq!(rows.len(), 2, "both models with real requests must appear: {rows:?}");
    assert_eq!(rows[0].model.as_deref(), Some("model-a"));
    assert_eq!(
        rows[0].tokens.total_tokens,
        Some(250),
        "model A tokens must be 100 + 150 TurnDeltas, not the later SessionTotal"
    );
    assert_eq!(rows[0].tokens.input_tokens, Some(200));
    assert_eq!(rows[0].tokens.output_tokens, Some(50));
    assert_eq!(rows[1].model.as_deref(), Some("model-b"));
    assert_eq!(
        rows[1].tokens.total_tokens,
        Some(80),
        "model B tokens must be its own TurnDelta, not the whole-session SessionTotal 330"
    );
    assert_eq!(rows[1].tokens.input_tokens, Some(60));
    assert_eq!(rows[1].tokens.output_tokens, Some(20));
}

/// Regression (user report): model A used earlier today was visible in the
/// "今天" per-model table; after opening a NEW session and using model B,
/// model A disappeared from the table. Opening a second session must NOT
/// remove the first session/model's row, because each session's baseline and
/// in-range TurnDeltas are computed independently per (session, model).
#[test]
fn usage_summary_today_keeps_prior_session_model_after_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    // Session 1, model A: started yesterday, used today morning.
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();
    // Yesterday baseline: SessionTotal(1000) + TurnDelta(1000), stamped A.
    for (scope, ts) in [
        (UsageEventScope::SessionTotal, "1752134400"),
        (UsageEventScope::TurnDelta, "1752134400"),
    ] {
        store
            .append_usage_event(
                "s1",
                &UsageEvent {
                    scope,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(ts.into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(1_000),
                        input_tokens: Some(800),
                        output_tokens: Some(200),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }
    // Today: SessionTotal(1500) + TurnDelta(500), stamped A.
    for (scope, total, ts) in [
        (UsageEventScope::SessionTotal, 1_500, "1752220800"),
        (UsageEventScope::TurnDelta, 500, "1752220800"),
    ] {
        store
            .append_usage_event(
                "s1",
                &UsageEvent {
                    scope,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(ts.into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(total),
                        input_tokens: Some(total - 100),
                        output_tokens: Some(100),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    // First check (only session 1 exists): model A must be visible today.
    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()),
            to: Some("1752307200".into()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "session 1 model A visible: {rows:?}");
    assert_eq!(rows[0].model.as_deref(), Some("gpt-5.1"));
    assert_eq!(rows[0].tokens.total_tokens, Some(500));

    // Now open a NEW session 2 with model B, used today afternoon.
    store.create_session("s2", "claude-sonnet").unwrap();
    store.update_session_agent_cli("s2", "codex-acp").unwrap();
    for (scope, total, ts) in [
        (UsageEventScope::SessionTotal, 80, "1752231600"),
        (UsageEventScope::TurnDelta, 80, "1752231600"),
    ] {
        store
            .append_usage_event(
                "s2",
                &UsageEvent {
                    scope,
                    model: Some("claude-sonnet".into()),
                    provider: Some("anthropic".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(ts.into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(total),
                        input_tokens: Some(60),
                        output_tokens: Some(20),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    // Re-check today: BOTH models must still be present.
    let mut rows = store
        .query_usage_summary(UsageSummaryRequest {
            from: Some("1752220800".into()),
            to: Some("1752307200".into()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    rows.sort_by(|a, b| a.model.cmp(&b.model));
    assert_eq!(
        rows.len(),
        2,
        "both sessions' models must remain visible after opening session 2: {rows:?}"
    );
    assert_eq!(rows[0].model.as_deref(), Some("claude-sonnet"));
    assert_eq!(rows[0].tokens.total_tokens, Some(80));
    assert_eq!(rows[1].model.as_deref(), Some("gpt-5.1"));
    assert_eq!(
        rows[1].tokens.total_tokens,
        Some(500),
        "model A's today total must be unchanged after session 2 was added"
    );
}

/// `query_usage_request_count` counts one per real model-API request in
/// range — i.e. only `TurnDelta` events (the per-request increment). It
/// excludes the pre-range `SessionTotal` baseline (which
/// `query_usage_summary` merges in via `merge_baseline_events` and would
/// otherwise inflate the count), the in-range `SessionTotal` (cumulative
/// overwrite of the same request, not a separate request), and
/// `ContextSnapshot` occupancy-only telemetry.
#[test]
fn query_usage_request_count_excludes_baseline_and_context_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-5.1").unwrap();
    store.update_session_agent_cli("s1", "codex-acp").unwrap();

    // Pre-range carry-over baseline: a SessionTotal strictly before `from`.
    store
        .append_usage_event(
            "s1",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("1752134400".into()), // 2026-07-10 00:00:00 UTC
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(1_000),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();

    // In-range: 2×TurnDelta + 1×SessionTotal + 1×ContextSnapshot.
    let in_range_base = 1_752_220_800_i64; // 2026-07-11 00:00:00 UTC
    for (offset, scope) in [
        (0, UsageEventScope::TurnDelta),
        (1, UsageEventScope::TurnDelta),
        (2, UsageEventScope::SessionTotal),
        (3, UsageEventScope::ContextSnapshot),
    ] {
        let tokens = if matches!(scope, UsageEventScope::ContextSnapshot) {
            UsageTokenBreakdown::default()
        } else {
            UsageTokenBreakdown {
                total_tokens: Some(100),
                ..Default::default()
            }
        };
        store
            .append_usage_event(
                "s1",
                &UsageEvent {
                    scope,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(format!("{}", in_range_base + offset)),
                    tokens,
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    let count = store
        .query_usage_request_count(UsageSummaryRequest {
            from: Some("1752220800".into()),
            to: Some("1752307200".into()),
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        count, 2,
        "must count only in-range TurnDelta events (2 TurnDelta; the SessionTotal \
         describes the same request and the ContextSnapshot is telemetry)"
    );
}

/// Regression (UI default scope): when two sessions in the SAME workspace use
/// the same reporting agent and model, the default summary request
/// (all_workspaces=false, no explicit workspace_root → store fallback) must
/// count BOTH sessions. The user saw "1 session" despite working in 2+ sessions.
#[test]
fn usage_summary_default_scope_counts_all_sessions_in_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    // Two sessions, same workspace (the store's own workspace_root), same
    // model/agent — mimicking the common "switched between two sessions
    // of the same repo" scenario.
    for (sid, ts) in [("s1", "100"), ("s2", "200")] {
        store.create_session(sid, "gpt-5.1").unwrap();
        store.update_session_agent_cli(sid, "codex-acp").unwrap();
        store
            .append_usage_event(
                sid,
                &UsageEvent {
                    scope: UsageEventScope::TurnDelta,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some(ts.into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(50),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    // Default request: all_workspaces=false, no explicit workspace_root.
    let rows = store
        .query_usage_summary(UsageSummaryRequest::default())
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(
        rows[0].session_count,
        2,
        "both sessions in the workspace must be counted"
    );
    assert_eq!(
        rows[0].tokens.total_tokens,
        Some(100),
        "50 + 50 = 100"
    );
}

/// Regression: when two sessions use the SAME model, each emitting its own
/// SessionTotal (cumulative) + TurnDelta, the cross-session summary must SUM
/// both sessions' request-scoped TurnDeltas (75 + 40 = 115). SessionTotal is
/// retained only as a no-TurnDelta fallback; preferring it would mis-attribute
/// after mid-session model switches and double-count once TurnDeltas exist.
#[test]
fn usage_summary_sums_session_totals_across_sessions_with_same_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("a", "gpt-5.1").unwrap();
    store.update_session_agent_cli("a", "codex-acp").unwrap();
    store.create_session("b", "gpt-5.1").unwrap();
    store.update_session_agent_cli("b", "codex-acp").unwrap();

    // Both sessions share timestamp "10" so the SQL ORDER BY tie-break is
    // non-deterministic; the fix must produce the same total regardless of
    // interleaving order.
    for (sid, total) in [("a", 150u64), ("b", 80u64)] {
        store
            .append_usage_event(
                sid,
                &UsageEvent {
                    scope: UsageEventScope::SessionTotal,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some("10".into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(total),
                        input_tokens: Some(total),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
        store
            .append_usage_event(
                sid,
                &UsageEvent {
                    scope: UsageEventScope::TurnDelta,
                    model: Some("gpt-5.1".into()),
                    provider: Some("openai".into()),
                    agent_cli: Some("codex-acp".into()),
                    timestamp: Some("10".into()),
                    tokens: UsageTokenBreakdown {
                        total_tokens: Some(total / 2),
                        ..Default::default()
                    },
                    context: UsageContextSnapshot::default(),
                    raw_json: None,
                },
                None,
                None,
            )
            .unwrap();
    }

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(
        rows[0].tokens.total_tokens,
        Some(115),
        "TurnDeltas from two sessions must SUM (75 + 40), not SessionTotals"
    );
    assert_eq!(
        rows[0].tokens.input_tokens,
        None,
        "TurnDelta fixtures only populate total_tokens"
    );
    assert_eq!(rows[0].session_count, 2, "two sessions expected");
    assert_eq!(
        rows[0].request_count, 2,
        "request_count counts one per real request (only TurnDelta; 2 sessions \
         x 1 TurnDelta each = 2)"
    );
    assert_eq!(rows[0].event_count, 4, "all 4 rows counted");
}

/// Regression: a session that only emits TurnDelta events (no SessionTotal)
/// must still contribute its accumulated deltas even when another session
/// using the same model has already emitted a SessionTotal. The old per-group
/// flag would suppress the TurnDelta-only session's tokens.
#[test]
fn usage_summary_turn_delta_session_survives_after_peer_session_total() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("a", "gpt-5.1").unwrap();
    store.update_session_agent_cli("a", "codex-acp").unwrap();
    store.create_session("b", "gpt-5.1").unwrap();
    store.update_session_agent_cli("b", "codex-acp").unwrap();

    // Session A: authoritative SessionTotal(150).
    store
        .append_usage_event(
            "a",
            &UsageEvent {
                scope: UsageEventScope::SessionTotal,
                model: Some("gpt-5.1".into()),
                provider: Some("openai".into()),
                agent_cli: Some("codex-acp".into()),
                timestamp: Some("10".into()),
                tokens: UsageTokenBreakdown {
                    total_tokens: Some(150),
                    ..Default::default()
                },
                context: UsageContextSnapshot::default(),
                raw_json: None,
            },
            None,
            None,
        )
        .unwrap();
    // Session B: only TurnDelta(50), no SessionTotal.
    store
        .append_usage_event(
            "b",
            &make_usage_event("gpt-5.1", 50, "11"),
            None,
            None,
        )
        .unwrap();

    let rows = store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            group_by: UsageSummaryGroupBy::Model,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1, "single model row expected, got: {rows:?}");
    assert_eq!(
        rows[0].tokens.total_tokens,
        Some(200),
        "Session A's SessionTotal(150) + Session B's TurnDelta(50) = 200"
    );
}

fn make_tool(id: &str, call_id: &str) -> ToolInvocation {
    ToolInvocation {
        id: Uuid::parse_str(id).unwrap_or_else(|_| Uuid::new_v4()),
        call_id: call_id.to_string(),
        parent_call_id: None,
        name: "shell".into(),
        kind: "shell".into(),
        summary: format!("summary-{call_id}"),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: Some(format!("input-{call_id}")),
        raw_output: Some(format!("output-{call_id}")),
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_input: None,
        permission_decision: None,
        can_stop: false,
        stop_kind: None,
        stop_status: None,
    }
}

#[test]
fn windowed_load_keeps_only_recent_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    // 10 messages (seq 1..=10) + 10 tools (seq 11..=20) = 20 entries.
    for i in 1..=10 {
        store
            .insert_message("s1", &Uuid::new_v4().to_string(), "User", &format!("m{i}"), i)
            .unwrap();
    }
    for i in 11..=20 {
        store
            .insert_tool("s1", &make_tool(&Uuid::new_v4().to_string(), &format!("c{i}")), i)
            .unwrap();
    }

    let window = store.load_session_window("s1", 5).unwrap();
    assert_eq!(window.total_count, 20);
    assert_eq!(window.timeline.len(), 5, "only the latest 5 entries load");
    // Latest 5 entries are seq 16..=20 (all tools).
    assert_eq!(window.earliest_seq, Some(16));
    assert!(window.messages.is_empty());
    assert_eq!(window.tools.len(), 5);
}

#[test]
fn windowed_load_short_session_loads_all() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    store
        .insert_message("s1", &Uuid::new_v4().to_string(), "User", "hi", 1)
        .unwrap();
    store
        .insert_message("s1", &Uuid::new_v4().to_string(), "Assistant", "hello", 2)
        .unwrap();

    let window = store.load_session_window("s1", 100).unwrap();
    assert_eq!(window.total_count, 2);
    assert_eq!(window.timeline.len(), 2);
    assert_eq!(window.earliest_seq, None, "nothing older to page");
}

// Helper: build a session with `turns` turns, each = 1 user + `tools_per_turn`
// tools + 1 assistant, sequenced in order. Returns the seq of each turn's user
// message.
fn build_turned_session(
    store: &SessionStore,
    session: &str,
    turns: usize,
    tools_per_turn: usize,
) -> Vec<i64> {
    let mut seq = 0i64;
    let mut user_seqs = Vec::new();
    for turn in 0..turns {
        seq += 1;
        user_seqs.push(seq);
        store
            .insert_message(
                session,
                &Uuid::new_v4().to_string(),
                "User",
                &format!("question {turn}"),
                seq,
            )
            .unwrap();
        for tool in 0..tools_per_turn {
            seq += 1;
            store
                .insert_tool(
                    session,
                    &make_tool(
                        &Uuid::new_v4().to_string(),
                        &format!("call-{turn}-{tool}"),
                    ),
                    seq,
                )
                .unwrap();
        }
        seq += 1;
        store
            .insert_message(
                session,
                &Uuid::new_v4().to_string(),
                "Assistant",
                &format!("answer {turn}"),
                seq,
            )
            .unwrap();
    }
    user_seqs
}

#[test]
fn turn_aligned_window_starts_on_turn_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    // 5 turns of 10 tools each = 60 entries. A plain 25-entry window would
    // start mid-turn (inside turn 4). Turn-aligned with min_turns=3 must back
    // up to turn 3's user message (the 3rd-most-recent turn).
    let user_seqs = build_turned_session(&store, "s1", 5, 10);

    let window = store.load_session_window_by_turns("s1", 25, 3).unwrap();
    assert_eq!(window.total_count, 60);
    // 3 turns of 12 entries each = 36 entries.
    assert_eq!(window.timeline.len(), 36);
    assert_eq!(
        window.earliest_seq,
        Some(user_seqs[2]),
        "window starts at the 3rd-most-recent turn's user message"
    );
    // The very first entry is a User message (the turn boundary), not a tool.
    assert_eq!(window.messages[0].role, MessageRole::User);
    assert!(matches!(window.timeline[0], TimelineItem::Message(_)));
}

#[test]
fn turn_aligned_window_loads_giant_final_turn_fully() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    // 2 small turns, then a final turn with 300 tools (> the 25-entry floor).
    build_turned_session(&store, "s1", 2, 5);
    let mut seq = 24i64;
    let final_user_seq = seq + 1;
    store
        .insert_message("s1", &Uuid::new_v4().to_string(), "User", "final question", final_user_seq)
        .unwrap();
    seq = final_user_seq;
    for tool in 0..300 {
        seq += 1;
        store
            .insert_tool(
                "s1",
                &make_tool(&Uuid::new_v4().to_string(), &format!("final-{tool}")),
                seq,
            )
            .unwrap();
    }
    seq += 1;
    store
        .insert_message("s1", &Uuid::new_v4().to_string(), "Assistant", "final answer", seq)
        .unwrap();

    let window = store.load_session_window_by_turns("s1", 25, 3).unwrap();
    // The final turn (302 entries) + the 2 small turns (2×7) — but min_turns=3
    // backs up to the 1st turn's user message, loading everything.
    assert_eq!(window.messages[0].role, MessageRole::User);
    // Crucially the final turn's user prompt is present even though the turn
    // dwarfs the entry floor.
    assert!(window
        .messages
        .iter()
        .any(|message| message.body == "final question"));
    assert!(window
        .messages
        .iter()
        .any(|message| message.body == "final answer"));
}

#[test]
fn turn_aligned_window_falls_back_to_floor_when_turns_are_small() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    // 5 turns with only 1 tool each: 15 entries total. Turn-aligned window
    // (3 turns = 9 entries) is smaller than the 12-entry floor, so the loader
    // keeps the entry-count window instead.
    build_turned_session(&store, "s1", 5, 1);

    let window = store.load_session_window_by_turns("s1", 12, 3).unwrap();
    assert_eq!(window.timeline.len(), 12, "floor wins over turn alignment");
}

#[test]
fn turn_aligned_window_loads_everything_when_fewer_turns_than_requested() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    build_turned_session(&store, "s1", 2, 4);

    let window = store.load_session_window_by_turns("s1", 5, 3).unwrap();
    // 2 turns × (1 user + 4 tools + 1 assistant) = 12 entries.
    assert_eq!(window.timeline.len(), 12, "only 2 turns exist: load all");
    assert_eq!(window.earliest_seq, None, "nothing older to page");
}

#[test]
fn history_before_pages_older_entries_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    for i in 1..=8 {
        store
            .insert_message("s1", &Uuid::new_v4().to_string(), "User", &format!("m{i}"), i)
            .unwrap();
    }

    let (messages, _tools, timeline, earliest) = store.load_history_before("s1", 6, 3).unwrap();
    // Entries strictly before seq 6 are 1..=5; latest 3 of those are 3,4,5.
    assert_eq!(timeline.len(), 3);
    assert_eq!(earliest, Some(3), "next cursor is this page's earliest seq");
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["m3", "m4", "m5"], "ascending order, most recent 3");
}

#[test]
fn load_tool_detail_returns_uncapped_stored_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();
    let tool_id = Uuid::new_v4().to_string();
    store
        .insert_tool("s1", &make_tool(&tool_id, "call-1"), 1)
        .unwrap();

    let detail = store.load_tool_detail("s1", &tool_id).unwrap();
    assert_eq!(detail.raw_input.as_deref(), Some("input-call-1"));
    assert_eq!(detail.raw_output.as_deref(), Some("output-call-1"));
    assert!(store.load_tool_detail("s1", &Uuid::new_v4().to_string()).is_err());
}

#[test]
fn load_session_usage_snapshot_rebuilds_context_from_persisted_events() {
    // Regression guard for the phone's session-info sheet: the usage
    // projection reaches the phone through the session snapshot, so the
    // persisted-events -> snapshot reconstruction must keep context
    // occupancy (used/window) intact.
    use workspace_model::{UsageContextSnapshot, UsageEvent, UsageEventScope, UsageTokenBreakdown};
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let session_id = "11111111-1111-4111-8111-111111111111";
    store.create_session(session_id, "test-model").unwrap();

    let event = UsageEvent {
        scope: UsageEventScope::ContextSnapshot,
        model: Some("cline-pass/glm-5.3-flash".into()),
        provider: None,
        agent_cli: Some("DeepSeek Harness".into()),
        tokens: UsageTokenBreakdown::default(),
        context: UsageContextSnapshot {
            used_tokens: Some(334_459),
            window_tokens: Some(500_000),
            updated_at: Some("2026-08-29T12:00:00Z".into()),
        },
        timestamp: None,
        raw_json: None,
    };
    store.append_usage_event(session_id, &event, None, None).unwrap();

    let snapshot = store.load_session_usage_snapshot(session_id).unwrap();
    assert_eq!(snapshot.context.used_tokens, Some(334_459));
    assert_eq!(snapshot.context.window_tokens, Some(500_000));
    assert!(!snapshot.by_model.is_empty(), "by_model must carry the model summary");
}

#[test]
fn list_change_sets_filters_by_session_in_sql() {
    // Regression: the session filter used to be applied in Rust over every
    // change set of the *workspace*, with an extra `SELECT session_id` per
    // non-matching row (a query that could only return the value already in
    // hand). Scoping must stay exact now that SQL does the work.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let session_a = Uuid::new_v4().to_string();
    let session_b = Uuid::new_v4().to_string();
    store.create_session(&session_a, "gpt-4").unwrap();
    store.create_session(&session_b, "gpt-4").unwrap();

    for (session_id, id) in [(session_a.as_str(), "set-a"), (session_b.as_str(), "set-b")] {
        let summary = make_change_set_summary(
            &store,
            id,
            session_id,
            ChangeSetSource::AgentTurn,
            None,
            id,
        );
        store
            .replace_change_set(
                &summary,
                &[make_file_record(id, "src/main.rs", Some("old"), Some("new"), 1, 1)],
            )
            .unwrap();
    }
    // A workspace-scoped set (no owning session, like the git-worktree sets)
    // must never be listed for one session.
    let workspace_set = make_change_set_summary(
        &store,
        "set-workspace",
        "not-a-session-uuid",
        ChangeSetSource::AgentTurn,
        None,
        "set-workspace",
    );
    store
        .replace_change_set(
            &workspace_set,
            &[make_file_record(
                "set-workspace",
                "src/main.rs",
                Some("old"),
                Some("new"),
                1,
                1,
            )],
        )
        .unwrap();

    let summaries = store
        .list_change_sets(Some(&session_a), Some(ChangeSetSource::AgentTurn))
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "set-a");

    let summaries = store
        .list_change_sets(Some(&session_b), Some(ChangeSetSource::AgentTurn))
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "set-b");

    let all = store.list_change_sets(None, Some(ChangeSetSource::AgentTurn)).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn legacy_change_set_summaries_aggregate_turn_totals() {
    // The legacy wrappers are now backed by COUNT/SUM/MAX aggregates instead of
    // loading every historical diff text. Totals, file counts and the newest
    // timestamp must survive that change, and a per-turn diff lookup must
    // return only that turn's file.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let older = Uuid::new_v4();
    let newer = Uuid::new_v4();

    store.create_session("s-agg", "gpt-4").unwrap();
    store
        .insert_message("s-agg", &older.to_string(), "Assistant", "older", 1)
        .unwrap();
    store
        .insert_message("s-agg", &newer.to_string(), "Assistant", "newer", 2)
        .unwrap();
    store
        .replace_turn_file_changes(
            "s-agg",
            &older,
            &[
                SessionFileChange {
                    path: "src/one.rs".into(),
                    change_type: FileChangeType::Modified,
                    old_text: Some("a".into()),
                    new_text: "b".into(),
                    added_lines: 2,
                    removed_lines: 1,
                    timestamp: "10".into(),
                },
                SessionFileChange {
                    path: "src/two.rs".into(),
                    change_type: FileChangeType::Created,
                    old_text: None,
                    new_text: "fresh".into(),
                    added_lines: 4,
                    removed_lines: 0,
                    timestamp: "11".into(),
                },
            ],
        )
        .unwrap();
    store
        .replace_turn_file_changes(
            "s-agg",
            &newer,
            &[SessionFileChange {
                path: "src/three.rs".into(),
                change_type: FileChangeType::Modified,
                old_text: Some("x".into()),
                new_text: "y".into(),
                added_lines: 1,
                removed_lines: 3,
                timestamp: "12".into(),
            }],
        )
        .unwrap();

    let summaries = store.list_change_sets_with_legacy("s-agg", None).unwrap();
    let older_summary = summaries
        .iter()
        .find(|summary| summary.id == legacy_agent_turn_id("s-agg", &older))
        .expect("older turn summary");
    assert_eq!(older_summary.file_count, 2);
    assert_eq!(older_summary.added_lines, 6);
    assert_eq!(older_summary.removed_lines, 1);
    assert!(
        !older_summary.updated_at.is_empty(),
        "the aggregate must still carry the newest row timestamp"
    );
    assert_eq!(older_summary.message_id, Some(older));

    let newer_summary = summaries
        .iter()
        .find(|summary| summary.id == legacy_agent_turn_id("s-agg", &newer))
        .expect("newer turn summary");
    assert_eq!(newer_summary.file_count, 1);
    assert_eq!(newer_summary.added_lines, 1);
    assert_eq!(newer_summary.removed_lines, 3);
    assert!(!newer_summary.updated_at.is_empty());

    let diff = store
        .load_change_set_file_diff_with_legacy(&legacy_agent_turn_id("s-agg", &newer), "src/three.rs")
        .unwrap()
        .expect("newer turn diff");
    assert_eq!(diff.new_text.as_deref(), Some("y"));
    assert!(
        store
            .load_change_set_file_diff_with_legacy(&legacy_agent_turn_id("s-agg", &newer), "src/one.rs")
            .unwrap()
            .is_none(),
        "a per-turn lookup must not leak another turn's file"
    );

    let files = store
        .list_change_set_files_with_legacy(&legacy_agent_turn_id("s-agg", &older))
        .unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "src/one.rs");
}

#[test]
fn load_recent_turn_file_changes_keeps_newest_and_orders_chronologically() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();

    store.create_session("s1", "gpt-4").unwrap();
    for (seq, message_id) in ids.iter().enumerate() {
        store
            .insert_message("s1", &message_id.to_string(), "Assistant", "body", (seq + 1) as i64)
            .unwrap();
        store
            .replace_turn_file_changes(
                "s1",
                message_id,
                &[SessionFileChange {
                    path: format!("src/file{}.ts", seq),
                    change_type: FileChangeType::Modified,
                    old_text: Some("old".into()),
                    new_text: format!("new {}", seq),
                    added_lines: 1,
                    removed_lines: 0,
                    timestamp: format!("{}", seq),
                }],
            )
            .unwrap();
    }

    // Limit 2 keeps the two NEWEST turns (seq 2 and 3) and returns them in
    // chronological (oldest-first) order.
    let loaded = store.load_recent_turn_file_changes("s1", 2).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].message_id, ids[1]);
    assert_eq!(loaded[1].message_id, ids[2]);
    assert!(loaded[0].changes[0].old_text.is_some(), "texts must be restored for GetFileDiff");

    // A limit larger than the turn count returns everything, oldest first.
    let loaded = store.load_recent_turn_file_changes("s1", 10).unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].message_id, ids[0]);
}

#[test]
fn repair_pending_agent_turn_change_sets_anchors_assistant_message() {
    // A turn whose finalize never ran leaves an AgentTurn set Pending with a
    // NULL message id — the review panel's `selectReviewChangeSet` cannot
    // select such a set once the turn is over, so the recorded edits vanished
    // from the tab. The repair must anchor the set to the turn's last
    // assistant message, mark it Complete, and mirror the files into the
    // per-turn table the restore path reads.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let user_id = Uuid::new_v4();
    let assistant_id = Uuid::new_v4();
    let next_user_id = Uuid::new_v4();
    store.create_session(&session_id, "gpt-4").unwrap();
    store
        .insert_message(&session_id, &user_id.to_string(), "User", "go", 1)
        .unwrap();
    store
        .insert_message(&session_id, &assistant_id.to_string(), "Assistant", "did it", 2)
        .unwrap();
    store
        .insert_message(&session_id, &next_user_id.to_string(), "User", "next", 3)
        .unwrap();

    let change_set_id = format!("agent-turn:{session_id}:{user_id}");
    let mut summary = make_change_set_summary(
        &store,
        &change_set_id,
        &session_id,
        ChangeSetSource::AgentTurn,
        None,
        "本轮对话",
    );
    // The helper hardcodes Complete; the stuck-pending shape is what repair
    // looks for.
    summary.status = ChangeSetStatus::Pending;
    store
        .replace_change_set(
            &summary,
            &[
                make_file_record(&change_set_id, "scripts/new.py", None, Some("created"), 9, 0),
                make_file_record(&change_set_id, "src/main.rs", Some("old"), Some("new"), 2, 1),
            ],
        )
        .unwrap();

    assert_eq!(store.repair_pending_agent_turn_change_sets().unwrap(), 1, "one set repaired");

    let summaries = store
        .list_change_sets(Some(&session_id), Some(ChangeSetSource::AgentTurn))
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].status, ChangeSetStatus::Complete);
    assert_eq!(summaries[0].message_id, Some(assistant_id));

    let turns = store.load_turn_file_changes(&session_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].message_id, assistant_id);
    assert_eq!(turns[0].changes.len(), 2);

    // Idempotent: a rerun must not report further repairs.
    assert_eq!(store.repair_pending_agent_turn_change_sets().unwrap(), 0);
}

#[test]
fn list_sessions_emits_iso8601_timestamps_for_phone_date_parse() {
    // The phone formats relative times with `Date.parse`, which is
    // engine-specific for bare epoch strings (Hermes mis-parsed them and
    // every session showed "刚刚"). The wire format must be ISO-8601 UTC.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store.create_session("s1", "gpt-4").unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let item = &sessions[0];
    // ISO-8601 UTC: `YYYY-MM-DDTHH:MM:SSZ` — unambiguous for every JS engine.
    assert!(item.updated_at.ends_with('Z'), "updated_at must end with Z: {}", item.updated_at);
    assert_eq!(item.updated_at.len(), 20, "YYYY-MM-DDTHH:MM:SSZ is 20 chars: {}", item.updated_at);
    assert!(item.created_at.ends_with('Z'));
    assert_eq!(&item.updated_at[10..11], "T");
    assert!(item.updated_at.starts_with("20"), "sane year: {}", item.updated_at);
}

fn make_automation(id: &str, name: &str, next_run_at_ms: Option<i64>) -> AutomationRecord {
    AutomationRecord {
        id: id.to_string(),
        name: name.to_string(),
        prompt: format!("{name} 的提示词"),
        workspace_root: "/tmp/ws".to_string(),
        agent_cli: Some(AgentCliId::DeepSeekHarness),
        agent_preset: Some("default".to_string()),
        schedule: AutomationSchedule {
            kind: AutomationScheduleKind::Daily,
            interval_minutes: None,
            hour: Some(9),
            minute: Some(30),
            weekday: None,
            run_at_ms: None,
        },
        enabled: true,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        next_run_at_ms,
        run_count: 0,
        last_run: None,
    }
}

#[test]
fn test_automation_crud_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    let record = make_automation("a1", "晨报", Some(1_000));
    store.insert_automation(&record).unwrap();

    let loaded = store.get_automation("a1").unwrap().unwrap();
    assert_eq!(loaded.name, "晨报");
    assert_eq!(loaded.prompt, "晨报 的提示词");
    assert_eq!(loaded.agent_cli, Some(AgentCliId::DeepSeekHarness));
    assert_eq!(loaded.agent_preset.as_deref(), Some("default"));
    assert_eq!(loaded.schedule.kind, AutomationScheduleKind::Daily);
    assert_eq!(loaded.schedule.hour, Some(9));
    assert_eq!(loaded.schedule.minute, Some(30));
    assert_eq!(loaded.next_run_at_ms, Some(1_000));
    assert!(loaded.enabled);
    assert_eq!(loaded.run_count, 0);
    assert!(loaded.last_run.is_none());

    let mut updated = loaded.clone();
    updated.name = "早报".to_string();
    updated.schedule.kind = AutomationScheduleKind::Weekly;
    updated.schedule.weekday = Some(1);
    updated.agent_cli = Some(AgentCliId::CodexAcp);
    updated.next_run_at_ms = Some(2_000);
    store.update_automation(&updated).unwrap();

    let loaded = store.get_automation("a1").unwrap().unwrap();
    assert_eq!(loaded.name, "早报");
    assert_eq!(loaded.schedule.kind, AutomationScheduleKind::Weekly);
    assert_eq!(loaded.schedule.weekday, Some(1));
    assert_eq!(loaded.agent_cli, Some(AgentCliId::CodexAcp));
    assert_eq!(loaded.next_run_at_ms, Some(2_000));

    store.set_automation_enabled("a1", false).unwrap();
    let loaded = store.get_automation("a1").unwrap().unwrap();
    assert!(!loaded.enabled);

    store.set_automation_next_run("a1", None).unwrap();
    let loaded = store.get_automation("a1").unwrap().unwrap();
    assert_eq!(loaded.next_run_at_ms, None);

    store.delete_automation("a1").unwrap();
    assert!(store.get_automation("a1").unwrap().is_none());
}

#[test]
fn test_due_automations_filtering() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();

    store
        .insert_automation(&make_automation("due", "到点", Some(100)))
        .unwrap();
    store
        .insert_automation(&make_automation("future", "未到", Some(5_000)))
        .unwrap();
    store
        .insert_automation(&make_automation("unscheduled", "无计划", None))
        .unwrap();
    let mut disabled = make_automation("disabled", "停用", Some(100));
    disabled.enabled = false;
    store.insert_automation(&disabled).unwrap();

    let due = store.list_due_automations(200).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, "due");
}

#[test]
fn test_automation_runs_lifecycle_and_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path(), dir.path()).unwrap();
    store
        .insert_automation(&make_automation("a1", "晨报", Some(1_000)))
        .unwrap();

    let run = AutomationRunRecord {
        id: "r1".to_string(),
        automation_id: "a1".to_string(),
        trigger: AutomationRunTrigger::Scheduled,
        status: AutomationRunStatus::Running,
        started_at: "2026-01-01T09:30:00Z".to_string(),
        finished_at: None,
        session_id: None,
        workspace_root: "/tmp/ws".to_string(),
        error: None,
    };
    store.insert_automation_run(&run).unwrap();
    store
        .attach_automation_run_session("r1", "session-1")
        .unwrap();
    store
        .finish_automation_run("r1", AutomationRunStatus::Completed, None)
        .unwrap();

    let runs = store.list_automation_runs("a1", 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, AutomationRunStatus::Completed);
    assert_eq!(runs[0].session_id.as_deref(), Some("session-1"));
    assert_eq!(runs[0].trigger, AutomationRunTrigger::Scheduled);

    // A second, failed run sorts first (started_at DESC) and feeds `last_run`.
    let run2 = AutomationRunRecord {
        id: "r2".to_string(),
        automation_id: "a1".to_string(),
        trigger: AutomationRunTrigger::Manual,
        status: AutomationRunStatus::Running,
        started_at: "2026-01-02T09:30:00Z".to_string(),
        finished_at: None,
        session_id: None,
        workspace_root: "/tmp/ws".to_string(),
        error: None,
    };
    store.insert_automation_run(&run2).unwrap();
    store
        .finish_automation_run("r2", AutomationRunStatus::Failed, Some("启动失败"))
        .unwrap();

    let runs = store.list_automation_runs("a1", 10).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].id, "r2");
    assert_eq!(runs[0].status, AutomationRunStatus::Failed);
    assert_eq!(runs[0].error.as_deref(), Some("启动失败"));

    let loaded = store.get_automation("a1").unwrap().unwrap();
    assert_eq!(loaded.run_count, 2);
    assert_eq!(loaded.last_run.as_ref().map(|run| run.id.as_str()), Some("r2"));

    // Reconciliation query: nothing is left `running`.
    let running = store
        .list_automation_runs_with_status(AutomationRunStatus::Running)
        .unwrap();
    assert!(running.is_empty());

    // Deleting the automation cascades to its runs.
    store.delete_automation("a1").unwrap();
    let runs = store.list_automation_runs("a1", 10).unwrap();
    assert!(runs.is_empty());
}
