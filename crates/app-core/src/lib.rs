mod application;
mod bootstrap;
mod editor_files;
mod file_tracker;
mod paths;
mod reducer;
pub mod settings;
pub mod startup_perf;
mod workspace_files;

pub use application::{
    Application, UiPatchCursor, UiSnapshotUpdate, normalize_path_for_storage,
    normalize_tracked_path,
};
pub use paths::AppPaths;

#[cfg(test)]
mod tests {
    use super::{AppPaths, Application, UiPatchCursor, UiSnapshotUpdate};
    use acp_core::ClientEvent;
    use session_store::SessionStore;
    use workspace_model::{
        AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, DiffLineKind, FileChangeType,
        MessageRole, SessionConfigCategory, SessionConfigChoice, SessionConfigControl,
        SessionConfigSource, SessionConfigState, SessionFileChange, TerminalOutput, ThinkingStatus,
        TimelineItem, ToolStatus, UserPromptContent,
    };

    use tempfile::tempdir;

    #[test]
    fn send_prompt_adds_messages_and_tool_updates() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);
        let starting_messages = app.ui.messages.len();
        app.send_prompt("Review current changes").unwrap();
        assert!(app.ui.messages.len() > starting_messages);
        assert!(app.ui.messages.iter().any(|message| message.role
            == workspace_model::MessageRole::Assistant
            && message.body.contains("Real ACP session connected")));
        assert!(
            app.ui
                .tools
                .iter()
                .any(|tool| tool.status == ToolStatus::Succeeded)
        );
        assert!(app.ui.agent_plan.is_empty());
    }

    #[test]
    fn background_prompt_polling_does_not_block_when_no_events_are_ready() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        app.send_prompt_background("first turn").unwrap();
        while app.has_in_flight_prompt() {
            app.poll_prompt_progress();
        }

        app.send_prompt_background("second turn").unwrap();
        std::thread::scope(|scope| {
            let poll = scope.spawn(|| app.poll_prompt_progress());
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert!(
                poll.is_finished(),
                "poll_prompt_progress should not block the UI thread"
            );
            poll.join().unwrap();
        });
    }

    #[test]
    fn lightweight_ui_update_emits_full_then_incremental_patches() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);
        let mut cursor = UiPatchCursor::default();

        let initial = app
            .lightweight_ui_update(&mut cursor)
            .expect("first update should seed the patch cursor with a full snapshot");
        let initial_timeline_len = match initial {
            UiSnapshotUpdate::Full(snapshot) => snapshot.timeline.len(),
            UiSnapshotUpdate::Patch(_) => panic!("first update must be full"),
        };
        assert!(app.lightweight_ui_update(&mut cursor).is_none());

        app.send_prompt_background("hello from patch cursor")
            .unwrap();

        let next = app
            .lightweight_ui_update(&mut cursor)
            .expect("new user prompt should produce a patch");
        let patch = match next {
            UiSnapshotUpdate::Patch(patch) => patch,
            UiSnapshotUpdate::Full(_) => panic!("same session should not re-emit full snapshots"),
        };

        assert_eq!(patch.timeline_start, initial_timeline_len);
        assert_eq!(patch.timeline.len(), 1);
        assert_eq!(patch.messages.len(), 1);
        assert!(patch.message_deltas.is_empty());
        assert_eq!(patch.messages[0].role, MessageRole::User);
        assert!(patch.messages[0].body.contains("hello from patch cursor"));
        assert!(patch.repository.is_none());
        assert!(app.lightweight_ui_update(&mut cursor).is_none());

        app.ui
            .messages
            .last_mut()
            .expect("prompt should create a message")
            .body
            .push_str(" with appended text");
        app.ui.revision += 1;

        let append_update = app
            .lightweight_ui_update(&mut cursor)
            .expect("appended body should produce a delta patch");
        let append_patch = match append_update {
            UiSnapshotUpdate::Patch(patch) => patch,
            UiSnapshotUpdate::Full(_) => panic!("append should stay incremental"),
        };
        assert!(append_patch.messages.is_empty());
        assert_eq!(append_patch.message_deltas.len(), 1);
        assert_eq!(append_patch.message_deltas[0].append, " with appended text");
        assert!(append_patch.repository.is_none());

        app.ui.repository.branch = "feature/snapshot-patch".into();
        app.ui.revision += 1;

        let repository_update = app
            .lightweight_ui_update(&mut cursor)
            .expect("repository changes should produce a patch");
        let repository_patch = match repository_update {
            UiSnapshotUpdate::Patch(patch) => patch,
            UiSnapshotUpdate::Full(_) => {
                panic!("repository updates should stay incremental for the same session")
            }
        };
        assert_eq!(
            repository_patch
                .repository
                .as_ref()
                .map(|repo| repo.branch.as_str()),
            Some("feature/snapshot-patch"),
        );
    }

    #[test]
    fn background_prompt_can_send_image_content() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);
        wait_for_image_prompt_capability(&mut app);

        app.send_prompt_content_background(vec![
            UserPromptContent::text("Describe this image"),
            UserPromptContent::image_with_thumbnail(
                "aW1hZ2U=",
                "image/png",
                Some("sample.png".into()),
                "dGh1bWI=",
                "image/png",
            ),
            UserPromptContent::file("ZmlsZQ==", Some("text/plain".into()), "notes.txt"),
        ])
        .unwrap();
        while app.has_in_flight_prompt() {
            app.poll_prompt_progress();
        }

        assert!(app.ui.messages.iter().any(|message| {
            message.body.contains("Describe this image")
                && message
                    .body
                    .contains("![Image: sample.png](data:image/png;base64,dGh1bWI=)")
                && message.body.contains("[File: notes.txt]")
        }));
        assert!(app.ui.messages.iter().any(|message| message.role
            == workspace_model::MessageRole::Assistant
            && message.body.contains("Real ACP session connected")));
    }

    #[test]
    fn new_background_prompt_clears_previous_plan() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);
        app.ui.agent_plan = vec![plan_entry(
            "Old plan",
            AgentPlanEntryPriority::High,
            AgentPlanEntryStatus::Pending,
        )];

        app.send_prompt_background("new work").unwrap();

        assert!(app.ui.agent_plan.is_empty());
    }

    #[test]
    fn reducer_syncs_session_config_into_summary_metadata() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::SessionConfigUpdated {
                state: SessionConfigState {
                    hydrated: true,
                    controls: vec![
                        select_control(
                            "model",
                            "Model",
                            SessionConfigCategory::Model,
                            "gpt-5.5",
                            &["gpt-5.4", "gpt-5.5"],
                        ),
                        select_control(
                            "mode",
                            "Mode",
                            SessionConfigCategory::Mode,
                            "plan",
                            &["plan", "code"],
                        ),
                    ],
                },
            },
        );

        assert_eq!(ui.session.model, "gpt-5.5");
        assert_eq!(ui.session.mode.as_deref(), Some("plan"));

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::SessionConfigValueChanged {
                control_id: "mode".into(),
                value_id: "code".into(),
                value_label: None,
            },
        );

        assert_eq!(ui.session.mode.as_deref(), Some("code"));
    }

    #[test]
    fn session_store_persists_current_model_and_mode() {
        let dir = tempdir().unwrap();
        let store = SessionStore::open(dir.path().join("home").as_path(), dir.path()).unwrap();
        store.create_session("session-a", "gpt-5.4").unwrap();
        store
            .update_session_model_mode("session-a", "gpt-5.5", Some("plan"))
            .unwrap();

        let (model, mode) = store
            .get_session_model_mode("session-a")
            .unwrap()
            .expect("session metadata should exist");
        assert_eq!(model, "gpt-5.5");
        assert_eq!(mode.as_deref(), Some("plan"));
    }

    #[test]
    fn app_paths_create_standard_home_data_dirs() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        paths.ensure_standard_dirs().unwrap();

        assert!(paths.root().is_dir());
        assert!(paths.config_dir().is_dir());
        assert!(paths.logs_dir().is_dir());
        assert!(paths.sessions_dir().is_dir());
        assert!(paths.workspaces_dir().is_dir());
    }

    #[test]
    fn reducer_keeps_idle_status_for_system_session_metadata() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.session.status = workspace_model::SessionStatus::Idle;
        let message_count = ui.messages.len();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: workspace_model::MessageRole::System,
                content: "ACP capabilities: loadSession=true, resume_id=none".into(),
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: workspace_model::MessageRole::System,
                content: "Connected to ACP workspace D:\\work\\kodex with model Claude-Opus-4.6-1M"
                    .into(),
            },
        );

        assert_eq!(ui.session.status, workspace_model::SessionStatus::Idle);
        assert_eq!(ui.messages.len(), message_count);
    }

    #[test]
    fn reducer_marks_open_tools_interrupted_on_abnormal_turn_finish() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "call_refusal".into(),
                parent_id: None,
                name: "PowerShell".into(),
                kind: "shell".into(),
                summary: "正在运行命令".into(),
                is_subagent: false,
                raw_input: None,
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::TurnFinished {
                stop_reason: "refusal".into(),
            },
        );

        let tool = ui.tools.iter().find(|tool| tool.call_id == "call_refusal");
        assert_eq!(ui.session.status, workspace_model::SessionStatus::Idle);
        assert_eq!(
            tool.map(|tool| &tool.status),
            Some(&ToolStatus::Interrupted)
        );
        assert!(tool.and_then(|tool| tool.error.as_ref()).is_some());
        assert_eq!(
            ui.inspector_sections
                .last()
                .map(|section| section.title.as_str()),
            Some("轮次异常")
        );
    }

    #[test]
    fn reducer_replaces_plan_without_touching_timeline_state() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        let message_count = ui.messages.len();
        let timeline_count = ui.timeline.len();
        let tool_count = ui.tools.len();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::PlanUpdated {
                entries: vec![
                    plan_entry(
                        "Read code",
                        AgentPlanEntryPriority::High,
                        AgentPlanEntryStatus::Pending,
                    ),
                    plan_entry(
                        "Make change",
                        AgentPlanEntryPriority::Medium,
                        AgentPlanEntryStatus::InProgress,
                    ),
                ],
            },
        );

        assert_eq!(ui.agent_plan.len(), 2);
        assert_eq!(ui.messages.len(), message_count);
        assert_eq!(ui.timeline.len(), timeline_count);
        assert_eq!(ui.tools.len(), tool_count);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::PlanUpdated {
                entries: vec![plan_entry(
                    "Verify result",
                    AgentPlanEntryPriority::Low,
                    AgentPlanEntryStatus::Completed,
                )],
            },
        );

        assert_eq!(ui.agent_plan.len(), 1);
        assert_eq!(ui.agent_plan[0].content, "Verify result");
        assert_eq!(ui.messages.len(), message_count);
        assert_eq!(ui.timeline.len(), timeline_count);
        assert_eq!(ui.tools.len(), tool_count);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::TurnFinished {
                stop_reason: "end_turn".into(),
            },
        );
        assert!(ui.agent_plan.is_empty());

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::PlanUpdated {
                entries: vec![plan_entry(
                    "Interrupted task",
                    AgentPlanEntryPriority::Low,
                    AgentPlanEntryStatus::Pending,
                )],
            },
        );

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::Interrupted {
                reason: "disconnected".into(),
            },
        );
        assert!(ui.agent_plan.is_empty());
    }

    #[test]
    fn reducer_projects_codebuddy_task_tools_into_agent_plan() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "create-1".into(),
                parent_id: None,
                name: "TaskCreate".into(),
                kind: "TaskCreate".into(),
                summary: "Create task".into(),
                is_subagent: false,
                raw_input: Some(
                    r#"{"subject":"定位日志事件","description":"查看日志中的任务事件"}"#
                        .replace('\\', "")
                        .into(),
                ),
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolCompleted {
                id: "create-1".into(),
                name: Some("TaskCreate".into()),
                outcome: "Task #1 created successfully: 定位日志事件".into(),
                raw_output: Some("Task #1 created successfully: 定位日志事件".into()),
                terminal_output: None,
            },
        );

        assert_eq!(ui.agent_plan.len(), 1);
        assert_eq!(ui.agent_plan[0].id.as_deref(), Some("1"));
        assert_eq!(ui.agent_plan[0].content, "定位日志事件");
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::Pending);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "update-1".into(),
                parent_id: None,
                name: "TaskUpdate".into(),
                kind: "TaskUpdate".into(),
                summary: "Update task".into(),
                is_subagent: false,
                raw_input: Some(
                    r#"{"taskId":"1","status":"in_progress"}"#.replace('\\', "").into(),
                ),
            },
        );
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::InProgress);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "update-2".into(),
                parent_id: None,
                name: "TaskUpdate".into(),
                kind: "TaskUpdate".into(),
                summary: "Update task".into(),
                is_subagent: false,
                raw_input: Some(r#"{"taskId":"1","status":"completed"}"#.replace('\\', "").into()),
            },
        );
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::Completed);

        ui.agent_plan.clear();
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "update-3".into(),
                parent_id: None,
                name: "TaskUpdate".into(),
                kind: "TaskUpdate".into(),
                summary: "Update task".into(),
                is_subagent: false,
                raw_input: Some(
                    r#"{"taskId":"1","status":"in_progress"}"#.replace('\\', "").into(),
                ),
            },
        );
        assert_eq!(ui.agent_plan[0].content, "定位日志事件");
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::InProgress);
    }

    #[test]
    fn reducer_keeps_codebuddy_task_names_from_final_updates() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "create-2".into(),
                parent_id: None,
                name: "TaskCreate".into(),
                kind: "TaskCreate".into(),
                summary: "TaskCreate".into(),
                is_subagent: false,
                raw_input: Some("{}".into()),
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolUpdated {
                id: "create-2".into(),
                parent_id: None,
                name: None,
                kind: None,
                summary: Some("Task #2 created successfully: 修复任务名展示".into()),
                is_subagent: false,
                raw_input: Some(
                    r#"{"subject":"修复任务名展示","description":"不要显示 Task #2"}"#
                        .replace('\\', "")
                        .into(),
                ),
                raw_output: Some("Task #2 created successfully: 修复任务名展示".into()),
                terminal_output: None,
                is_partial: false,
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolCompleted {
                id: "create-2".into(),
                name: None,
                outcome: "Task #2 created successfully: 修复任务名展示".into(),
                raw_output: Some("Task #2 created successfully: 修复任务名展示".into()),
                terminal_output: None,
            },
        );

        assert_eq!(ui.agent_plan.len(), 1);
        assert_eq!(ui.agent_plan[0].id.as_deref(), Some("2"));
        assert_eq!(ui.agent_plan[0].content, "修复任务名展示");

        ui.tools[0].name = "Tool".into();
        ui.agent_plan.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "update-2".into(),
                parent_id: None,
                name: "TaskUpdate".into(),
                kind: "TaskUpdate".into(),
                summary: "TaskUpdate".into(),
                is_subagent: false,
                raw_input: Some("{}".into()),
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolUpdated {
                id: "update-2".into(),
                parent_id: None,
                name: None,
                kind: None,
                summary: Some("Updated task #2 status".into()),
                is_subagent: false,
                raw_input: Some(
                    r#"{"taskId":"2","status":"in_progress"}"#.replace('\\', "").into(),
                ),
                raw_output: Some("Updated task #2 status".into()),
                terminal_output: None,
                is_partial: false,
            },
        );

        assert_eq!(ui.agent_plan.len(), 1);
        assert_eq!(ui.agent_plan[0].content, "修复任务名展示");
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::InProgress);
    }

    #[test]
    fn reducer_projects_codebuddy_todo_write_into_agent_plan() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "todo-1".into(),
                parent_id: None,
                name: "todo: todo write".into(),
                kind: "tool".into(),
                summary: "todo: todo write".into(),
                is_subagent: false,
                raw_input: Some(
                    serde_json::json!({
                        "content": "- [ ] 查看当前 ci.yml 文件内容\n- [ ] 理解现有部署流程（测试服务器配置）\n- [x] 添加 release 分支判断逻辑"
                    })
                    .to_string(),
                ),
            },
        );

        assert_eq!(ui.agent_plan.len(), 3);
        assert_eq!(ui.agent_plan[0].content, "查看当前 ci.yml 文件内容");
        assert_eq!(ui.agent_plan[0].status, AgentPlanEntryStatus::Pending);
        assert_eq!(ui.agent_plan[2].content, "添加 release 分支判断逻辑");
        assert_eq!(ui.agent_plan[2].status, AgentPlanEntryStatus::Completed);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolUpdated {
                id: "todo-1".into(),
                parent_id: None,
                name: None,
                kind: None,
                summary: Some("Updated (107 chars)".into()),
                is_subagent: false,
                raw_input: None,
                raw_output: None,
                terminal_output: None,
                is_partial: false,
            },
        );

        assert_eq!(ui.agent_plan.len(), 3);
        assert_eq!(
            ui.agent_plan[1].content,
            "理解现有部署流程（测试服务器配置）"
        );
    }

    #[test]
    fn reducer_caps_large_tool_payloads_kept_in_ui_state() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        let huge = "x".repeat(128 * 1024);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolUpdated {
                id: "read-large".into(),
                parent_id: None,
                name: Some("Read".into()),
                kind: Some("read".into()),
                summary: Some("Read large file".into()),
                is_subagent: false,
                raw_input: Some(huge.clone()),
                raw_output: Some(huge.clone()),
                terminal_output: Some(TerminalOutput {
                    exit_code: Some(0),
                    output: huge,
                }),
                is_partial: false,
            },
        );

        let tool = ui.tools.first().expect("tool should be recorded");
        assert!(tool.raw_input.as_deref().unwrap_or_default().len() < 20 * 1024);
        assert!(tool.raw_output.as_deref().unwrap_or_default().len() < 36 * 1024);
        assert!(
            tool.terminal_output
                .as_ref()
                .map(|output| output.output.len())
                .unwrap_or_default()
                < 36 * 1024
        );
    }

    #[test]
    fn reducer_uses_fs_write_diff_for_session_changes_without_duplicate_tools() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-1".into(),
                path: "d:/work/kodex/AGENTS.md".into(),
                old_text: Some("old section".into()),
                new_text: "new section".into(),
            },
        );

        assert_eq!(ui.tools.len(), 1);
        assert!(ui.session_changes.is_empty());

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:d:\\work\\kodex\\AGENTS.md".into(),
                path: "d:\\work\\kodex\\AGENTS.md".into(),
                old_text: Some("before\nold section\nafter\n".into()),
                new_text: "before\nnew section\nafter\n".into(),
            },
        );

        assert_eq!(ui.tools.len(), 1);
        assert_eq!(ui.session_changes.len(), 1);
        assert_eq!(
            ui.session_changes[0].old_text.as_deref(),
            Some("before\nold section\nafter\n")
        );
        assert_eq!(
            ui.session_changes[0].new_text,
            "before\nnew section\nafter\n"
        );
        assert_eq!(ui.tools[0].diff_previews.len(), 1);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-2".into(),
                path: "d:/work/kodex/AGENTS.md".into(),
                old_text: Some("after".into()),
                new_text: "done".into(),
            },
        );
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:d:\\work\\kodex\\AGENTS.md".into(),
                path: "d:\\work\\kodex\\AGENTS.md".into(),
                old_text: Some("before\nnew section\nafter\n".into()),
                new_text: "before\nnew section\ndone\n".into(),
            },
        );

        assert_eq!(ui.tools.len(), 2);
        assert_eq!(ui.session_changes.len(), 1);
        assert_eq!(
            ui.session_changes[0].old_text.as_deref(),
            Some("before\nold section\nafter\n")
        );
        assert_eq!(
            ui.session_changes[0].new_text,
            "before\nnew section\ndone\n"
        );
        let first_tool_added = ui.tools[0].diff_previews[0].hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        let second_tool_added = ui.tools[1].diff_previews[0].hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Added)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first_tool_added, vec!["new section"]);
        assert_eq!(second_tool_added, vec!["done"]);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:\\\\?\\D:\\work\\kodex\\AGENTS.md".into(),
                path: "\\\\?\\D:\\work\\kodex\\AGENTS.md".into(),
                old_text: Some("before\nnew section\ndone\n".into()),
                new_text: "before\nnew section\nfinal\n".into(),
            },
        );

        assert_eq!(ui.session_changes.len(), 1);
        assert_eq!(
            ui.session_changes[0].new_text,
            "before\nnew section\nfinal\n"
        );
    }

    #[test]
    fn manual_editor_save_does_not_update_agent_session_changes() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save(
            "src/main.ts",
            Some("before\n".into()),
            "agent edit\n".into(),
        );
        app.record_manual_editor_save(
            "src/main.ts",
            Some("agent edit\n".into()),
            "manual edit\n".into(),
        );

        assert!(app.ui.session_changes.is_empty());
        assert!(app.ui.review_changes.is_empty());
    }

    #[test]
    fn manual_editor_save_is_separate_from_current_turn_changes() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save(
            "src/new.ts",
            Some("before\n".into()),
            "before\nextra\n".into(),
        );

        assert!(app.ui.session_changes.is_empty());
        assert!(app.ui.review_changes.is_empty());
    }

    #[test]
    fn manual_editor_save_revert_does_not_create_agent_changes() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save("src/main.ts", Some("before\n".into()), "after\n".into());
        app.record_manual_editor_save("src/main.ts", Some("after\n".into()), "before\n".into());

        assert!(app.ui.session_changes.is_empty());
    }

    #[test]
    fn manual_editor_save_line_ending_only_diff_does_not_create_agent_changes() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        app.record_manual_editor_save(
            "src/main.ts",
            Some("alpha\nbeta\n".into()),
            "alpha\nchanged\n".into(),
        );
        app.record_manual_editor_save(
            "src/main.ts",
            Some("alpha\nchanged\n".into()),
            "alpha\r\nbeta\r\n".into(),
        );

        assert!(app.ui.session_changes.is_empty());
    }

    #[test]
    fn reducer_skips_tool_diff_preview_when_old_text_is_missing() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-missing-old".into(),
                path: "d:/work/kodex/storyboard.ts".into(),
                old_text: None,
                new_text: "line 1\nline 2\n".into(),
            },
        );

        let tool = ui.tools.first().expect("tool should exist");
        assert_eq!(tool.call_id, "tool-edit-missing-old");
        assert_eq!(tool.diff_paths.len(), 0);
        assert_eq!(tool.diff_previews.len(), 0);
    }

    #[test]
    fn reducer_does_not_replace_good_preview_with_fragment_old_text() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        let old_text = (0..60)
            .map(|line| {
                if line == 30 {
                    "expect(page.locator('.chat')).toBeVisible();".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let new_text = old_text.replace(
            "expect(page.locator('.chat')).toBeVisible();",
            "expect(page.locator('#bottom-chat-panel')).toBeVisible();",
        );

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-fragment".into(),
                path: "app-smoke.spec.ts".into(),
                old_text: Some(old_text),
                new_text: new_text.clone(),
            },
        );

        let initial_hunks = ui.tools[0].diff_previews[0].hunks.clone();
        let initial_added = initial_hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        assert_eq!(initial_added, 1);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-fragment".into(),
                path: "app-smoke.spec.ts".into(),
                old_text: Some("toBeVisible();".into()),
                new_text,
            },
        );

        assert_eq!(ui.tools[0].diff_previews[0].hunks, initial_hunks);
    }

    #[test]
    fn reducer_does_not_replace_good_preview_with_synthetic_full_file_addition() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-exact".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: Some("const first = 1;\nconst second = 2;\n".into()),
                new_text: "const first = 1;\nconst second = 2;\nconst third = 3;\n".into(),
            },
        );

        let initial_hunks = ui.tools[0].diff_previews[0].hunks.clone();
        let initial_added = initial_hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        assert_eq!(initial_added, 1);

        let whole_file_text = (1..=870)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:smokeTest/tests/app-smoke.spec.ts".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: None,
                new_text: whole_file_text,
            },
        );

        assert_eq!(ui.tools[0].diff_previews[0].hunks, initial_hunks);
        assert!(
            ui.session_changes.is_empty(),
            "synthetic full-file fallback should not create a bogus +870 session change"
        );
    }

    #[test]
    fn reducer_does_not_replace_good_preview_with_synthetic_fragment_to_full_file_diff() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        let fragment_old =
            "async function openPromptPanel(page: Page) {\n  await openPromptPanel(page);\n}\n";
        let fragment_new = "\
async function openPromptPanel(page: Page) {
  await openPromptPanel(page);
}

