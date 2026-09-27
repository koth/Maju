use super::*;

/// Persist a message into the session's store (fork ordinal / candidate
/// resolution walks the FULL persisted history, not the UI window).
fn persist_message(app: &mut Application, id: uuid::Uuid, role: &str, body: &str, seq: i64) {
    app.store
        .insert_message(
            &app.ui.session.id.to_string(),
            &id.to_string(),
            role,
            body,
            seq,
        )
        .unwrap();
}

#[test]
fn fork_turn_ordinal_counts_turn_opening_user_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let u1 = uuid::Uuid::new_v4();
    let a1 = uuid::Uuid::new_v4();
    let steer = uuid::Uuid::new_v4();
    let a2 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let a3 = uuid::Uuid::new_v4();
    persist_message(&mut app, u1, "User", "第一个问题", 1);
    persist_message(&mut app, a1, "Assistant", "第一个回答", 2);
    app.store
        .insert_steer_message(
            &app.ui.session.id.to_string(),
            &steer.to_string(),
            "补充一下",
            3,
        )
        .unwrap();
    persist_message(&mut app, a2, "Assistant", "补充后的回答", 4);
    persist_message(&mut app, u2, "User", "第二个问题", 5);
    persist_message(&mut app, a3, "Assistant", "第二个回答", 6);

    // 第一条助手消息属于第 1 轮。
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a1.to_string()).unwrap(),
        1
    );
    // steer 不开新轮次：它前后的助手消息都属于第 1 轮。
    assert_eq!(
        app.fork_turn_ordinal_for_message(&steer.to_string())
            .unwrap(),
        1
    );
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a2.to_string()).unwrap(),
        1
    );
    // 第二条用户消息是第 2 轮的起点，其后的助手消息同属第 2 轮。
    assert_eq!(
        app.fork_turn_ordinal_for_message(&u2.to_string()).unwrap(),
        2
    );
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a3.to_string()).unwrap(),
        2
    );
    app.session.shutdown();
}

#[test]
fn fork_turn_ordinal_skips_compact_intercepts() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let u1 = uuid::Uuid::new_v4();
    let a1 = uuid::Uuid::new_v4();
    let compact = uuid::Uuid::new_v4();
    let a2 = uuid::Uuid::new_v4();
    persist_message(&mut app, u1, "User", "问题", 1);
    persist_message(&mut app, a1, "Assistant", "回答", 2);
    persist_message(&mut app, compact, "User", "/compact", 3);
    persist_message(&mut app, a2, "Assistant", "压缩后的回答", 4);
    // `/compact` 拦截不产生后端轮次：压缩后的回答仍属于第 1 轮，
    // 分叉切点必须锚在第 1 轮的 turn/end 上。
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a2.to_string()).unwrap(),
        1
    );
    app.session.shutdown();
}

#[test]
fn fork_turn_ordinal_rejects_message_before_any_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let only = uuid::Uuid::new_v4();
    persist_message(&mut app, only, "System", "系统通知", 1);

    let error = app
        .fork_turn_ordinal_for_message(&only.to_string())
        .unwrap_err();
    assert!(
        error.contains("还没有已完成的对话轮次"),
        "error must explain there is no completed turn: {error}"
    );
    app.session.shutdown();
}

#[test]
fn fork_turn_ordinal_rejects_unknown_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let unknown = uuid::Uuid::new_v4();
    let error = app
        .fork_turn_ordinal_for_message(&unknown.to_string())
        .unwrap_err();
    assert!(
        error.contains("未找到该消息"),
        "error must explain the message was not found: {error}"
    );
    app.session.shutdown();
}

#[test]
fn fork_turn_ordinal_reaches_turns_outside_the_ui_window() {
    // 长会话只加载尾部窗口：更早轮次的消息不在 ui 里，但分叉序号必须
    // 仍能从全量历史解析出来。
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let u1 = uuid::Uuid::new_v4();
    let a1 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let a2 = uuid::Uuid::new_v4();
    persist_message(&mut app, u1, "User", "第一轮", 1);
    persist_message(&mut app, a1, "Assistant", "第一轮回复", 2);
    persist_message(&mut app, u2, "User", "第二轮", 3);
    persist_message(&mut app, a2, "Assistant", "第二轮回复", 4);
    // 只保留第二轮在 UI 窗口里。
    app.ui
        .messages
        .retain(|message| message.id == u2 || message.id == a2);
    app.ui.timeline = vec![TimelineItem::Message(u2), TimelineItem::Message(a2)];

    // 第一轮的消息不在 UI 里，但序号解析仍然正确。
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a1.to_string()).unwrap(),
        1
    );
    assert_eq!(
        app.fork_turn_ordinal_for_message(&u1.to_string()).unwrap(),
        1
    );
    assert_eq!(
        app.fork_turn_ordinal_for_message(&a2.to_string()).unwrap(),
        2
    );
    app.session.shutdown();
}

