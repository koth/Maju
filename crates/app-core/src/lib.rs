mod application;
mod bootstrap;
mod file_tracker;
mod paths;
mod reducer;
pub mod settings;

pub use application::{Application, normalize_path_for_storage, normalize_tracked_path};
pub use paths::AppPaths;

#[cfg(test)]
mod tests {
    use super::{AppPaths, Application};
    use acp_core::ClientEvent;
    use session_store::SessionStore;
    use workspace_model::{
        AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, MessageRole,
        SessionConfigCategory, SessionConfigChoice, SessionConfigControl, SessionConfigSource,
        SessionConfigState, TerminalOutput, ThinkingStatus, TimelineItem, ToolStatus,
        UserPromptContent,
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
    fn creating_session_uses_new_agent_session_and_agent_default_model() {
        let dir = tempdir().unwrap();
        let mut app = test_app(&dir);

        wait_for_control(&mut app, SessionConfigCategory::Model);
        app.set_session_config_control("model", "mock-smart")
            .unwrap();
        assert_eq!(app.ui.session.model, "Mock Smart");

        app.session_create().unwrap();

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

        let tool = ui.tools.first().expect("tool should exist");
        assert_eq!(tool.parent_call_id.as_deref(), Some("parent-1"));
        assert_eq!(tool.kind, "explore");
        assert!(tool.is_subagent);
    }
}
