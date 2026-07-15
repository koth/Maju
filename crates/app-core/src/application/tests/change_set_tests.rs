use super::*;
use workspace_model::ChangeSetStatus;

#[test]
fn multiple_files_with_nonzero_changes_keep_change_set_after_turn() {
    // Regression: a turn that edits two files must keep its AgentTurn change
    // set (and thus the ChangesBar / review panel) even though the per-file
    // added/removed line counts happen to cancel out across files. The
    // visibility gate is per-file `file_count`, never a net added-minus-removed
    // sum, so neither file may be dropped.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "edit two files".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = Some(user_id);

    // File A: +3 -1, File B: +1 -3 — net cancels, but each file is non-empty.
    app.upsert_review_file_change(
        "src/a.rs",
        FileChangeType::Modified,
        Some("x\n".into()),
        "a\nb\nc\nd\n".into(),
    );
    app.upsert_review_file_change(
        "src/b.rs",
        FileChangeType::Modified,
        Some("p\nq\nr\ns\n".into()),
        "t\n".into(),
    );
    assert_eq!(app.ui.review_changes.len(), 2);
    assert!(app.persist_current_turn_file_changes());

    let turn_sets = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let pending = turn_sets
        .iter()
        .find(|summary| summary.status == ChangeSetStatus::Complete)
        .expect("turn change set must survive with two non-empty files");
    let files = app.store.list_change_set_files(&pending.id).unwrap();
    assert_eq!(files.len(), 2);
}

#[test]
fn codex_acp_apply_patch_two_files_persist_turn_change_set() {
    // Reproduce the Codex (codex-acp) apply_patch path: file changes arrive
    // via the file tracker (no ACP ToolDiff event), one call per file. The
    // turn's AgentTurn change set must be persisted so the ChangesBar and
    // review panel show both files after the turn ends.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "edit two files".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = Some(user_id);

    assert!(app.apply_tracker_changes(
        "call-apply-patch-a",
        vec![crate::file_tracker::VerifiedFileChange {
            path: "src/a.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("x\n".into()),
            new_text: "a\nb\nc\nd\n".into(),
            skipped_diff: false,
            quality: DiffQuality::Exact,
        }],
    ));
    assert!(app.apply_tracker_changes(
        "call-apply-patch-b",
        vec![crate::file_tracker::VerifiedFileChange {
            path: "src/b.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("p\nq\nr\ns\n".into()),
            new_text: "t\n".into(),
            skipped_diff: false,
            quality: DiffQuality::Exact,
        }],
    ));
    assert_eq!(app.ui.review_changes.len(), 2);
    assert!(app.review_changes_started);
    assert!(app.persist_current_turn_file_changes());

    let turn_sets = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let completed = turn_sets
        .iter()
        .find(|summary| summary.status == ChangeSetStatus::Complete)
        .expect("turn change set must be persisted for codex-acp apply_patch edits");
    assert_eq!(completed.file_count, 2);
    let files = app.store.list_change_set_files(&completed.id).unwrap();
    assert_eq!(files.len(), 2);
}

#[test]
fn current_turn_without_file_changes_does_not_inherit_recent_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    let recent_change = SessionFileChange {
        path: "backend/scripts/run_dev.ps1".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("old\n".into()),
        new_text: "new\n".into(),
        added_lines: 1,
        removed_lines: 1,
        timestamp: "2026-05-13T00:00:00Z".into(),
    };

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "How do I start the frontend?".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "Use npm run dev.".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = Some(user_id);
    app.review_changes_started = false;
    app.ui.review_changes = vec![recent_change.clone()];
    app.ui.turn_changes = vec![TurnFileChanges {
        message_id: assistant_id,
        changes: vec![recent_change],
    }];

    assert!(app.persist_current_turn_file_changes());

    assert_eq!(app.ui.review_changes.len(), 1);
    assert!(app.ui.turn_changes.is_empty());
    assert!(
        app.store
            .load_turn_file_changes(&app.ui.session.id.to_string())
            .unwrap()
            .is_empty()
    );
    assert!(
        app.store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn)
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn new_prompt_first_change_does_not_inherit_previous_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let first_user = uuid::Uuid::new_v4();
    let first_assistant = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: first_user,
        role: MessageRole::User,
        body: "first turn".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: first_assistant,
        role: MessageRole::Assistant,
        body: "first done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(first_user));
    app.ui.timeline.push(TimelineItem::Message(first_assistant));
    app.current_turn_user_message_id = Some(first_user);

    app.upsert_review_file_change(
        "src/previous.rs",
        FileChangeType::Modified,
        Some("previous before\n".into()),
        "previous after\n".into(),
    );
    assert!(app.persist_current_turn_file_changes());
    assert_eq!(app.ui.review_changes.len(), 1);
    app.current_turn_user_message_id = None;

    app.send_prompt_background("second turn").unwrap();
    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, "src/previous.rs");
    assert_eq!(
        app.store
            .load_review_file_changes(&app.ui.session.id.to_string())
            .unwrap()
            .len(),
        1
    );

    app.upsert_review_file_change(
        "src/current.rs",
        FileChangeType::Modified,
        Some("current before\n".into()),
        "current after\n".into(),
    );
    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, "src/current.rs");
    assert!(
        app.store
            .load_review_file_changes(&app.ui.session.id.to_string())
            .unwrap()
            .iter()
            .all(|change| change.path != "src/previous.rs")
    );

    let turn_sets = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let pending = turn_sets
        .iter()
        .find(|summary| summary.status == ChangeSetStatus::Pending)
        .expect("current turn should have a pending change set");
    let pending_files = app.store.list_change_set_files(&pending.id).unwrap();

    assert_eq!(pending_files.len(), 1);
    assert_eq!(pending_files[0].path, "src/current.rs");
    assert!(
        pending_files
            .iter()
            .all(|file| file.path != "src/previous.rs")
    );
}

