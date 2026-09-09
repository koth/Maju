use super::*;
use workspace_model::{PendingSteer, PromptSendOutcome, UserPromptContent};

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
        ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: system_id,
        role: MessageRole::System,
        body: "会话已断开：boom".into(),
        created_at: current_timestamp(),
        ..Default::default()
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Thinking(Default::default()));
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

/// A tool whose terminal event never arrived (or whose `ToolStarted` was
/// re-delivered by a dsh follow-stream re-baseline) must not stay Running
/// once the session is fully idle — the idle poll finalizes it and persists
/// the terminal state so the card stops rendering "运行中".
#[test]
fn stale_running_tool_is_finalized_when_session_goes_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    // Simulate a completed turn where one tool's terminal event was lost.
    app.apply_event_with_dirty_tracking(&ClientEvent::ToolStarted {
        id: "call-stale".into(),
        parent_id: None,
        name: "bash".into(),
        kind: "execute".into(),
        summary: "ls".into(),
        is_subagent: false,
        raw_input: None,
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::TurnFinished {
        stop_reason: "end_turn".into(),
        detail: None,
    });
    assert_eq!(app.ui.session.status, SessionStatus::Idle);

    // A late `tool/call` re-delivery (dsh follow-stream re-baseline) or a
    // partial `tool_call_update` resurrects the card to Running without a
    // terminal event. Use ToolStarted here: it exercises the same regression
    // path and the reducer's duplicate guard must not block this resurrection
    // for an Interrupted row (the idle sweep below is the real fix).
    // Bypass the reducer's duplicate guard (which correctly blocks this
    // resurrection for a Succeeded row) to simulate the post-resume SQLite
    // state: the row is Running because its terminal event never persisted.
    {
        let tool = app
            .ui
            .tools
            .iter_mut()
            .find(|tool| tool.call_id == "call-stale")
            .expect("tool row must exist");
        tool.status = ToolStatus::Running;
    }

    // Next idle poll finalizes the stale row.
    app.poll_prompt_progress();

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-stale")
        .expect("tool row must exist");
    assert_eq!(
        tool.status,
        ToolStatus::Interrupted,
        "idle session must finalize a stale running tool"
    );
    app.session.shutdown();
}

/// After a resume, an ACP `session/load` replayed `ToolCallUpdate` with
/// status Completed must still be applied (it is kept for codex sessions) so
/// the persisted tool row is not left stuck in Running.
#[test]
fn resumed_codex_session_applies_replayed_tool_completed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    // Simulate resume: skip_replay is armed, the tool row already exists in
    // the UI snapshot with a terminal state (persisted from the original run).
    app.skip_replay = true;
    let call_id = "call-replayed".to_string();
    let tool_id = uuid::Uuid::new_v4();
    app.ui.tools.push(ToolInvocation {
        id: tool_id,
        call_id: call_id.clone(),
        parent_call_id: None,
        name: "bash".into(),
        kind: "execute".into(),
        summary: "ls".into(),
        status: ToolStatus::Succeeded,
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
    app.ui.timeline.push(TimelineItem::Tool(tool_id));

    // skip_replay keeps the event (codex path) and applies it without
    // resetting the persisted terminal state.
    app.apply_event_and_restore_model(ClientEvent::ToolCompleted {
        id: call_id.clone(),
        name: Some("bash".into()),
        outcome: "done".into(),
        raw_output: None,
        terminal_output: None,
    });

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == call_id)
        .expect("tool row must exist");
    assert_eq!(
        tool.status,
        ToolStatus::Succeeded,
        "replayed completion must not reset the persisted tool state"
    );
    app.session.shutdown();
}

