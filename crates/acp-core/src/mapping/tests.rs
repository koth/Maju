use super::*;
use agent_client_protocol::schema::{
    Cost, PlanEntry, SessionMode, SessionModeId, SessionModeState, SessionNotification, TextContent,
};
use std::path::PathBuf;

mod codebuddy;
mod diff_preview;

#[test]
fn session_config_options_preserve_model_provider_meta() {
    let option: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": "kodex-provider/byok/claude-opus-4.8",
        "options": [
            {
                "value": "kodex-provider/byok/claude-opus-4.8",
                "name": "claude-opus-4.8",
                "_meta": {
                    "provider": "byok",
                    "provider_label": "BYOK",
                    "source_provider": "timiai",
                    "source_provider_label": "TimiAI"
                }
            },
            {
                "value": "kodex-provider/byok/claude-opus-4.8",
                "name": "claude-opus-4.8",
                "_meta": {
                    "provider": "byok",
                    "provider_label": "BYOK",
                    "source_provider": "commandcode",
                    "source_provider_label": "CommandCode"
                }
            }
        ]
    }))
    .unwrap();

    let state = session_config_from_options(vec![option]);
    let model = state
        .controls
        .iter()
        .find(|control| control.id == "model")
        .unwrap();

    assert_eq!(
        model
            .choices
            .iter()
            .map(|choice| choice.provider.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("timiai"), Some("commandcode")]
    );
    assert_eq!(
        model
            .choices
            .iter()
            .map(|choice| choice.provider_label.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("TimiAI"), Some("CommandCode")]
    );
}

#[test]
fn session_config_options_normalize_agent_mode_control_to_policy_modes() {
    let option: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "mode",
        "name": "Approval Preset",
        "description": "Choose an approval and sandboxing preset",
        "category": "mode",
        "type": "select",
        "currentValue": "auto",
        "options": [
            {
                "value": "read-only",
                "name": "Read Only",
                "description": "Read files only"
            },
            {
                "value": "auto",
                "name": "Default",
                "description": "Workspace write with approvals"
            },
            {
                "value": "full-access",
                "name": "Full Access",
                "description": "No sandbox"
            }
        ]
    }))
    .unwrap();

    let state = session_config_from_options(vec![option]);
    let mode = state
        .controls
        .iter()
        .find(|control| control.id == "mode")
        .unwrap();

    assert_eq!(mode.source, SessionConfigSource::LocalMode);
    assert_eq!(mode.current_value_id, "build");
    assert_eq!(mode.current_value_label, "Build");
    assert_eq!(
        mode.choices
            .iter()
            .map(|choice| (choice.id.as_str(), choice.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("plan", "Plan"),
            ("build", "Build"),
            ("full-access", "完全访问")
        ]
    );
}