#[test]
fn current_turn_changes_preserve_first_base_and_final_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "update file".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = Some(user_id);

    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("before\n".into()),
        "middle\n".into(),
    );
    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("middle\n".into()),
        "after\n".into(),
    );

    let pending = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, ChangeSetStatus::Pending);
    assert_eq!(pending[0].message_id, None);
    assert_eq!(pending[0].added_lines, 1);
    assert_eq!(pending[0].removed_lines, 1);

    assert!(app.persist_current_turn_file_changes());

    let entry = app
        .ui
        .turn_changes
        .iter()
        .find(|entry| entry.message_id == assistant_id)
        .expect("turn changes should be attached to assistant message");
    assert_eq!(entry.changes.len(), 1);
    assert_eq!(entry.changes[0].old_text.as_deref(), Some("before\n"));
    assert_eq!(entry.changes[0].new_text, "after\n");
    assert_eq!(entry.changes[0].added_lines, 1);
    assert_eq!(entry.changes[0].removed_lines, 1);

    let completed = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].status, ChangeSetStatus::Complete);
    assert_eq!(completed[0].message_id, Some(assistant_id));
    let stored_diff = app
        .store
        .load_change_set_file_diff(&completed[0].id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(stored_diff.old_text.as_deref(), Some("before\n"));
    assert_eq!(stored_diff.new_text.as_deref(), Some("after\n"));

    let conversation = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentConversation),
        )
        .unwrap();
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].added_lines, 1);
    assert_eq!(conversation[0].removed_lines, 1);
}

#[test]
fn current_turn_revert_removes_pending_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    app.current_turn_user_message_id = Some(user_id);

    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("A\n".into()),
        "B\n".into(),
    );
    assert_eq!(
        app.store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn)
            )
            .unwrap()
            .len(),
        1
    );

    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("B\n".into()),
        "A\n".into(),
    );

    assert!(
        app.store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn)
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manual_editor_saves_use_manual_change_set_and_preserve_first_base() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.record_manual_editor_save("src/main.rs", Some("A\n".into()), "B\n".into());
    app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "C\n".into());

    assert!(app.ui.session_changes.is_empty());
    assert!(app.ui.review_changes.is_empty());
    let manual_sets = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::ManualEdit),
        )
        .unwrap();
    assert_eq!(manual_sets.len(), 1);
    assert_eq!(manual_sets[0].added_lines, 1);
    assert_eq!(manual_sets[0].removed_lines, 1);
    let diff = app
        .store
        .load_change_set_file_diff(&manual_sets[0].id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(diff.old_text.as_deref(), Some("A\n"));
    assert_eq!(diff.new_text.as_deref(), Some("C\n"));
}