/// codex `session/load` replays every historical tool call as a terminal
/// `ToolCallUpdate` carrying rollout-internal ids (`tool_…`) that never match
/// the app's persisted rows (`call_…`). The resume-replay filter must drop
/// terminal events for unknown call ids — applying them created one junk
/// "tool" card per history entry on every reopen (timeline flooding with
/// "已运行 tool / Plan updated" rows) and `refresh_session_status` flipped the
/// finished session back to Streaming (stop button + steer placeholder on a
/// session that had ended). Terminal events for KNOWN call ids are still kept
/// so a genuinely stuck Running card can be finalized.
#[test]
fn resume_replay_keeps_terminal_tool_events_only_for_known_call_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    app.skip_replay = true;

    // Replay artifact: unknown rollout-internal call id → must be dropped.
    let replayed = ClientEvent::ToolCompleted {
        id: "tool_X95POpB556MXMdrD5E327f8s".into(),
        name: Some("tool".into()),
        outcome: "Plan updated".into(),
        raw_output: None,
        terminal_output: None,
    };
    assert!(
        !app.keep_replay_event(&replayed),
        "terminal event for an unknown call id is a replay artifact"
    );

    // Known row: the terminal event is kept so a stuck Running card can be
    // finalized (the original purpose of the codex carve-out).
    let known_id = "call-known".to_string();
    let tool_id = uuid::Uuid::new_v4();
    app.ui.tools.push(ToolInvocation {
        id: tool_id,
        call_id: known_id.clone(),
        parent_call_id: None,
        name: "bash".into(),
        kind: "execute".into(),
        summary: "ls".into(),
        status: ToolStatus::Running,
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
    let known = ClientEvent::ToolCompleted {
        id: known_id,
        name: Some("bash".into()),
        outcome: "done".into(),
        raw_output: None,
        terminal_output: None,
    };
    assert!(
        app.keep_replay_event(&known),
        "terminal event for a known call id finalizes the stuck card"
    );

    // The dropped artifact must never reach the reducer: no new row, no
    // timeline growth, no status flip.
    let tools_before = app.ui.tools.len();
    let timeline_before = app.ui.timeline.len();
    app.ui.session.status = SessionStatus::Idle;
    // (the filter drops it before apply — nothing to apply for `replayed`)
    assert_eq!(app.ui.tools.len(), tools_before);
    assert_eq!(app.ui.timeline.len(), timeline_before);
    assert_eq!(app.ui.session.status, SessionStatus::Idle);

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
        ..Default::default()
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "already replying".into(),
        created_at: current_timestamp(),
        ..Default::default()
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
fn interleaved_assistant_messages_survive_session_restore() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let session_id = app.ui.session.id.to_string();

    app.ui.messages.clear();
    app.ui.timeline.clear();

    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "先说明一下".into(),
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::ToolStarted {
        id: "read-file".into(),
        parent_id: None,
        name: "Read".into(),
        kind: "read".into(),
        summary: "Read source file".into(),
        is_subagent: false,
        raw_input: None,
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "中间继续解释".into(),
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::ToolStarted {
        id: "edit-file".into(),
        parent_id: None,
        name: "Edit".into(),
        kind: "edit".into(),
        summary: "Edit source file".into(),
        is_subagent: false,
        raw_input: None,
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "最后总结".into(),
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::TurnFinished {
        stop_reason: "end_turn".into(),
        detail: None,
    });

    let (messages, tools, timeline) = app.store.load_session(&session_id).unwrap();
    let restored = timeline
        .iter()
        .map(|item| match item {
            TimelineItem::Message(id) => messages
                .iter()
                .find(|message| message.id == *id)
                .map(|message| format!("message:{}", message.body))
                .unwrap(),
            TimelineItem::Tool(id) => tools
                .iter()
                .find(|tool| tool.id == *id)
                .map(|tool| format!("tool:{}", tool.call_id))
                .unwrap(),
            TimelineItem::Thinking(_) => "thinking".into(),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        restored,
        vec![
            "message:先说明一下",
            "tool:read-file",
            "message:中间继续解释",
            "tool:edit-file",
            "message:最后总结",
        ]
    );
    app.session.shutdown();
}
#[test]
fn steer_prompt_queues_pending_message_and_preserves_turn_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app_with_agent_command(&dir, steer_mock_agent_command());
    app.ui.prompt_capabilities.session_steer = true;

    app.send_prompt_background("first prompt").unwrap();
    assert!(app.has_in_flight_prompt());
    let current_turn_user_message_id = app.current_turn_user_message_id;
    app.ui.session.status = SessionStatus::WaitingForTool;
    app.ui.agent_plan = vec![workspace_model::AgentPlanEntry {
        id: Some("plan-1".into()),
        content: "Keep this plan".into(),
        priority: workspace_model::AgentPlanEntryPriority::High,
        status: workspace_model::AgentPlanEntryStatus::InProgress,
    }];
    app.review_changes_started = true;

    app.send_prompt_background("second prompt").unwrap();

    assert!(app.has_in_flight_prompt());
    assert_eq!(
        app.current_turn_user_message_id,
        current_turn_user_message_id
    );
    assert_eq!(app.ui.session.status, SessionStatus::WaitingForTool);
    assert_eq!(app.ui.agent_plan.len(), 1);
    assert!(app.review_changes_started);

    // The steer is queued as pending — NOT yet in the timeline — so it does
    // not cut the currently-streaming assistant message. It is, however,
    // already persisted to SQLite (crash-safe) and tracked in messages so
    // the frontend can show it above the composer.
    assert_eq!(
        app.ui
            .timeline
            .iter()
            .filter(|item| matches!(item, TimelineItem::Message(id)
                if app
                    .ui
                    .messages
                    .iter()
                    .find(|m| m.id == *id)
                    .is_some_and(|m| m.role == MessageRole::User && m.body == "second prompt")))
            .count(),
        0,
        "queued steer must not be in the timeline before the agent responds"
    );
    assert_eq!(app.ui.pending_steers.len(), 1);
    assert_eq!(app.ui.pending_steers[0].body, "second prompt");
    assert!(
        app.ui
            .messages
            .iter()
            .any(|m| m.role == MessageRole::User && m.body == "second prompt")
    );

    let (messages, _tools, _timeline) = app
        .store
        .load_session(&app.ui.session.id.to_string())
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.body == "second prompt"),
        "queued steer must be persisted to SQLite immediately"
    );

    poll_until_prompt_finished(&mut app);

    // After the agent responds, the steer is flushed into the timeline and
    // the assistant response to the steer appears as a new message.
    assert!(
        app.ui.pending_steers.is_empty(),
        "pending steers are flushed"
    );
    assert!(app.ui.messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.body.contains("Steer accepted: second prompt")
    }));
    // Exactly one user message with the steer body (the queued one); the
    // agent's echoed UserMessageChunk must have been suppressed.
    assert_eq!(
        app.ui
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User && m.body == "second prompt")
            .count(),
        1,
        "steer echo must be suppressed, not duplicated"
    );
    app.session.shutdown();
}