#[test]
fn session_modes_normalize_agent_mode_ids_to_policy_modes() {
    let modes = SessionModeState::new(
        SessionModeId::new("full-access"),
        vec![
            SessionMode::new("read-only", "Read Only"),
            SessionMode::new("auto", "Default"),
            SessionMode::new("full-access", "Full Access"),
        ],
    );

    let state = session_config_from_parts(None, Some(&modes), None);
    let mode = state
        .controls
        .iter()
        .find(|control| control.id == "mode")
        .unwrap();

    assert_eq!(mode.source, SessionConfigSource::LocalMode);
    assert_eq!(mode.current_value_id, "full-access");
    assert_eq!(mode.current_value_label, "完全访问");
    assert_eq!(
        mode.choices
            .iter()
            .map(|choice| (choice.id.as_str(), choice.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("plan", "Plan"),
            ("build", "Build"),
            ("full-access", "完全访问")
        ]
    );
}

#[test]
fn generic_in_progress_tool_update_preserves_raw_output() {
    let (tx, rx) = mpsc::channel();

    emit_tool_update(
        &tx,
        ToolCallUpdate::new(
            "call-run",
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::InProgress)
                .title("Run tests".to_string())
                .raw_output(serde_json::json!(
                    "Exited with code 0. Final output:\nhello\nworld\n"
                )),
        ),
    )
    .unwrap();

    match rx.try_recv().unwrap() {
        ClientEvent::ToolUpdated {
            id,
            name,
            summary,
            raw_output,
            terminal_output,
            is_partial,
            ..
        } => {
            assert_eq!(id, "call-run");
            assert_eq!(name.as_deref(), Some("Run tests"));
            assert!(
                summary
                    .as_deref()
                    .is_some_and(|text| text.contains("hello"))
            );
            assert!(
                raw_output
                    .as_deref()
                    .is_some_and(|text| text.contains("hello"))
            );
            assert_eq!(
                terminal_output,
                Some(TerminalOutput {
                    exit_code: Some(0),
                    output: "hello\nworld".to_string(),
                })
            );
            assert!(!is_partial);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn empty_text_content_does_not_emit_message_chunk() {
    let (tx, rx) = mpsc::channel();

    emit_content(
        &tx,
        MessageRole::Assistant,
        ContentBlock::Text(TextContent::new(String::new())),
    )
    .unwrap();

    assert!(rx.try_recv().is_err());
}

#[test]
fn generic_pending_tool_update_without_payload_stays_progress() {
    let (tx, rx) = mpsc::channel();

    emit_tool_update(
        &tx,
        ToolCallUpdate::new(
            "call-pending",
            ToolCallUpdateFields::new().status(ToolCallStatus::Pending),
        ),
    )
    .unwrap();

    match rx.try_recv().unwrap() {
        ClientEvent::ToolProgress { id, content } => {
            assert_eq!(id, "call-pending");
            assert_eq!(content, "Awaiting approval");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn generic_statusless_tool_update_preserves_raw_output() {
    let (tx, rx) = mpsc::channel();

    emit_tool_update(
        &tx,
        ToolCallUpdate::new(
            "call-stream",
            ToolCallUpdateFields::new().raw_output(serde_json::json!({
                "stdout": "chunk one\n"
            })),
        ),
    )
    .unwrap();

    match rx.try_recv().unwrap() {
        ClientEvent::ToolUpdated {
            id,
            summary,
            raw_output,
            ..
        } => {
            assert_eq!(id, "call-stream");
            assert!(
                summary
                    .as_deref()
                    .is_some_and(|text| text.contains("chunk"))
            );
            assert!(
                raw_output
                    .as_deref()
                    .is_some_and(|text| text.contains("chunk"))
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

struct TestWorkspace {
    root: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kodex-acp-core-mapping-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn write(&self, relative_path: &str, contents: &str) {
        let path = self.root.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn root_str(&self) -> &str {
        self.root.to_str().unwrap()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn diff_conversion_marks_added_and_removed_lines() {
    let hunks = diff_to_hunks(Some("alpha\nbeta"), "alpha\ngamma");
    assert_eq!(hunks.len(), 1);
    assert!(
        hunks[0]
            .lines
            .iter()
            .any(|line| matches!(line.kind, DiffLineKind::Removed) && line.content == "beta")
    );
    assert!(
        hunks[0]
            .lines
            .iter()
            .any(|line| matches!(line.kind, DiffLineKind::Added) && line.content == "gamma")
    );
}

#[test]
fn diff_conversion_returns_empty_for_unchanged_content() {
    let hunks = diff_to_hunks(Some("alpha\nbeta"), "alpha\nbeta");
    assert!(hunks.is_empty());
}

#[test]
fn diff_conversion_ignores_line_ending_only_changes() {
    let hunks = diff_to_hunks(Some("alpha\r\nbeta\r\n"), "alpha\nbeta\n");
    assert!(hunks.is_empty());
}

#[test]
fn kodex_context_compacted_meta_emits_structured_event() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new()),
    )
    .meta(serde_json::Map::from_iter([(
        KODEX_CONTEXT_COMPACTED_META_KEY.to_string(),
        serde_json::json!({
            "message": "上下文已压缩"
        }),
    )]));

    emit_notification(&tx, "", notification).unwrap();

    match rx.try_recv().unwrap() {
        ClientEvent::ContextCompacted { message } => {
            assert_eq!(message, "上下文已压缩");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn kodex_context_compaction_started_meta_emits_structured_event() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new()),
    )
    .meta(serde_json::Map::from_iter([(
        KODEX_CONTEXT_COMPACTION_META_KEY.to_string(),
        serde_json::json!({
            "phase": "started",
            "message": "正在压缩上下文"
        }),
    )]));

    emit_notification(&tx, "", notification).unwrap();

    match rx.try_recv().unwrap() {
        ClientEvent::ContextCompactionStarted { message } => {
            assert_eq!(message, "正在压缩上下文");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn usage_update_maps_full_breakdown_into_session_total_and_turn_delta() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(UsageUpdate::new(1700, 200_000).meta(
            serde_json::Map::from_iter([(
                "kodex.ai/usage".to_string(),
                serde_json::json!({
                    "scope": "session_total",
                    "model": "gpt-5.1",
                    "provider": "openai",
                    "agent_cli": "codex-acp",
                    "input_tokens": 900,
                    "output_tokens": 200,
                    "cache_read_tokens": 400,
                    "cache_write_tokens": 50,
                    "reasoning_tokens": 100,
                    "total_tokens": 1650,
                    "turn_delta": {
                        "input_tokens": 100,
                        "output_tokens": 30,
                        "cache_read_tokens": 40,
                        "reasoning_tokens": 10,
                        "total_tokens": 180,
                        "latency_ms": 1500,
                        "ttft_ms": 300,
                        "tokens_per_second": 42.5
                    }
                }),
            )]),
        )),
    );

    emit_notification(&tx, "", notification).unwrap();

    // First event: SessionTotal populated from top-level fields.
    let session_event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(session_event.scope, UsageEventScope::SessionTotal);
    assert_eq!(session_event.model.as_deref(), Some("gpt-5.1"));
    assert_eq!(session_event.provider.as_deref(), Some("openai"));
    assert_eq!(session_event.agent_cli.as_deref(), Some("codex-acp"));
    assert_eq!(session_event.context.used_tokens, Some(1700));
    assert_eq!(session_event.context.window_tokens, Some(200_000));
    assert_eq!(session_event.tokens.input_tokens, Some(900));
    assert_eq!(session_event.tokens.output_tokens, Some(200));
    assert_eq!(session_event.tokens.cache_read_tokens, Some(400));
    assert_eq!(session_event.tokens.cache_write_tokens, Some(50));
    assert_eq!(session_event.tokens.reasoning_tokens, Some(100));
    assert_eq!(session_event.tokens.total_tokens, Some(1650));
    // SessionTotal is sourced from top-level fields only; timing lives in
    // the nested turn_delta object, so it must be absent here.
    assert_eq!(session_event.tokens.latency_ms, None);
    assert_eq!(session_event.tokens.ttft_ms, None);
    assert_eq!(session_event.tokens.tokens_per_second, None);

    // Second event: TurnDelta populated from the nested object.
    let turn_event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(turn_event.scope, UsageEventScope::TurnDelta);
    assert_eq!(turn_event.context.used_tokens, Some(1700));
    assert_eq!(turn_event.context.window_tokens, Some(200_000));
    assert_eq!(turn_event.tokens.input_tokens, Some(100));
    assert_eq!(turn_event.tokens.output_tokens, Some(30));
    assert_eq!(turn_event.tokens.cache_read_tokens, Some(40));
    assert_eq!(turn_event.tokens.reasoning_tokens, Some(10));
    assert_eq!(turn_event.tokens.total_tokens, Some(180));
    // Timing fields are extracted from the nested turn_delta object.
    assert_eq!(turn_event.tokens.latency_ms, Some(1500));
    assert_eq!(turn_event.tokens.ttft_ms, Some(300));
    assert_eq!(turn_event.tokens.tokens_per_second, Some(42.5));

    // No further events.
    assert!(rx.try_recv().is_err());
}

#[test]
fn usage_update_without_turn_delta_emits_only_session_total() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(UsageUpdate::new(500, 100_000).meta(
            serde_json::Map::from_iter([(
                "kodex.ai/usage".to_string(),
                serde_json::json!({
                    "scope": "session_total",
                    "input_tokens": 400,
                    "output_tokens": 100,
                    "total_tokens": 500
                }),
            )]),
        )),
    );

    emit_notification(&tx, "", notification).unwrap();

    let event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(event.scope, UsageEventScope::SessionTotal);
    assert_eq!(event.tokens.input_tokens, Some(400));
    assert_eq!(event.tokens.output_tokens, Some(100));
    assert_eq!(event.tokens.total_tokens, Some(500));
    assert!(rx.try_recv().is_err(), "no TurnDelta event expected");
}

#[test]
fn usage_update_skips_zero_turn_delta() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(
            UsageUpdate::new(100, 10_000).meta(serde_json::Map::from_iter([(
                "kodex.ai/usage".to_string(),
                serde_json::json!({
                    "scope": "session_total",
                    "input_tokens": 80,
                    "output_tokens": 20,
                    "total_tokens": 100,
                    "turn_delta": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "total_tokens": 0
                    }
                }),
            )])),
        ),
    );

    emit_notification(&tx, "", notification).unwrap();

    let event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(event.scope, UsageEventScope::SessionTotal);
    assert!(rx.try_recv().is_err(), "zero turn_delta must be skipped");
}

#[test]
fn usage_update_without_metadata_emits_context_only_event() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(UsageUpdate::new(64, 8_000)),
    );

    emit_notification(&tx, "", notification).unwrap();

    let event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(event.scope, UsageEventScope::ContextSnapshot);
    assert_eq!(event.context.used_tokens, Some(64));
    assert_eq!(event.context.window_tokens, Some(8_000));
    assert!(event.tokens.input_tokens.is_none());
    assert!(event.tokens.output_tokens.is_none());
    assert!(event.tokens.total_tokens.is_none());
    assert!(rx.try_recv().is_err());
}

#[test]
fn usage_update_maps_codex_field_aliases() {
    // Codex core uses `cached_input_tokens` and `reasoning_output_tokens`; the
    // mapping layer should resolve them into the Kodex slots.
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(
            UsageUpdate::new(300, 50_000).meta(serde_json::Map::from_iter([(
                "kodex.ai/usage".to_string(),
                serde_json::json!({
                    "scope": "session_total",
                    "input_tokens": 200,
                    "cached_input_tokens": 80,
                    "output_tokens": 100,
                    "reasoning_output_tokens": 20,
                    "total_tokens": 300
                }),
            )])),
        ),
    );

    emit_notification(&tx, "", notification).unwrap();

    let event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(event.scope, UsageEventScope::SessionTotal);
    assert_eq!(event.tokens.input_tokens, Some(200));
    assert_eq!(
        event.tokens.cache_read_tokens,
        Some(80),
        "cached_input_tokens must resolve to cache_read_tokens"
    );
    assert_eq!(event.tokens.output_tokens, Some(100));
    assert_eq!(
        event.tokens.reasoning_tokens,
        Some(20),
        "reasoning_output_tokens must resolve to reasoning_tokens"
    );
    assert_eq!(event.tokens.total_tokens, Some(300));
}
#[test]
fn usage_update_maps_context_with_malformed_metadata_and_ignores_cost() {
    let (tx, rx) = mpsc::channel();
    let notification = SessionNotification::new(
        "session-1",
        SessionUpdate::UsageUpdate(
            UsageUpdate::new(33, 4096)
                .cost(Cost::new(1.23, "USD"))
                .meta(serde_json::Map::from_iter([(
                    "kodex.ai/usage".to_string(),
                    serde_json::json!({
                        "scope": 7,
                        "model": "",
                        "agent_cli": "",
                        "input_tokens": "not a number",
                        "total_tokens": -10
                    }),
                )])),
        ),
    );

    emit_notification(&tx, "", notification).unwrap();

    // Malformed meta with bad scope value and unparseable token fields:
    // the mapping layer still emits a SessionTotal event (the meta is
    // present, so we honor the cumulative-usage intent) but all token
    // breakdown slots are None. No TurnDelta event is produced because
    // the meta lacks a `turn_delta` sub-object.
    let event = match rx.try_recv().unwrap() {
        ClientEvent::UsageUpdated { usage } => usage,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(event.scope, UsageEventScope::SessionTotal);
    assert_eq!(event.model, None);
    assert_eq!(event.agent_cli, None);
    assert_eq!(event.context.used_tokens, Some(33));
    assert_eq!(event.context.window_tokens, Some(4096));
    assert_eq!(event.tokens.input_tokens, None);
    assert_eq!(event.tokens.total_tokens, None);
    let raw_json = event.raw_json.as_deref().unwrap();
    assert!(raw_json.contains("not a number"));
    assert!(!raw_json.contains("USD"));
    assert!(rx.try_recv().is_err());
}
#[test]
fn plan_update_emits_normalized_plan_event() {
    let (tx, rx) = mpsc::channel();

    emit_notification(
        &tx,
        "",
        SessionNotification::new(
            "session-1",
            SessionUpdate::Plan(Plan::new(vec![
                PlanEntry::new(
                    "Read the code",
                    AcpPlanEntryPriority::High,
                    AcpPlanEntryStatus::Pending,
                ),
                PlanEntry::new(
                    "Apply the fix",
                    AcpPlanEntryPriority::Medium,
                    AcpPlanEntryStatus::InProgress,
                ),
                PlanEntry::new(
                    "Verify behavior",
                    AcpPlanEntryPriority::Low,
                    AcpPlanEntryStatus::Completed,
                ),
            ])),
        ),
    )
    .unwrap();

    let event = rx.try_recv().unwrap();
    assert_eq!(
        event,
        ClientEvent::PlanUpdated {
            entries: vec![
                AgentPlanEntry {
                    id: None,
                    content: "Read the code".into(),
                    priority: AgentPlanEntryPriority::High,
                    status: AgentPlanEntryStatus::Pending,
                },
                AgentPlanEntry {
                    id: None,
                    content: "Apply the fix".into(),
                    priority: AgentPlanEntryPriority::Medium,
                    status: AgentPlanEntryStatus::InProgress,
                },
                AgentPlanEntry {
                    id: None,
                    content: "Verify behavior".into(),
                    priority: AgentPlanEntryPriority::Low,
                    status: AgentPlanEntryStatus::Completed,
                },
            ],
        }
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn codebuddy_tool_call_survives_session_notification_round_trip() {
    let (tx, rx) = mpsc::channel();
    let raw_payload = serde_json::json!({
        "sessionId": "019f2c43-96c4-7512-9f88-891765cc9045",
        "update": {
            "_meta": {
                "kodex.ai/toolStop": {
                    "stopKind": "agent_owned",
                    "toolCallId": "call_83adddaecdcc4253a2237350"
                }
            },
            "kind": "execute",
            "rawInput": {
                "call_id": "call_83adddaecdcc4253a2237350",
                "command": ["pwsh.exe", "-Command", "Get-ChildItem"],
                "cwd": "D:\\work\\kodex",
                "source": "agent",
                "started_at_ms": 1783154209982_i64,
                "turn_id": "019f2c45-ceb3-7a52-8d92-81fcdcdb15c6"
            },
            "sessionUpdate": "tool_call",
            "status": "in_progress",
            "title": "Get-ChildItem",
            "toolCallId": "call_83adddaecdcc4253a2237350"
        }
    });

    // Simulate the real code path: raw JSON → SessionNotification → re-serialized Value
    let notification: SessionNotification =
        serde_json::from_value(raw_payload).expect("CodeBuddy tool_call should deserialize");
    let round_tripped = serde_json::to_value(&notification).expect("should re-serialize");

    // Verify the CodeBuddy extension fields survived the round-trip
    let update = round_tripped
        .get("update")
        .expect("update should exist after round-trip");
    assert_eq!(
        update.get("sessionUpdate").and_then(Value::as_str),
        Some("tool_call"),
        "sessionUpdate tag should survive round-trip"
    );
    assert_eq!(
        update.get("toolCallId").and_then(Value::as_str),
        Some("call_83adddaecdcc4253a2237350"),
        "toolCallId should survive round-trip"
    );
    assert_eq!(
        update.get("status").and_then(Value::as_str),
        Some("in_progress"),
        "status should survive round-trip"
    );
    assert!(
        update.get("rawInput").is_some(),
        "rawInput should survive round-trip"
    );
    assert!(
        update.get("_meta").is_some(),
        "_meta should survive round-trip"
    );

    // Verify emit_codebuddy_notification can process the round-tripped value
    emit_notification(&tx, "", notification)
        .expect("emit_notification should not error on CodeBuddy tool_call");

    // Should emit at least a ToolStarted event
    let event = rx
        .try_recv()
        .expect("should emit a tool event after round-trip");
    match &event {
        ClientEvent::ToolStarted { id, name, .. } => {
            assert_eq!(id, "call_83adddaecdcc4253a2237350");
            assert!(!name.is_empty());
        }
        _ => panic!("expected ToolStarted, got {event:?}"),
    }
}
