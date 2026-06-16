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
    assert!(is_placeholder_session_title("Untitled Session"));
    assert!(!is_placeholder_session_title("修复登录流程"));
}

#[test]
fn retry_user_message_updates_failed_prompt_and_removes_failure_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let session_id = app.ui.session.id.to_string();
    let user_id = uuid::Uuid::new_v4();
    let system_id = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.thinking_status = Some(ThinkingStatus::Active);
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "old prompt".into(),
        created_at: current_timestamp(),
    });
    app.ui.messages.push(ChatMessage {
        id: system_id,
        role: MessageRole::System,
        body: "会话已断开：boom".into(),
        created_at: current_timestamp(),
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Thinking);
    app.ui.timeline.push(TimelineItem::Message(system_id));
    app.store
        .insert_message(&session_id, &user_id.to_string(), "User", "old prompt", 1)
        .unwrap();
    app.store
        .insert_message(
            &session_id,
            &system_id.to_string(),
            "System",
            "会话已断开：boom",
            2,
        )
        .unwrap();

    app.retry_user_message_background(&user_id.to_string(), "new prompt".into())
        .unwrap();

    assert!(app.has_in_flight_prompt());
    assert_eq!(app.ui.session.status, SessionStatus::Streaming);
    assert_eq!(app.ui.timeline, vec![TimelineItem::Message(user_id)]);
    assert_eq!(app.ui.thinking_status, None);
    assert_eq!(app.ui.messages.len(), 1);
    assert_eq!(app.ui.messages[0].id, user_id);
    assert_eq!(app.ui.messages[0].body, "new prompt");

    let (messages, _tools, timeline) = app.store.load_session(&session_id).unwrap();
    assert_eq!(timeline, vec![TimelineItem::Message(user_id)]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, user_id);
    assert_eq!(messages[0].body, "new prompt");
    app.session.shutdown();
}

#[test]
fn retry_user_message_is_rejected_after_assistant_started() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();

    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "old prompt".into(),
        created_at: current_timestamp(),
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "already replying".into(),
        created_at: current_timestamp(),
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));

    let error = app
        .retry_user_message_background(&user_id.to_string(), "new prompt".into())
        .unwrap_err();

    assert!(error.to_string().contains("已经开始回复"));
    assert!(!app.has_in_flight_prompt());
    assert_eq!(
        app.ui
            .messages
            .iter()
            .find(|message| message.id == user_id)
            .unwrap()
            .body,
        "old prompt"
    );
    app.session.shutdown();
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
fn codex_first_prompt_sets_fallback_until_protocol_title() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Codex".into());
    app.provisional_prompt_title = None;

    app.send_prompt_background("修复登录").unwrap();

    assert_eq!(app.ui.session.title, "修复登录");
    assert_eq!(app.provisional_prompt_title.as_deref(), Some("修复登录"));
    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "修复登录流程".into(),
    });
    assert_eq!(app.ui.session.title, "修复登录流程");
    assert!(app.agent_title_received);
    assert!(!app.needs_title);
    app.session.shutdown();
}

#[test]
fn placeholder_agent_title_does_not_clear_codex_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.agent_title_received = false;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Codex".into());
    app.provisional_prompt_title = None;

    app.send_prompt_background("修复会话标题生成").unwrap();
    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "New Session".into(),
    });

    assert_eq!(app.ui.session.title, "修复会话标题生成");
    assert!(!app.agent_title_received);
    assert!(app.needs_title);
    assert_eq!(
        app.provisional_prompt_title.as_deref(),
        Some("修复会话标题生成")
    );
    let persisted = app
        .store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == app.ui.session.id.to_string())
        .unwrap();
    assert_eq!(persisted.title, "修复会话标题生成");
    app.session.shutdown();
}

#[test]
fn protocol_title_agents_refine_local_fallback_when_title_metadata_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.agent_title_received = false;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Codex".into());
    app.provisional_prompt_title = None;

    app.send_prompt_background("修复会话标题生成").unwrap();
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "好的，我来稳定会话标题生成".into(),
    });

    assert!(app.refine_session_title_after_turn_if_needed());
    assert_eq!(app.ui.session.title, "稳定会话标题生成");
    assert!(!app.needs_title);
    assert!(!app.agent_title_received);
    assert!(app.provisional_prompt_title.is_none());

    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "会话标题自动生成".into(),
    });
    assert_eq!(app.ui.session.title, "会话标题自动生成");
    assert!(app.agent_title_received);
    app.session.shutdown();
}