#[test]
fn manual_editor_revert_removes_manual_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.record_manual_editor_save("src/main.rs", Some("A\n".into()), "B\n".into());
    app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "A\n".into());

    assert!(
        app.store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::ManualEdit)
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manual_and_agent_changes_for_same_path_stay_separate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "agent edit".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = Some(user_id);

    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("A\n".into()),
        "B\n".into(),
    );
    assert!(app.persist_current_turn_file_changes());
    app.record_manual_editor_save("src/main.rs", Some("B\n".into()), "C\n".into());

    let conversation = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentConversation),
        )
        .unwrap();
    let manual = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::ManualEdit),
        )
        .unwrap();
    assert_eq!(conversation.len(), 1);
    assert_eq!(manual.len(), 1);
    let conversation_diff = app
        .store
        .load_change_set_file_diff(&conversation[0].id, "src/main.rs")
        .unwrap()
        .unwrap();
    let manual_diff = app
        .store
        .load_change_set_file_diff(&manual[0].id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(conversation_diff.old_text.as_deref(), Some("A\n"));
    assert_eq!(conversation_diff.new_text.as_deref(), Some("B\n"));
    assert_eq!(manual_diff.old_text.as_deref(), Some("B\n"));
    assert_eq!(manual_diff.new_text.as_deref(), Some("C\n"));
}

#[test]
fn interrupted_turn_change_set_remains_user_owned_when_no_assistant_message_exists() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "change then stop".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
    ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.current_turn_user_message_id = Some(user_id);

    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("before\n".into()),
        "after\n".into(),
    );
    assert!(!app.persist_current_turn_file_changes());

    let pending = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, ChangeSetStatus::Pending);
    assert_eq!(pending[0].message_id, None);
    assert_eq!(
        pending[0].owner_key.as_deref(),
        Some(format!("user-message:{user_id}").as_str())
    );
}

#[test]
fn historical_agent_turn_change_sets_keep_their_own_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let first_user = uuid::Uuid::new_v4();
    let first_assistant = uuid::Uuid::new_v4();
    let second_user = uuid::Uuid::new_v4();
    let second_assistant = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    for (id, role, body) in [
        (first_user, MessageRole::User, "first"),
        (first_assistant, MessageRole::Assistant, "first done"),
    ] {
        app.ui.messages.push(ChatMessage {
            id,
            role,
            body: body.into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        ..Default::default()
        });
        app.ui.timeline.push(TimelineItem::Message(id));
    }

    app.current_turn_user_message_id = Some(first_user);
    app.review_changes_started = false;
    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("A\n".into()),
        "B\n".into(),
    );
    assert!(app.persist_current_turn_file_changes());

    for (id, role, body) in [
        (second_user, MessageRole::User, "second"),
        (second_assistant, MessageRole::Assistant, "second done"),
    ] {
        app.ui.messages.push(ChatMessage {
            id,
            role,
            body: body.into(),
            created_at: "2026-05-13T00:00:00Z".into(),
        ..Default::default()
        });
        app.ui.timeline.push(TimelineItem::Message(id));
    }
    app.current_turn_user_message_id = Some(second_user);
    app.review_changes_started = false;
    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("B\n".into()),
        "C\n".into(),
    );
    assert!(app.persist_current_turn_file_changes());

    let turns = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    assert_eq!(turns.len(), 2);
    let first_turn = turns
        .iter()
        .find(|summary| summary.message_id == Some(first_assistant))
        .unwrap();
    let first_diff = app
        .store
        .load_change_set_file_diff(&first_turn.id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(first_diff.old_text.as_deref(), Some("A\n"));
    assert_eq!(first_diff.new_text.as_deref(), Some("B\n"));

    let conversation = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentConversation),
        )
        .unwrap();
    assert_eq!(conversation.len(), 1);
    let conversation_diff = app
        .store
        .load_change_set_file_diff(&conversation[0].id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(conversation_diff.old_text.as_deref(), Some("A\n"));
    assert_eq!(conversation_diff.new_text.as_deref(), Some("C\n"));

    let reloaded = test_app(&dir);
    let reloaded_turns = reloaded
        .store
        .list_change_sets(
            Some(&reloaded.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    assert_eq!(reloaded_turns.len(), 2);
    let reloaded_first = reloaded_turns
        .iter()
        .find(|summary| summary.message_id == Some(first_assistant))
        .unwrap();
    let reloaded_first_diff = reloaded
        .store
        .load_change_set_file_diff(&reloaded_first.id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(reloaded_first_diff.old_text.as_deref(), Some("A\n"));
    assert_eq!(reloaded_first_diff.new_text.as_deref(), Some("B\n"));
}

#[test]
fn scoped_change_set_queries_keep_agent_sources_separate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_message_id = uuid::Uuid::new_v4();
    let assistant_message_id = uuid::Uuid::new_v4();
    let change = SessionFileChange {
        path: "src/main.rs".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("before\n".into()),
        new_text: "after\n".into(),
        added_lines: 1,
        removed_lines: 1,
        timestamp: "1".into(),
    };

    app.current_turn_user_message_id = Some(user_message_id);
    app.ui.review_changes = vec![change.clone()];
    app.persist_current_agent_turn_change_set(
        Some(assistant_message_id),
        ChangeSetStatus::Complete,
    );
    app.ui.turn_changes.push(TurnFileChanges {
        message_id: assistant_message_id,
        changes: vec![change.clone()],
    });
    app.persist_agent_conversation_change_set_from_turns();

    let turn_sets = app.list_change_sets(ListChangeSetsRequest {
        source: Some(ChangeSetSource::AgentTurn),
        ..Default::default()
    });
    let conversation_sets = app.list_change_sets(ListChangeSetsRequest {
        source: Some(ChangeSetSource::AgentConversation),
        ..Default::default()
    });
    assert_eq!(turn_sets.len(), 1);
    assert_eq!(conversation_sets.len(), 1);

    let turn_diff = app
        .get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: turn_sets[0].id.clone(),
            path: "src/main.rs".into(),
        })
        .unwrap();
    let conversation_diff = app
        .get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: conversation_sets[0].id.clone(),
            path: "src/main.rs".into(),
        })
        .unwrap();
    let missing = app.get_change_set_file_diff(GetChangeSetFileDiffRequest {
        change_set_id: turn_sets[0].id.clone(),
        path: "src/other.rs".into(),
    });

    assert_eq!(turn_diff.old_text.as_deref(), Some("before\n"));
    assert_eq!(turn_diff.new_text.as_deref(), Some("after\n"));
    assert_eq!(conversation_diff.old_text.as_deref(), Some("before\n"));
    assert_eq!(conversation_diff.new_text.as_deref(), Some("after\n"));
    assert!(missing.is_none());
}

