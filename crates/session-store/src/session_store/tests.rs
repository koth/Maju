use super::*;
use workspace_model::DiffQuality;

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
            &make_usage_event("gpt-5.1", 10, "2026-06-01T00:00:00Z"),
            None,
            None,
        )
        .unwrap();
    store_a.create_session("a2", "gpt-5.1").unwrap();
    store_a
        .append_usage_event(
            "a2",
            &make_usage_event("gpt-5.1", 20, "2026-06-02T00:00:00Z"),
            None,
            None,
        )
        .unwrap();
    store_a.archive_session("a2").unwrap();

    store_b.create_session("b1", "claude-opus-4.7").unwrap();
    store_b
        .append_usage_event(
            "b1",
            &make_usage_event("claude-opus-4.7", 30, "2026-06-03T00:00:00Z"),
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

    let date_filtered = store_a
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: true,
            from: Some("2026-06-02T00:00:00Z".into()),
            to: Some("2026-06-02T23:59:59Z".into()),
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