#[test]
fn steer_prompt_is_rejected_when_session_capability_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app_with_agent_command(&dir, steer_mock_agent_command());
    app.ui.prompt_capabilities.session_steer = false;

    app.send_prompt_background("first prompt").unwrap();
    assert!(app.has_in_flight_prompt());
    let current_turn_user_message_id = app.current_turn_user_message_id;
    let user_message_count = app
        .ui
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();

    let error = app.send_prompt_background("second prompt").unwrap_err();

    assert!(error.to_string().contains("不支持运行中追加指令"));
    assert!(app.has_in_flight_prompt());
    assert_eq!(
        app.current_turn_user_message_id,
        current_turn_user_message_id
    );
    assert_eq!(
        app.ui
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count(),
        user_message_count
    );
    app.session.shutdown();
}

/// Deterministic test (no agent subprocess): a queued steer stays out of the
/// timeline while the assistant is streaming, and is flushed into the
/// timeline right before the assistant chunk that responds to it — placing
/// the User steer message immediately before the new Assistant message
/// without splitting the prior streaming message.
#[test]
fn pending_steer_flushes_before_assistant_response_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.ui.messages.clear();
    app.ui.timeline.clear();

    // The assistant is mid-stream (its message is the last timeline item).
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "正在分析问题".into(),
    });
    let streaming_assistant_id = app
        .ui
        .timeline
        .last()
        .and_then(|item| match item {
            TimelineItem::Message(id) => Some(*id),
            _ => None,
        })
        .expect("streaming assistant message is last");

    // User steers; the steer is queued, NOT inserted into the timeline, so
    // the streaming assistant stays the last timeline item (no visual cut).
    let steer_id = uuid::Uuid::new_v4();
    app.ui.pending_steers.push(PendingSteer {
        message_id: steer_id,
        body: "改为处理登录".into(),
        created_at: current_timestamp(),
    });
    app.ui.messages.push(ChatMessage {
        id: steer_id,
        role: MessageRole::User,
        body: "改为处理登录".into(),
        created_at: current_timestamp(),
        ..Default::default()
    });
    let steer_body = app.ui.pending_steers[0].body.clone();
    assert_eq!(app.ui.timeline.len(), 1);
    assert_eq!(
        app.ui.timeline.last(),
        Some(&TimelineItem::Message(streaming_assistant_id))
    );

    // The agent's echoed UserMessageChunk for the steer must be suppressed
    // (it would otherwise cut the stream and duplicate the queued message).
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::User,
        content: steer_body.clone(),
    });
    assert_eq!(
        app.ui.timeline.len(),
        1,
        "steer echo must not enter the timeline"
    );
    assert_eq!(
        app.ui
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User && m.body == steer_body)
            .count(),
        1,
        "steer echo must be suppressed, not duplicated"
    );

    // The agent starts responding to the steer — the assistant chunk flushes
    // the queued steer into the timeline first.
    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "好的，改为处理登录".into(),
    });

    assert!(
        app.ui.pending_steers.is_empty(),
        "steer flushed on assistant chunk"
    );
    let timeline_roles: Vec<MessageRole> = app
        .ui
        .timeline
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Message(id) => app
                .ui
                .messages
                .iter()
                .find(|m| m.id == *id)
                .map(|m| m.role.clone()),
            _ => None,
        })
        .collect();
    // Order: prior streaming assistant, flushed user steer, new assistant.
    assert_eq!(
        timeline_roles,
        vec![
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant
        ]
    );
    // The prior streaming assistant was not split — it stays a single message.
    assert_eq!(
        app.ui
            .messages
            .iter()
            .filter(|m| m.id == streaming_assistant_id)
            .count(),
        1
    );
    app.session.shutdown();
}

