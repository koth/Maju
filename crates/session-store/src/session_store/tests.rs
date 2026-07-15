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
    assert_eq!(buckets[0].by_model[0].request_count, 2);

    // Day B: lone TurnDelta(300) accumulates.
    assert_eq!(buckets[1].tokens.total_tokens, Some(300));
    assert_eq!(buckets[1].by_model.len(), 1);
    assert_eq!(buckets[1].by_model[0].tokens.total_tokens, Some(300));
    assert_eq!(buckets[1].by_model[0].event_count, 1);
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

/// P5: `request_count` must count only token-reporting events
/// (TurnDelta + SessionTotal), while `event_count` counts every row
/// including ContextSnapshot occupancy-only reports. With
/// 1×SessionTotal + 1×TurnDelta + 3×ContextSnapshot we expect
/// event_count=5 and request_count=2.
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
        rows[0].request_count, 2,
        "request_count must exclude ContextSnapshot occupancy-only reports"
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
        rows[0].request_count, 1,
        "baseline SessionTotal must not inflate request_count"
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
        rows[0].request_count, 1,
        "only the in-range SessionTotal should count as a request"
    );
    assert_eq!(rows[0].event_count, 1);
    assert_eq!(rows[0].session_count, 1);
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

/// `query_usage_request_count` must count only in-range token-reporting
/// events (`TurnDelta` + `SessionTotal`), excluding both the pre-range
/// `SessionTotal` baseline (which `query_usage_summary` merges in via
/// `merge_baseline_events` and would otherwise inflate the count) and
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
        count, 3,
        "must count only in-range token-reporting events (2 TurnDelta + 1 SessionTotal), \
         excluding the pre-range baseline and the ContextSnapshot"
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
        rows[0].request_count, 4,
        "2 SessionTotal + 2 TurnDelta = 4 token-reporting events"
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
