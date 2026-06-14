use super::*;
use agent_client_protocol::schema::{
    PlanEntry, SessionMode, SessionModeId, SessionModeState, SessionNotification,
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
        "currentValue": "kodex-provider/timiai/claude-opus-4.8",
        "options": [
            {
                "value": "kodex-provider/timiai/claude-opus-4.8",
                "name": "claude-opus-4.8",
                "_meta": {
                    "provider": "timiai"
                }
            },
            {
                "value": "kodex-provider/commandcode/claude-opus-4.8",
                "name": "claude-opus-4.8",
                "_meta": {
                    "provider": "commandcode"
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