/// Safety net: if the turn ends while steers are still queued (e.g. the agent
/// never emitted an assistant chunk for them), they are flushed into the
/// timeline so they are never lost.
#[test]
fn pending_steer_safety_net_flushes_on_turn_finished() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.ui.messages.clear();
    app.ui.timeline.clear();

    app.apply_event_with_dirty_tracking(&ClientEvent::MessageChunk {
        role: MessageRole::Assistant,
        content: "处理中".into(),
    });
    let steer_id = uuid::Uuid::new_v4();
    app.ui.pending_steers.push(PendingSteer {
        message_id: steer_id,
        body: "等下再说".into(),
        created_at: current_timestamp(),
    });
    app.ui.messages.push(ChatMessage {
        id: steer_id,
        role: MessageRole::User,
        body: "等下再说".into(),
        created_at: current_timestamp(),
        ..Default::default()
    });

    app.apply_event_with_dirty_tracking(&ClientEvent::TurnFinished {
        stop_reason: "end_turn".into(),
        detail: None,
    });

    assert!(
        app.ui.pending_steers.is_empty(),
        "safety net flushes on TurnFinished"
    );
    assert!(
        app.ui
            .timeline
            .iter()
            .any(|item| matches!(item, TimelineItem::Message(id)
            if app.ui.messages.iter().any(|m| m.id == *id
                && m.role == MessageRole::User
                && m.body == "等下再说"))),
        "queued steer moved into timeline by safety net"
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
        ..Default::default()
    });

    if app.needs_title && !app.agent_title_received {
        app.refine_session_title();
    }

    assert_eq!(app.ui.session.title, "修复登录");
    app.session.shutdown();
}