async function clickCanvasNewMenuItem(page: Page, itemText: string) {
  await page.getByText(itemText, { exact: true }).click();
}
";
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-fragment-to-fragment".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: Some(fragment_old.into()),
                new_text: fragment_new.into(),
            },
        );

        let initial_hunks = ui.tools[0].diff_previews[0].hunks.clone();
        let initial_added = initial_hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        assert_eq!(initial_added, 4);

        let whole_file_text = (1..=890)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:smokeTest/tests/app-smoke.spec.ts".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: Some(fragment_new.into()),
                new_text: whole_file_text,
            },
        );

        assert_eq!(ui.tools[0].diff_previews[0].hunks, initial_hunks);
        assert!(
            ui.session_changes.is_empty(),
            "synthetic fragment-to-full-file diff should not create a bogus +890 session change"
        );
    }

    #[test]
    fn reducer_uses_trustworthy_synthetic_fs_write_to_restore_tool_card_preview() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "tool-edit-exact".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: Some("line a\nline b\n".into()),
                new_text: "line a\nline b\nline c\n".into(),
            },
        );
        let initial_hunks = ui.tools[0].diff_previews[0].hunks.clone();

        let whole_old_text = (1..=904)
            .map(|line| {
                if line == 120 {
                    "old target".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let whole_new_text = whole_old_text.replace("old target", "new target\nextra target");
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:smokeTest/tests/app-smoke.spec.ts".into(),
                path: "smokeTest/tests/app-smoke.spec.ts".into(),
                old_text: Some(whole_old_text),
                new_text: whole_new_text,
            },
        );

        assert_ne!(ui.tools[0].diff_previews[0].hunks, initial_hunks);
        let added = ui.tools[0].diff_previews[0]
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        let removed = ui.tools[0].diff_previews[0]
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn reducer_ignores_noop_fs_write_diff() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.tools.clear();
        ui.timeline.clear();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolDiff {
                id: "fs_write:notes.txt".into(),
                path: "notes.txt".into(),
                old_text: Some("same\ncontent\n".into()),
                new_text: "same\ncontent\n".into(),
            },
        );

        assert!(ui.session_changes.is_empty());
        assert!(ui.tools.is_empty());
    }

    #[test]
    fn idle_session_can_switch_agent_provided_model() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        for _ in 0..100 {
            app.poll_prompt_progress();
            if app
                .ui
                .session_config
                .controls
                .iter()
                .any(|control| control.category == SessionConfigCategory::Model)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            app.ui
                .session_config
                .controls
                .iter()
                .any(|control| control.category == SessionConfigCategory::Model),
            "mock agent should expose a model selector"
        );

        app.set_session_config_control("model", "mock-smart")
            .unwrap();

        assert_eq!(app.ui.session.model, "Mock Smart");
    }

    #[test]
    fn bootstrap_preserves_persisted_session_agent_label() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let app_paths = AppPaths::from_root(dir.path().join("home").join(".kodex"));
        let store = SessionStore::open(app_paths.root(), &workspace).unwrap();
        let session_id = uuid::Uuid::new_v4().to_string();
        store.create_session(&session_id, "Agent default").unwrap();
        store
            .update_session_agent_cli(&session_id, "goose")
            .unwrap();
        drop(store);

        let app =
            Application::bootstrap_with_app_paths(&workspace, "codebuddy --acp", app_paths.clone())
                .unwrap();

        assert_eq!(app.ui.session.id.to_string(), session_id);
        assert_eq!(app.ui.session.agent_cli.as_deref(), Some("goose"));
        assert!(app.agent_command.to_lowercase().contains("goose"));

        let reopened_store = SessionStore::open(app_paths.root(), &workspace).unwrap();
        let sessions = reopened_store.list_sessions().unwrap();
        assert_eq!(sessions[0].agent_cli.as_deref(), Some("goose"));
    }

    #[test]
    fn creating_session_uses_new_agent_session_and_agent_default_model() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        wait_for_control(&mut app, SessionConfigCategory::Model);
        app.set_session_config_control("model", "mock-smart")
            .unwrap();
        assert_eq!(app.ui.session.model, "Mock Smart");

        app.session_create(None).unwrap();

        assert_eq!(app.ui.session.model, "Agent default");
        assert!(app.ui.session_config.controls.is_empty());

        wait_for_control(&mut app, SessionConfigCategory::Model);

        assert_eq!(app.ui.session.model, "Mock Fast");
        let model_control = app
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == SessionConfigCategory::Model)
            .expect("new session should receive model state from session/new");
        assert_eq!(model_control.current_value_id, "mock-fast");
        assert!(
            model_control
                .choices
                .iter()
                .all(|choice| choice.id != "mock-loaded"),
            "new session must not use session/load model state"
        );
    }

    #[test]
    fn local_mode_survives_session_config_refresh() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        ui.session.mode = Some("Build".into());

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::SessionConfigUpdated {
                state: SessionConfigState {
                    hydrated: true,
                    controls: vec![local_mode_control("plan")],
                },
            },
        );

        assert_eq!(ui.session.mode.as_deref(), Some("Build"));
        let mode_control = ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == SessionConfigCategory::Mode)
            .expect("mode control should exist");
        assert_eq!(mode_control.current_value_id, "build");
    }

    #[test]
    fn default_build_mode_persists_across_application_bootstrap() {
        let dir = tempdir().unwrap();

        {
            let mut app = test_app(&dir);
            wait_for_control(&mut app, SessionConfigCategory::Mode);
            assert_eq!(app.ui.session.mode.as_deref(), Some("Build"));
        }

        let mut reopened = test_app(&dir);
        wait_for_control(&mut reopened, SessionConfigCategory::Mode);

        assert_eq!(reopened.ui.session.mode.as_deref(), Some("Build"));
    }

    #[test]
    fn selected_model_persists_across_application_bootstrap() {
        let dir = tempdir().unwrap();

        {
            let mut app = test_app(&dir);
            wait_for_control(&mut app, SessionConfigCategory::Model);
            app.set_session_config_control("model", "mock-smart")
                .unwrap();
            assert_eq!(app.ui.session.model, "Mock Smart");
        }

        let mut reopened = test_app(&dir);
        wait_for_control(&mut reopened, SessionConfigCategory::Model);

        assert_eq!(reopened.ui.session.model, "Mock Smart");
        let model_control = reopened
            .ui
            .session_config
            .controls
            .iter()
            .find(|control| control.category == SessionConfigCategory::Model)
            .expect("model control should exist after bootstrap");
        assert_eq!(model_control.current_value_id, "mock-smart");

        let app_root = dir.path().join("home").join(".kodex");
        let store = SessionStore::open(&app_root, dir.path()).unwrap();
        let (model, _) = store
            .get_session_model_mode(&reopened.ui.session.id.to_string())
            .unwrap()
            .expect("session metadata should exist");
        assert_eq!(model, "Mock Smart");
    }

    #[test]
    fn permission_resolution_completes_permission_tool() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolPermissionRequest {
                id: "perm-1".into(),
                name: "Read".into(),
                options: vec![],
            },
        );
        let tool = ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "perm-1")
            .expect("permission tool should exist");
        assert_eq!(tool.status, ToolStatus::Running);
        assert_eq!(
            ui.session.status,
            workspace_model::SessionStatus::WaitingForTool
        );

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolPermissionResolved {
                id: "perm-1".into(),
                outcome: "Permission selected: Allow".into(),
            },
        );

        let tool = ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "perm-1")
            .expect("permission tool should exist");
        assert_eq!(tool.status, ToolStatus::Succeeded);
        assert!(tool.permission_options.is_empty());
        assert_eq!(
            tool.permission_decision.as_deref(),
            Some("Permission selected: Allow")
        );
    }

    #[test]
    fn bootstrap_interrupts_persisted_running_tools() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let app_paths = AppPaths::from_root(dir.path().join("home").join(".kodex"));
        let store = SessionStore::open(app_paths.root(), &workspace).unwrap();
        let session_id = uuid::Uuid::new_v4().to_string();
        store.create_session(&session_id, "Agent default").unwrap();

        let tool = workspace_model::ToolInvocation {
            id: uuid::Uuid::new_v4(),
            call_id: "call-running-1".into(),
            parent_call_id: None,
            name: "Explore".into(),
            kind: "Explore".into(),
            summary: "Explore model persistence".into(),
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
            permission_decision: None,
        };
        store.insert_tool(&session_id, &tool, 1).unwrap();
        drop(store);

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("app-core should live under crates/app-core")
            .join("Cargo.toml");
        let manifest = manifest.display().to_string().replace('\\', "/");

        let app = Application::bootstrap_with_app_paths(
            &workspace,
            format!(
                "cargo run --manifest-path {} -p mock-acp-agent --quiet --",
                manifest
            ),
            app_paths.clone(),
        )
        .unwrap();

        assert_eq!(app.ui.tools.len(), 1);
        assert_eq!(app.ui.tools[0].status, ToolStatus::Interrupted);
        assert_eq!(
            app.ui.tools[0].error.as_deref(),
            Some("上次会话结束前未完成")
        );

        let reopened_store = SessionStore::open(app_paths.root(), &workspace).unwrap();
        let (_, persisted_tools, _) = reopened_store.load_session(&session_id).unwrap();
        assert_eq!(persisted_tools.len(), 1);
        assert_eq!(persisted_tools[0].status, ToolStatus::Interrupted);
    }

    #[test]
    fn session_file_diff_matches_absolute_and_relative_workspace_paths() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut app = test_app(&dir);
        app.ui.session_changes = vec![SessionFileChange {
            path: "src/main.rs".into(),
            change_type: FileChangeType::Modified,
            old_text: Some("old\n".into()),
            new_text: "new\n".into(),
            added_lines: 1,
            removed_lines: 1,
            timestamp: "1".into(),
        }];

        let relative = app.session_file_diff("src/main.rs").unwrap();
        let absolute_path = dir.path().join("src/main.rs").display().to_string();
        let absolute = app.session_file_diff(&absolute_path).unwrap();

        assert_eq!(relative.path, "src/main.rs");
        assert_eq!(absolute.path, "src/main.rs");
    }

    fn test_app(dir: &tempfile::TempDir) -> Application {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("app-core should live under crates/app-core")
            .join("Cargo.toml");
        let manifest = manifest.display().to_string().replace('\\', "/");
        Application::bootstrap_with_app_paths(
            dir.path(),
            format!(
                "cargo run --manifest-path {} -p mock-acp-agent --quiet --",
                manifest
            ),
            AppPaths::from_root(dir.path().join("home").join(".kodex")),
        )
        .unwrap()
    }

    fn wait_for_control(app: &mut Application, category: SessionConfigCategory) {
        for _ in 0..100 {
            app.poll_prompt_progress();
            if app
                .ui
                .session_config
                .controls
                .iter()
                .any(|control| control.category == category)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn wait_for_image_prompt_capability(app: &mut Application) {
        for _ in 0..100 {
            app.poll_prompt_progress();
            if app.ui.prompt_capabilities.image && app.ui.prompt_capabilities.embedded_context {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn select_control(
        id: &str,
        label: &str,
        category: SessionConfigCategory,
        current: &str,
        choices: &[&str],
    ) -> SessionConfigControl {
        SessionConfigControl {
            id: id.into(),
            label: label.into(),
            description: None,
            category,
            source: SessionConfigSource::ConfigOption,
            current_value_id: current.into(),
            current_value_label: current.into(),
            choices: choices
                .iter()
                .map(|choice| SessionConfigChoice {
                    id: (*choice).into(),
                    label: (*choice).into(),
                    description: None,
                })
                .collect(),
            enabled: true,
        }
    }

    fn plan_entry(
        content: &str,
        priority: AgentPlanEntryPriority,
        status: AgentPlanEntryStatus,
    ) -> AgentPlanEntry {
        AgentPlanEntry {
            id: None,
            content: content.into(),
            priority,
            status,
        }
    }

    fn local_mode_control(current: &str) -> SessionConfigControl {
        let choices = [
            SessionConfigChoice {
                id: "plan".into(),
                label: "Plan".into(),
                description: None,
            },
            SessionConfigChoice {
                id: "build".into(),
                label: "Build".into(),
                description: None,
            },
        ];
        let current_value_label = choices
            .iter()
            .find(|choice| choice.id == current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current.into());

        SessionConfigControl {
            id: "mode".into(),
            label: "Mode".into(),
            description: None,
            category: SessionConfigCategory::Mode,
            source: SessionConfigSource::LocalMode,
            current_value_id: current.into(),
            current_value_label,
            choices: choices.into_iter().collect(),
            enabled: true,
        }
    }

    #[test]
    fn reducer_applies_session_title_updated() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        assert_eq!(ui.session.title, "新 ACP 会话");

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::SessionTitleUpdated {
                title: "Fix authentication bug".into(),
            },
        );

        assert_eq!(ui.session.title, "Fix authentication bug");

        // Second update overwrites
        super::reducer::apply_event(
            &mut ui,
            ClientEvent::SessionTitleUpdated {
                title: "Refactor login flow".into(),
            },
        );

        assert_eq!(ui.session.title, "Refactor login flow");
    }

    #[test]
    fn reducer_hides_thought_text_and_tracks_thinking_activity() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();
        let initial_msg_count = ui.messages.len();

        super::reducer::apply_event(&mut ui, ClientEvent::ThinkingActivity { active: true });

        assert_eq!(ui.thinking_status, Some(ThinkingStatus::Active));
        assert!(
            ui.timeline
                .iter()
                .any(|item| matches!(item, TimelineItem::Thinking))
        );
        assert_eq!(ui.messages.len(), initial_msg_count);

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "Here is the answer.".into(),
            },
        );

        assert_eq!(ui.thinking_status, Some(ThinkingStatus::Completed));
        assert_eq!(ui.messages.len(), initial_msg_count + 1);
        assert!(!ui.messages[initial_msg_count].body.contains("thinking"));
    }

    #[test]
    fn reducer_tool_update_can_promote_existing_tool_to_subagent() {
        let dir = tempdir().unwrap();
        let mut ui = super::bootstrap::build_initial_ui(dir.path()).unwrap();

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolStarted {
                id: "task-1".into(),
                parent_id: None,
                name: "task".into(),
                kind: "tool".into(),
                summary: "task".into(),
                is_subagent: false,
                raw_input: Some("{}".into()),
            },
        );

        super::reducer::apply_event(
            &mut ui,
            ClientEvent::ToolUpdated {
                id: "task-1".into(),
                parent_id: Some("parent-1".into()),
                name: Some("task".into()),
                kind: Some("explore".into()),
                summary: Some("探索项目结构和状态".into()),
                is_subagent: true,
                raw_input: Some(
                    "{\"description\":\"探索项目结构和状态\",\"subagent_type\":\"explore\"}".into(),
                ),
                raw_output: None,
                terminal_output: None,
                is_partial: false,
            },
        );

        let tool = ui
            .tools
            .iter()
            .find(|tool| tool.call_id == "task-1")
            .expect("tool should exist");
        assert_eq!(tool.parent_call_id.as_deref(), Some("parent-1"));
        assert_eq!(tool.kind, "explore");
        assert!(tool.is_subagent);
    }
}