#[test]
fn scoped_change_set_queries_keep_manual_source_separate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.record_manual_editor_save(
        "src/manual.rs",
        Some("manual before\n".into()),
        "manual after\n".into(),
    );

    let manual_sets = app.list_change_sets(ListChangeSetsRequest {
        source: Some(ChangeSetSource::ManualEdit),
        ..Default::default()
    });
    assert_eq!(manual_sets.len(), 1);
    let files = app.list_change_set_files(ListChangeSetFilesRequest {
        change_set_id: manual_sets[0].id.clone(),
    });
    let diff = app
        .get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: manual_sets[0].id.clone(),
            path: "src/manual.rs".into(),
        })
        .unwrap();
    let agent_fallback = app.get_change_set_file_diff(GetChangeSetFileDiffRequest {
        change_set_id: "agent-conversation:missing".into(),
        path: "src/manual.rs".into(),
    });

    assert_eq!(files.files.len(), 1);
    assert_eq!(diff.old_text.as_deref(), Some("manual before\n"));
    assert_eq!(diff.new_text.as_deref(), Some("manual after\n"));
    assert!(agent_fallback.is_none());
}

#[test]
fn scoped_change_set_queries_expose_git_worktree_without_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "before\n").unwrap();
    commit_paths(&repo, &[".gitignore", "src/main.rs"]);

    fs::write(dir.path().join("src/main.rs"), "after\n").unwrap();
    app.refresh_repository();

    let git_sets = app.list_change_sets(ListChangeSetsRequest {
        source: Some(ChangeSetSource::GitWorktree),
        ..Default::default()
    });
    let unstaged = git_sets
        .iter()
        .find(|summary| summary.id == "git-worktree:unstaged")
        .expect("unstaged git change set should be exposed");
    let files = app.list_change_set_files(ListChangeSetFilesRequest {
        change_set_id: unstaged.id.clone(),
    });
    let diff = app
        .get_change_set_file_diff(GetChangeSetFileDiffRequest {
            change_set_id: unstaged.id.clone(),
            path: "src/main.rs".into(),
        })
        .unwrap();
    let persisted_git_sets = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::GitWorktree),
        )
        .unwrap();

    assert_eq!(unstaged.status, ChangeSetStatus::Live);
    assert_eq!(files.files.len(), 1);
    assert_eq!(diff.old_text.as_deref(), Some("before\n"));
    assert_eq!(diff.new_text.as_deref(), Some("after\n"));
    assert!(persisted_git_sets.is_empty());
}

