use super::*;

#[test]
fn inline_think_filter_strips_complete_blocks_from_visible_text() {
    let mut filter = InlineThinkFilter::default();

    assert_eq!(
        filter.filter_chunk("好的，<think>这里是推理</think>现在开始。"),
        Some("好的，现在开始。".into())
    );
    assert_eq!(filter.flush(), None);
}

#[test]
fn inline_think_filter_strips_blocks_split_across_chunks() {
    let mut filter = InlineThinkFilter::default();

    assert_eq!(filter.filter_chunk("好<thi"), Some("好".into()));
    assert_eq!(filter.filter_chunk("nk>隐藏"), None);
    assert_eq!(filter.filter_chunk("推理</th"), None);
    assert_eq!(filter.filter_chunk("ink>，正文"), Some("，正文".into()));
    assert_eq!(filter.flush(), None);
}

#[test]
fn inline_think_filter_preserves_literal_partial_tag_text_on_flush() {
    let mut filter = InlineThinkFilter::default();

    assert_eq!(
        filter.filter_chunk("普通文本 <thi"),
        Some("普通文本 ".into())
    );
    assert_eq!(filter.flush(), Some("<thi".into()));
}

#[test]
fn placeholder_session_titles_are_not_meaningful_agent_titles() {
    assert!(is_placeholder_session_title("新会话"));
    assert!(is_placeholder_session_title("New Session"));
    assert!(!is_placeholder_session_title("修复登录流程"));
}

#[test]
fn agent_title_matching_prompt_acknowledges_protocol_sync() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.agent_title_received = false;
    app.ui.session.title = "修复登录".into();
    app.provisional_prompt_title = Some("修复登录".into());
    app.ui.messages.clear();
    app.ui.timeline.clear();

    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "修复登录".into(),
    });

    assert_eq!(app.ui.session.title, "修复登录");
    assert!(app.agent_title_received);
    assert!(!app.needs_title);
    assert!(app.provisional_prompt_title.is_none());
    let persisted = app
        .store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == app.ui.session.id.to_string())
        .unwrap();
    assert_eq!(persisted.title, "修复登录");

    app.ui.messages.push(ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: MessageRole::Assistant,
        body: "好的，我来修复登录流程。".into(),
        created_at: current_timestamp(),
    });

    if app.needs_title && !app.agent_title_received {
        app.refine_session_title();
    }

    assert_eq!(app.ui.session.title, "修复登录");
    app.session.shutdown();
}

#[test]
fn non_codex_first_prompt_sets_provisional_title_for_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.ui.session.title = "新 ACP 会话".into();
    app.provisional_prompt_title = None;

    app.send_prompt_background("修复登录").unwrap();

    assert_eq!(app.ui.session.title, "修复登录");
    assert_eq!(app.provisional_prompt_title.as_deref(), Some("修复登录"));
    app.session.shutdown();
}

#[test]
fn codex_first_prompt_waits_for_protocol_title() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Codex".into());
    app.provisional_prompt_title = None;

    app.send_prompt_background("修复登录").unwrap();

    assert_eq!(app.ui.session.title, "新 ACP 会话");
    assert!(app.provisional_prompt_title.is_none());
    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "修复登录流程".into(),
    });
    assert_eq!(app.ui.session.title, "修复登录流程");
    assert!(app.agent_title_received);
    assert!(!app.needs_title);
    app.session.shutdown();
}

#[test]
fn abnormal_turn_notice_explains_refusal_without_blocking_followup() {
    let notice = turn_finished_notice("refusal", Some("CodeBuddy"))
        .expect("refusal should produce a visible notice");

    assert!(notice.contains("CodeBuddy"));
    assert!(notice.contains("refusal"));
    assert!(notice.contains("429"));
    assert!(turn_finished_notice("end_turn", Some("CodeBuddy")).is_none());
}