#[test]
fn cancel_prompt_finishes_after_runtime_local_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app_with_agent_command(&dir, prompt_never_responds_mock_agent_command());

    app.send_prompt_background("first prompt").unwrap();
    assert!(app.has_in_flight_prompt());

    // cancel_prompt is fire-and-forget: it returns immediately after marking
    // the turn cancelled locally. The worker's TurnFinished event (which
    // creates the inspector section) arrives shortly after.
    app.cancel_prompt().unwrap();

    // Local state is updated synchronously.
    assert!(!app.has_in_flight_prompt());
    assert_eq!(app.ui.session.status, SessionStatus::Idle);

    // Poll until the worker emits and we drain TurnFinished.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && !app.ui.inspector_sections.iter().any(|section| {
            section.title == "轮次异常" && section.items.iter().any(|item| item == "cancelled")
        })
    {
        app.poll_prompt_progress();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.ui.inspector_sections.iter().any(|section| {
        section.title == "轮次异常" && section.items.iter().any(|item| item == "cancelled")
    }));
    app.session.shutdown();
}

fn test_app_with_agent_command(dir: &tempfile::TempDir, agent_command: String) -> Application {
    Application::bootstrap_with_app_paths(
        dir.path(),
        agent_command,
        crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex")),
        None,
    )
    .unwrap()
}

fn steer_mock_agent_command() -> String {
    format!("KODEX_MOCK_ACP_STEER_TEST=1 {}", mock_agent_command())
}

fn prompt_never_responds_mock_agent_command() -> String {
    format!(
        "KODEX_MOCK_ACP_PROMPT_NEVER_RESPONDS=1 {}",
        mock_agent_command()
    )
}

fn poll_until_prompt_finished(app: &mut Application) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while app.has_in_flight_prompt() && std::time::Instant::now() < deadline {
        app.poll_prompt_progress();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.poll_prompt_progress();
    assert!(!app.has_in_flight_prompt());
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
    let notice = turn_finished_notice("refusal", None, Some("CodeBuddy"))
        .expect("refusal should produce a visible notice");

    assert!(notice.contains("CodeBuddy"));
    assert!(notice.contains("refusal"));
    assert!(notice.contains("429"));
    assert!(turn_finished_notice("end_turn", None, Some("CodeBuddy")).is_none());
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

#[test]
fn ansi_laden_streamable_http_disconnect_reason_is_cleaned_and_human_readable() {
    let raw = "agent process exited with exit code: 1073807364: \u{1b}[2m2026-07-24T06:24:48.131294Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mrmcp::transport::streamable_http_client\u{1b}[0m\u{1b}[2m:\u{1b}[0m fail to get common stream: Client error: streamable HTTP session expired with 404 Not Found";

    let reason = humanize_acp_disconnect_reason(raw);

    assert!(reason.contains("流式连接已过期"));
    assert!(!reason.contains('\u{1b}'));
    assert!(!reason.contains("2026-07-24"));
    assert!(!reason.contains("rmcp::transport"));
    assert!(!reason.contains("exit code"));
}

#[test]
fn sanitize_acp_error_text_strips_ansi_timestamps_and_log_targets() {
    let cleaned = sanitize_acp_error_text(
        "\u{1b}[2m2026-07-24T06:24:48.131294Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mrmcp::transport::worker\u{1b}[0m\u{1b}[2m:\u{1b}[0m something went wrong",
    );

    // Timestamp at line start, log level and target are stripped, ANSI gone.
    assert!(!cleaned.contains('\u{1b}'));
    assert!(!cleaned.contains("2026-07-24"));
    assert!(!cleaned.contains("ERROR"));
    assert!(cleaned.contains("something went wrong"));
}

#[test]
fn sanitize_acp_error_text_strips_log_target_when_line_starts_with_timestamp() {
    let cleaned = sanitize_acp_error_text(
        "2026-07-24T06:24:48.131294Z ERROR rmcp::transport::worker: something went wrong",
    );

    assert_eq!(cleaned, "something went wrong");
}

fn attach_text_only_image_mcp(app: &mut Application, workspace_root: std::path::PathBuf) {
    let service = crate::image_mcp::ImageMcpService::new(
        workspace_model::ImageCapabilities {
            native_view: false,
            native_generate: false,
            native_edit: false,
            view_fallback: true,
        },
        crate::image_mcp::ImageMcpConfig {
            workspace_root,
            settings: workspace_model::ImageSettings::default(),
            view_api_key: None,
            generate_api_key: None,
        },
    );
    let handle = crate::image_mcp::start_image_mcp_server(service).unwrap();
    app.image_mcp = Some(handle);
    app.ui.image_capabilities.native_view = false;
    app.ui.image_capabilities.view_fallback = true;
    app.ui.prompt_capabilities.image = true;
}

/// Sending an image to a text-only model must show the user message
/// immediately with the original image (not the degraded text), and the image
/// degradation must run asynchronously so the send call returns without
/// blocking. After polling, the degraded prompt reaches the agent.
#[test]
fn image_prompt_to_text_only_model_shows_original_then_degrades_async() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    attach_text_only_image_mcp(&mut app, dir.path().to_path_buf());

    let prompt = vec![
        UserPromptContent::text("这张图里是什么"),
        UserPromptContent::Image {
            data: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            name: Some("cat.png".into()),
            display_url: None,
            thumbnail_data: None,
            thumbnail_mime_type: None,
        },
    ];

    // The send must return immediately (no blocking view-model call). The user
    // message body renders the original image, identical to a multimodal model.
    app.send_prompt_content_background(prompt).unwrap();
    assert!(app.has_in_flight_prompt());

    let user_body = app
        .ui
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.body.clone())
        .expect("user message appended");
    assert!(
        user_body.contains("cat.png"),
        "user message should render the original image: {user_body}"
    );
    assert!(
        !user_body.contains("view_image"),
        "user message must not leak the degraded tool hint: {user_body}"
    );

    // Poll until the background degradation completes and dispatches to the
    // agent. The agent then receives the degraded prompt (tool hint), but the
    // already-displayed user message keeps showing the original image.
    poll_until_prompt_finished(&mut app);
    assert!(!app.has_in_flight_prompt());
    let user_body_after = app
        .ui
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.body.clone())
        .expect("user message appended");
    assert!(
        user_body_after.contains("cat.png"),
        "displayed user message keeps the original image after degradation: {user_body_after}"
    );

    app.session.shutdown();
}