#[test]
fn fork_candidates_list_every_turn_from_full_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let u1 = uuid::Uuid::new_v4();
    let a1 = uuid::Uuid::new_v4();
    let steer = uuid::Uuid::new_v4();
    let a2 = uuid::Uuid::new_v4();
    let compact = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let a3 = uuid::Uuid::new_v4();
    persist_message(&mut app, u1, "User", "第一轮的问题", 1);
    persist_message(&mut app, a1, "Assistant", "第一轮的回复", 2);
    app.store
        .insert_steer_message(
            &app.ui.session.id.to_string(),
            &steer.to_string(),
            "补充",
            3,
        )
        .unwrap();
    persist_message(&mut app, a2, "Assistant", "补充后的回复", 4);
    persist_message(&mut app, compact, "User", "/compact", 5);
    persist_message(&mut app, u2, "User", "第二轮的问题", 6);
    persist_message(&mut app, a3, "Assistant", "第二轮的回复", 7);

    let candidates = app.session_fork_candidates().unwrap();
    // steer 与 /compact 都不是轮次：两个真实轮次，各出一条候选。
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    assert_eq!(candidates[0].turn_ordinal, 1);
    assert_eq!(candidates[0].user_message_id, u1);
    assert!(candidates[0].user_excerpt.contains("第一轮的问题"));
    // 轮内最后一条非空回复作为该轮摘要。
    assert!(
        candidates[0].reply_excerpt.contains("补充后的回复"),
        "{candidates:?}"
    );
    assert_eq!(candidates[1].turn_ordinal, 2);
    assert_eq!(candidates[1].user_message_id, u2);
    assert!(candidates[1].user_excerpt.contains("第二轮"));
    assert!(candidates[1].reply_excerpt.contains("第二轮的回复"));
    app.session.shutdown();
}

#[test]
fn fork_prompt_anchor_resolves_text_and_occurrence() {
    // The dsh fork anchors on the target turn's prompt text; repeated prompts
    // must count their occurrence so a repeated send forks the right turn, and
    // steer/compact messages must resolve to the turn that owns them.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let u1 = uuid::Uuid::new_v4();
    let a1 = uuid::Uuid::new_v4();
    let steer = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let a2 = uuid::Uuid::new_v4();
    let compact = uuid::Uuid::new_v4();
    let u3 = uuid::Uuid::new_v4();
    let a3 = uuid::Uuid::new_v4();
    persist_message(&mut app, u1, "User", "同一个问题", 1);
    persist_message(&mut app, a1, "Assistant", "回答一", 2);
    app.store
        .insert_steer_message(
            &app.ui.session.id.to_string(),
            &steer.to_string(),
            "补充说明",
            3,
        )
        .unwrap();
    persist_message(&mut app, a2, "Assistant", "回答二", 4);
    persist_message(&mut app, compact, "User", "/compact", 5);
    persist_message(&mut app, u3, "User", "重复的问题", 6);
    persist_message(&mut app, a3, "Assistant", "回答三", 7);

    // 第一轮：文本首次出现 → occurrence 1。
    let (text, occurrence) = app.fork_prompt_anchor(&u1.to_string());
    assert_eq!(text.as_deref(), Some("同一个问题"));
    assert_eq!(occurrence, 1);
    // 重复发送的相同文本：第二次出现 → occurrence 2。
    persist_message(&mut app, u2, "User", "同一个问题", 7);
    let (text, occurrence) = app.fork_prompt_anchor(&u2.to_string());
    assert_eq!(text.as_deref(), Some("同一个问题"));
    assert_eq!(occurrence, 2);
    // steer 归属其所在轮的 opening prompt 文本。
    let (text, occurrence) = app.fork_prompt_anchor(&steer.to_string());
    assert_eq!(text.as_deref(), Some("同一个问题"));
    assert_eq!(occurrence, 1);
    // /compact 同理。
    let (text, occurrence) = app.fork_prompt_anchor(&compact.to_string());
    assert_eq!(text.as_deref(), Some("同一个问题"));
    assert_eq!(occurrence, 1);
    // 未知消息 → 无锚点，调用方退回序号切分。
    let unknown = uuid::Uuid::new_v4();
    assert_eq!(app.fork_prompt_anchor(&unknown.to_string()), (None, 0));
    app.session.shutdown();
}