#[test]
fn cancel_prompt_finalizes_pending_change_set() {
    // Regression: cancel_prompt must finalize the current turn's pending
    // AgentTurn change set (status Pending → Complete) before clearing the
    // turn state. Without this the review panel falls back to the previous
    // turn's changes because selectReviewChangeSet cannot match a Pending
    // change set with message_id=None after the turn ends.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    // ── Previous turn (so there is a prior change set to fall back to) ──
    let prev_user = uuid::Uuid::new_v4();
    let prev_assistant = uuid::Uuid::new_v4();
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: prev_user,
        role: MessageRole::User,
        body: "first".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
        ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: prev_assistant,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-05-13T00:00:01Z".into(),
        ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(prev_user));
    app.ui.timeline.push(TimelineItem::Message(prev_assistant));
    app.current_turn_user_message_id = Some(prev_user);
    app.review_changes_started = false;
    app.upsert_review_file_change(
        "src/prev.rs",
        FileChangeType::Modified,
        Some("A\n".into()),
        "B\n".into(),
    );
    assert!(app.persist_current_turn_file_changes());

    // ── Current turn (will be cancelled) ──
    let cur_user = uuid::Uuid::new_v4();
    let cur_assistant = uuid::Uuid::new_v4();
    app.ui.messages.push(ChatMessage {
        id: cur_user,
        role: MessageRole::User,
        body: "second".into(),
        created_at: "2026-05-13T00:00:10Z".into(),
        ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: cur_assistant,
        role: MessageRole::Assistant,
        body: "partial".into(),
        created_at: "2026-05-13T00:00:11Z".into(),
        ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(cur_user));
    app.ui.timeline.push(TimelineItem::Message(cur_assistant));
    app.current_turn_user_message_id = Some(cur_user);
    app.review_changes_started = false;
    app.ui.session.status = SessionStatus::Streaming;
    app.in_flight_prompt = Some(InFlightPrompt {
        task: PromptTask::test_placeholder(),
    });
    app.upsert_review_file_change(
        "src/cur.rs",
        FileChangeType::Modified,
        Some("X\n".into()),
        "Y\n".into(),
    );

    // The pending change set should exist with status Pending.
    let before = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let pending_before = before
        .iter()
        .find(|s| s.status == ChangeSetStatus::Pending)
        .expect("pending change set should exist during the turn");
    assert!(pending_before.message_id.is_none());

    // Cancel the turn.
    app.cancel_prompt().unwrap();

    // After cancel the current turn's change set must be Complete (not
    // orphaned as Pending), so the frontend can select it.
    let after = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let cur_set = after
        .iter()
        .find(|s| {
            s.message_id == Some(cur_assistant)
        })
        .expect("cancelled turn's change set should be finalized with the assistant message id");
    assert_eq!(cur_set.status, ChangeSetStatus::Complete);

    let cur_files = app.list_change_set_files(ListChangeSetFilesRequest {
        change_set_id: cur_set.id.clone(),
    });
    assert_eq!(cur_files.files.len(), 1);
    assert_eq!(cur_files.files[0].path, "src/cur.rs");
}

#[test]
fn cancel_prompt_finalizes_pending_change_set_without_assistant_message() {
    // Edge case: the turn is cancelled during a tool call before the agent
    // produced any assistant text. There is no assistant message_id to
    // attach, but the change set must still be finalized as Complete so it
    // does not disappear from the review panel.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    let cur_user = uuid::Uuid::new_v4();
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: cur_user,
        role: MessageRole::User,
        body: "edit file".into(),
        created_at: "2026-05-13T00:00:00Z".into(),
        ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(cur_user));
    app.current_turn_user_message_id = Some(cur_user);
    app.review_changes_started = false;
    app.ui.session.status = SessionStatus::Streaming;
    app.in_flight_prompt = Some(InFlightPrompt {
        task: PromptTask::test_placeholder(),
    });
    app.upsert_review_file_change(
        "src/main.rs",
        FileChangeType::Modified,
        Some("A\n".into()),
        "B\n".into(),
    );

    app.cancel_prompt().unwrap();

    let after = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let cur_set = after
        .iter()
        .find(|s| s.status == ChangeSetStatus::Complete)
        .expect("cancelled turn's change set should be finalized as Complete");
    assert!(cur_set.message_id.is_none());

    let cur_files = app.list_change_set_files(ListChangeSetFilesRequest {
        change_set_id: cur_set.id.clone(),
    });
    assert_eq!(cur_files.files.len(), 1);
    assert_eq!(cur_files.files[0].path, "src/main.rs");
}