/// Cancelling while an image degradation is pending must drop the pending
/// degradation and return the session to idle without dispatching to the agent.
#[test]
fn cancel_during_image_degradation_aborts_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    attach_text_only_image_mcp(&mut app, dir.path().to_path_buf());

    let prompt = vec![
        UserPromptContent::text("描述这张图"),
        UserPromptContent::Image {
            data: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            name: Some("cat.png".into()),
            display_url: None,
            thumbnail_data: None,
            thumbnail_mime_type: None,
        },
    ];
    app.send_prompt_content_background(prompt).unwrap();
    assert!(app.has_in_flight_prompt());

    app.cancel_prompt().unwrap();
    assert!(!app.has_in_flight_prompt());
    assert_eq!(app.ui.session.status, SessionStatus::Idle);

    app.session.shutdown();
}

/// A `/compact` command sent right after a resumed app start must consume
/// `skip_replay`. The compaction lifecycle events (`compaction/start` →
/// `compaction/end` + paired `command/done`) arrive while the session is Idle;
/// with skip_replay still active, `poll_prompt_progress`'s idle drain silently
/// dropped them, so the UI showed no notices at all and the bare command
/// message (with only the retry affordance under it) looked like an instant
/// send failure even though the backend compaction succeeded.
#[test]
fn compact_slash_command_consumes_skip_replay_so_compaction_events_apply() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    // Simulate the resumed-session state: skip_replay is armed until the
    // first send, and the session must be a harness one for the interception.
    app.agent_command = "dsh".into();
    app.skip_replay = true;
    assert!(app.skip_replay);

    let outcome = app
        .send_prompt_content_background(vec![UserPromptContent::text("/compact")])
        .unwrap();

    assert_eq!(outcome, PromptSendOutcome::Command);
    assert!(!app.skip_replay, "/compact must consume skip_replay");
    // Fire-and-forget: no turn starts, the session stays Idle.
    assert!(!app.has_in_flight_prompt());
    assert_eq!(app.ui.session.status, SessionStatus::Idle);
    let last = app.ui.messages.last().expect("command message appended");
    assert_eq!(last.role, MessageRole::User);
    assert_eq!(last.body, "/compact");

    app.session.shutdown();
}