#[test]
fn claude_first_prompt_sets_fallback_until_protocol_title() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Claude".into());
    app.provisional_prompt_title = None;

    app.send_prompt_background("帮我修复登录 token 刷新失败的问题")
        .unwrap();

    assert_eq!(app.ui.session.title, "帮我修复登录 token 刷新失败的问题");
    assert_eq!(
        app.provisional_prompt_title.as_deref(),
        Some("帮我修复登录 token 刷新失败的问题")
    );
    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "修复登录 token 刷新".into(),
    });
    assert_eq!(app.ui.session.title, "修复登录 token 刷新");
    assert!(app.agent_title_received);
    assert!(!app.needs_title);
    app.session.shutdown();
}

#[test]
fn claude_session_title_matching_user_prompt_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.needs_title = true;
    app.agent_title_received = false;
    app.ui.session.title = "新 ACP 会话".into();
    app.ui.session.agent_cli = Some("Claude".into());
    app.provisional_prompt_title = None;
    let _ = app
        .store
        .update_session_title(&app.ui.session.id.to_string(), "新 ACP 会话");

    app.send_prompt_background("你看下rembg现在用的什么模型")
        .unwrap();

    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "你看下rembg现在用的什么模型".into(),
    });

    assert_eq!(app.ui.session.title, "你看下rembg现在用的什么模型");
    assert!(!app.agent_title_received);
    assert!(app.needs_title);
    assert_eq!(
        app.provisional_prompt_title.as_deref(),
        Some("你看下rembg现在用的什么模型")
    );
    let persisted = app
        .store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == app.ui.session.id.to_string())
        .unwrap();
    assert_eq!(persisted.title, "你看下rembg现在用的什么模型");

    app.apply_event_with_dirty_tracking(&ClientEvent::SessionTitleUpdated {
        title: "检查 rembg 模型配置".into(),
    });

    assert_eq!(app.ui.session.title, "检查 rembg 模型配置");
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

#[test]
fn context_length_disconnect_reason_is_human_readable() {
    let reason = humanize_acp_disconnect_reason(
        "Internal error: Requested token count exceeds the model's maximum context length of 131072 tokens.",
    );

    assert!(reason.contains("模型上下文超限"));
    assert!(!reason.contains("Internal error"));
}

#[test]
fn internal_error_json_disconnect_reason_uses_data_without_spawn_location() {
    let reason = humanize_acp_disconnect_reason(
        r#"Internal error: { "data": "plain remote startup failure", "spawned_at": "/tmp/jsonrpc.rs:1203:39" }"#,
    );

    assert_eq!(reason, "plain remote startup failure");
    assert!(!reason.contains("spawned_at"));
    assert!(!reason.contains("jsonrpc.rs"));
}

#[test]
fn remote_agent_readiness_disconnect_reason_is_human_readable() {
    let reason = humanize_acp_disconnect_reason(
        r#"Internal error: { "data": "ssh remote agent process ended before readiness was reported: missing remote provider configuration", "spawned_at": "/tmp/jsonrpc.rs:1203:39" }"#,
    );

    assert!(reason.contains("远程 ACP Agent"));
    assert!(reason.contains("missing remote provider configuration"));
    assert!(!reason.contains("Internal error"));
    assert!(!reason.contains("spawned_at"));
}

#[test]
fn streamable_http_connection_not_found_reason_is_human_readable() {
    let reason = humanize_acp_disconnect_reason(
        r#"streamable-http ACP request failed with status 409 Conflict using connection_id=Some("ecdf6795-f370-4ed1-92f2-17610a4e8257"): {"jsonrpc":"2.0","error":{"code":-32000,"message":"Connection not found. Please establish a connection first via POST /acp/connect before sending requests."},"id":null}"#,
    );

    assert!(reason.contains("CodeBuddy ACP 连接状态已失效"));
    assert!(!reason.contains("connection_id=Some"));
    assert!(!reason.contains("jsonrpc"));
}
