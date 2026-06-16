use super::diff_utils::{
    canonical_text_diff, edit_input_after_text, edit_input_before_text,
    expand_tool_diff_fragment_from_disk, is_file_write_tool_identity,
    is_trustworthy_review_change_text, looks_like_fragment_to_full_file_text,
    looks_like_whole_file_addition_hunks, reverse_apply_unified_diff,
    sanitize_session_file_changes, tool_command_write_hint_paths, tool_diff_hunks,
    tool_diff_hunks_for_tracker_change, tool_event_change_paths, tool_event_hint_paths,
    tool_hunks_for_tracker_update,
};
use super::inline_think::InlineThinkFilter;
use super::titles::is_placeholder_session_title;
use super::{
    Application, ModelSelection, current_timestamp, humanize_acp_disconnect_reason,
    normalize_tracked_path, turn_finished_notice,
};
use acp_core::{ClientEvent, RemoteSshSessionConfig, diff_to_hunks};
use std::{collections::HashMap, fs, path::PathBuf, time::Duration};
use workspace_model::{
    AgentCliId, ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind,
    DiffQuality, FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest,
    ListChangeSetsRequest, MessageRole, RemoteLinuxWorkspace, SessionAttentionState,
    SessionConfigCategory, SessionConfigChoice, SessionConfigControl, SessionConfigSource,
    SessionConfigState, SessionFileChange, SessionListItem, SessionRuntimeStatus, SessionStatus,
    ThinkingStatus, TimelineItem, ToolDiffPreview, ToolInvocation, ToolStatus, TurnFileChanges,
    WorkspaceLocation,
};

mod change_set_tests;
mod diff_tests;
mod prompt_tests;

fn init_test_git_repo(path: &std::path::Path) -> git2::Repository {
    let repo = git2::Repository::init(path).unwrap();
    fs::write(path.join(".gitignore"), "home/\n").unwrap();
    repo
}

fn commit_paths(repo: &git2::Repository, paths: &[&str]) {
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(std::path::Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let parent_oid = repo.head().ok().and_then(|head| head.target());
    let parent_commit = parent_oid.and_then(|oid| repo.find_commit(oid).ok());
    let parents = parent_commit.into_iter().collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parent_refs)
        .unwrap();
}

fn test_app(dir: &tempfile::TempDir) -> Application {
    Application::bootstrap_with_app_paths(
        dir.path(),
        mock_agent_command(),
        crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex")),
    )
    .unwrap()
}

fn mock_agent_command() -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let cargo = cargo.replace('\\', "/");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("app-core should live under crates/app-core")
        .join("Cargo.toml");
    let manifest = manifest.display().to_string().replace('\\', "/");
    format!(
        "{} run --manifest-path {} -p mock-acp-agent --quiet --",
        shell_words::quote(&cargo),
        shell_words::quote(&manifest)
    )
}

fn remote_workspace_fixture() -> RemoteLinuxWorkspace {
    RemoteLinuxWorkspace {
        profile_id: None,
        ssh_target: "alice@devbox".into(),
        ssh_port: Some(2222),
        remote_path: "/srv/project".into(),
        ssh_password: None,
        agent_cli: None,
        agent_command: None,
        local_port: Some(41000),
        remote_port: Some(41001),
    }
}

#[test]
fn active_agent_label_prefers_current_command_over_stale_persisted_label() {
    assert_eq!(
        super::active_agent_label_for_command("codex-acp", Some("CodeBuddy".into())),
        "Codex"
    );
    assert_eq!(
        super::active_agent_label_for_command("codebuddy --acp", Some("Codex".into())),
        "CodeBuddy"
    );
    assert_eq!(
        super::active_agent_label_for_command("codex-acp", Some("Codex".into())),
        "Codex"
    );
}

#[test]
fn bootstrap_session_selection_uses_matching_agent_history() {
    let recent_codebuddy = session_list_item("recent-codebuddy", Some("CodeBuddy"));
    let older_codex = session_list_item("older-codex", Some("Codex"));
    let unlabelled = session_list_item("legacy-unlabelled", None);
    let sessions = vec![
        recent_codebuddy.clone(),
        older_codex.clone(),
        unlabelled.clone(),
    ];

    let selected = super::select_session_for_agent_command(&sessions, "codex-acp")
        .expect("codex history should be selected");
    assert_eq!(selected.id, older_codex.id);

    let selected = super::select_session_for_agent_command(&sessions, "codebuddy --acp")
        .expect("codebuddy history should be selected");
    assert_eq!(selected.id, recent_codebuddy.id);

    assert!(
        super::select_session_for_agent_command(&[unlabelled], "claude-agent-acp").is_none(),
        "unlabelled legacy history should not be claimed by a different selected agent"
    );
}

#[test]
fn remote_workspace_new_session_uses_remote_agent_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let remote = remote_workspace_fixture();
    app.ui.workspace.location = WorkspaceLocation::RemoteLinux(remote);

    assert_eq!(
        app.agent_command_for_new_session(Some(AgentCliId::CodexAcp)),
        "codex-acp"
    );
    assert_eq!(
        app.agent_command_for_new_session(Some(AgentCliId::ClaudeAgentAcp)),
        "claude-agent-acp"
    );
    assert_eq!(
        app.command_for_agent_label_in_current_workspace("Codex")
            .as_deref(),
        Some("codex-acp")
    );
    assert!(
        !app.agent_command_for_new_session(Some(AgentCliId::CodexAcp))
            .contains("/"),
        "remote agent command must not use a local absolute binary path"
    );
}

fn session_list_item(id: &str, agent_cli: Option<&str>) -> SessionListItem {
    SessionListItem {
        id: id.into(),
        title: id.into(),
        status: "active".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        message_count: 0,
        acp_session_id: None,
        agent_cli: agent_cli.map(str::to_string),
        runtime_status: Default::default(),
        attention_state: Default::default(),
    }
}

#[test]
fn remote_workspace_new_session_reuses_bootstrapped_current_agent_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let mut remote = remote_workspace_fixture();
    remote.agent_cli = Some(AgentCliId::ClaudeAgentAcp);
    remote.agent_command =
        Some("/root/.kodex/remote-agents/claude-agent-acp/current/bin/claude-agent-acp".into());
    app.agent_command =
        "/root/.kodex/remote-agents/claude-agent-acp/current/bin/claude-agent-acp".into();
    app.ui.session.agent_cli = Some("Claude".into());
    app.ui.workspace.location = WorkspaceLocation::RemoteLinux(remote);

    assert_eq!(
        app.agent_command_for_new_session(Some(AgentCliId::ClaudeAgentAcp)),
        "/root/.kodex/remote-agents/claude-agent-acp/current/bin/claude-agent-acp"
    );
}

#[test]
fn remote_session_config_workspace_root_uses_remote_path_not_workspace_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let remote = remote_workspace_fixture();
    app.ui.workspace.root = PathBuf::from(remote.key());
    app.ui.workspace.location = WorkspaceLocation::RemoteLinux(remote.clone());
    let remote_ssh = RemoteSshSessionConfig {
        ssh_target: remote.ssh_target,
        ssh_port: remote.ssh_port,
        remote_workspace_root: remote.remote_path.clone(),
        local_port: 41000,
        remote_port: 41001,
        reverse_forwards: Vec::new(),
        ssh_command: None,
        ssh_password: None,
    };

    assert_eq!(
        app.session_config_workspace_root(Some(&remote_ssh)),
        remote.remote_path
    );
    assert_ne!(
        app.session_config_workspace_root(Some(&remote_ssh)),
        app.ui.workspace.root.display().to_string()
    );
}

#[test]
fn local_workspace_smoke_preserves_file_git_shell_and_restore_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    let repo = init_test_git_repo(dir.path());
    commit_paths(&repo, &[".gitignore", "README.md"]);

    let app_paths = crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex"));
    let first_session_id = {
        let mut app = Application::bootstrap_with_app_paths(
            dir.path(),
            mock_agent_command(),
            app_paths.clone(),
        )
        .unwrap();

        assert!(!app.is_remote_workspace());
        assert!(matches!(
            app.ui.workspace.location,
            WorkspaceLocation::Local
        ));
        assert_eq!(app.ui.workspace.root, dir.path());

        let root_entries = app.list_workspace_dir("").unwrap();
        assert!(
            root_entries.iter().any(|entry| entry.name == "README.md"),
            "local file tree should list workspace files: {root_entries:?}"
        );

        let original = app.editor_open_file("README.md").unwrap();
        assert_eq!(original.path, "README.md");
        assert_eq!(original.content, "hello\n");

        let saved = app
            .editor_save_file(
                "README.md",
                "hello\nlocal workspace smoke\n",
                Some(&original.version),
                false,
            )
            .unwrap();
        assert_eq!(saved.content, "hello\nlocal workspace smoke\n");

        app.refresh_repository();
        assert!(
            app.ui
                .repository
                .changed_files
                .iter()
                .any(|file| file.path == std::path::PathBuf::from("README.md")),
            "local git refresh should report README.md as changed: {:?}",
            app.ui.repository.changed_files
        );

        let diff = app
            .review_git_diff_content("README.md")
            .unwrap()
            .expect("local git review should load README.md diff");
        assert_eq!(diff.path, "README.md");
        assert_eq!(diff.old_text.as_deref(), Some("hello\n"));
        assert_eq!(diff.new_text, "hello\nlocal workspace smoke\n");

        let resolved = app.resolve_workspace_entry_for_shell("README.md").unwrap();
        let expected = dir.path().join("README.md");
        assert_eq!(
            normalize_tracked_path(
                &std::fs::canonicalize(&resolved)
                    .unwrap()
                    .display()
                    .to_string()
            ),
            normalize_tracked_path(
                &std::fs::canonicalize(&expected)
                    .unwrap()
                    .display()
                    .to_string()
            )
        );

        app.stage_files(&["README.md".into()]).unwrap();
        assert!(
            app.ui
                .repository
                .changed_files
                .iter()
                .any(|file| file.path == std::path::PathBuf::from("README.md")),
            "local git stage should keep README.md visible in the repository snapshot"
        );

        app.ui.session.id
    };

    let restored =
        Application::bootstrap_with_app_paths(dir.path(), mock_agent_command(), app_paths).unwrap();
    assert!(!restored.is_remote_workspace());
    assert!(matches!(
        restored.ui.workspace.location,
        WorkspaceLocation::Local
    ));
    assert_eq!(restored.ui.workspace.root, dir.path());
    assert_eq!(
        restored.ui.session.id, first_session_id,
        "local workspace bootstrap should restore the existing session from the local store"
    );
}

#[test]
fn stale_model_config_refresh_preserves_user_selected_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = model_config_state(
        SessionConfigSource::ConfigOption,
        "gpt-5.4",
        &["gpt-5.4", "gpt-5.5"],
    );
    app.ui.session.model = "gpt-5.4".into();
    app.authoritative_model_selection = Some(ModelSelection::new("gpt-5.4", None));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: model_config_state(
            SessionConfigSource::ConfigOption,
            "gpt-5.5",
            &["gpt-5.4", "gpt-5.5"],
        ),
    });

    assert_eq!(app.ui.session.model, "gpt-5.4");
    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(model_control.current_value_id, "gpt-5.4");
}

#[test]
fn provider_model_config_refresh_preserves_selected_provider_for_duplicate_model_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kimi-for-coding",
        &[
            ("command_code", "kimi-for-coding"),
            ("kimi_code", "kimi-for-coding"),
        ],
    );
    app.ui.session.model = "kimi-for-coding".into();
    app.authoritative_model_selection = Some(ModelSelection::new(
        "kimi-for-coding",
        Some("kimi_code".into()),
    ));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: provider_model_config_state(
            SessionConfigSource::ConfigOption,
            "kimi-for-coding",
            &[
                ("command_code", "kimi-for-coding"),
                ("kimi_code", "kimi-for-coding"),
            ],
        ),
    });

    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(
        model_control.current_value_id,
        "kodex-provider/kimi_code/kimi-for-coding"
    );
    assert_eq!(model_control.current_value_label, "kimi-for-coding");
}

#[test]
fn pending_model_restore_keeps_provider_model_visible_before_agent_ack() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/timiai/claude-opus-4.8",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    );
    app.ui.session.model = "Minimax M3".into();
    app.pending_model_restore = Some(ModelSelection::new("Minimax M3", Some("timiai".into())));

    let prepared = app.prepare_session_config_update(&provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/timiai/claude-opus-4.8",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    ));

    let model_control = prepared
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(
        model_control.current_value_id,
        "kodex-provider/timiai/Minimax M3"
    );
    assert_eq!(model_control.current_value_label, "Minimax M3");
}

#[test]
fn provider_model_config_refresh_preserves_timiai_minimax_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/timiai/Minimax M3",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    );
    app.ui.session.model = "Minimax M3".into();
    app.authoritative_model_selection =
        Some(ModelSelection::new("Minimax M3", Some("timiai".into())));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: provider_model_config_state(
            SessionConfigSource::ConfigOption,
            "kodex-provider/timiai/claude-opus-4.8",
            &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
        ),
    });

    assert_eq!(app.ui.session.model, "Minimax M3");
    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(
        model_control.current_value_id,
        "kodex-provider/timiai/Minimax M3"
    );
    assert_eq!(model_control.current_value_label, "Minimax M3");
}

#[test]
fn new_session_model_config_hydrate_infers_provider_for_duplicate_kimi_model_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: provider_model_config_state(
            SessionConfigSource::ConfigOption,
            "kimi-for-coding",
            &[
                ("commandcode", "kimi-for-coding"),
                ("kimi_code", "kimi-for-coding"),
            ],
        ),
    });

    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(
        model_control.current_value_id,
        "kodex-provider/kimi_code/kimi-for-coding"
    );
    assert_eq!(
        app.current_model_provider_for_persistence().as_deref(),
        Some("kimi_code")
    );
}

#[test]
fn live_runtime_reuse_avoids_session_load_when_switching_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let first_session_id = app.ui.session.id.to_string();

    app.session_create(None).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let second_session_id = app.ui.session.id.to_string();

    app.session_switch(&first_session_id).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);

    assert_eq!(app.ui.session.id.to_string(), first_session_id);
    assert_ne!(first_session_id, second_session_id);
    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("live runtime should retain session/new model state");
    assert!(
        model_control
            .choices
            .iter()
            .all(|choice| choice.id != "mock-loaded"),
        "reusing the live runtime must not run session/load"
    );
}

#[test]
fn switched_away_in_flight_prompt_completes_under_original_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("finish while hidden").unwrap();
    app.session_create(None).unwrap();
    let visible_session_id = app.ui.session.id.to_string();

    let running = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        running.runtime_status,
        SessionRuntimeStatus::BackgroundRunning
    );

    wait_for_session_attention(
        &mut app,
        &background_session_id,
        SessionAttentionState::CompletedUnviewed,
    );

    assert_eq!(app.ui.session.id.to_string(), visible_session_id);
    let (messages, tools, _) = app.store.load_session(&background_session_id).unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::User
                && message.body.contains("finish while hidden"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant
                && message.body.contains("Real ACP session connected"))
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.status == ToolStatus::Succeeded)
    );
}

#[test]
fn session_list_refresh_marks_hidden_completed_prompt_unviewed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("finish from session list refresh")
        .unwrap();
    app.session_create(None).unwrap();

    let completed = wait_for_session_attention_from_list_refresh(
        &mut app,
        &background_session_id,
        SessionAttentionState::CompletedUnviewed,
    );
    assert_eq!(
        completed.runtime_status,
        SessionRuntimeStatus::BackgroundIdle
    );
}

#[test]
fn background_idle_runtime_retires_and_reopens_with_session_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.set_runtime_clock_now(std::time::Instant::now());
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("retire after completion")
        .unwrap();
    app.session_create(None).unwrap();
    wait_for_session_attention(
        &mut app,
        &background_session_id,
        SessionAttentionState::CompletedUnviewed,
    );

    app.advance_runtime_clock(Duration::from_secs(10 * 60 + 1));
    app.poll_prompt_progress();

    assert!(
        !app.runtime_registry
            .entries
            .contains_key(&background_session_id),
        "idle background runtime should be reclaimed"
    );
    let retired = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(retired.runtime_status, SessionRuntimeStatus::None);
    assert_eq!(
        retired.attention_state,
        SessionAttentionState::CompletedUnviewed
    );

    app.session_switch(&background_session_id).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("session/load should hydrate model state after retirement");
    assert!(
        model_control
            .choices
            .iter()
            .any(|choice| choice.id == "mock-loaded"),
        "retired runtime should restore through session/load"
    );
    let reopened = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(reopened.attention_state, SessionAttentionState::None);
}

#[test]
fn cancel_visible_session_does_not_cancel_background_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("keep running").unwrap();
    app.session_create(None).unwrap();
    app.cancel_prompt().unwrap();

    let background = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        background.runtime_status,
        SessionRuntimeStatus::BackgroundRunning
    );
}

#[test]
fn background_permission_marks_only_owning_session_as_needing_attention() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.session_create(None).unwrap();
    let visible_session_id = app.ui.session.id.to_string();
    let mut runtime = app
        .runtime_registry
        .remove(&background_session_id)
        .expect("previous session should be backgrounded");

    app.swap_visible_state_with_runtime(&mut runtime);
    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Read".into(),
        options: vec![],
        details: None,
        input: None,
    });
    let needs_attention = app.runtime_needs_attention();
    app.swap_visible_state_with_runtime(&mut runtime);
    runtime.attention_state = if needs_attention {
        SessionAttentionState::NeedsAttention
    } else {
        SessionAttentionState::None
    };
    runtime.runtime_status = SessionRuntimeStatus::BackgroundRunning;
    app.runtime_registry.insert(runtime);

    assert_eq!(app.ui.session.id.to_string(), visible_session_id);
    assert!(
        app.ui
            .tools
            .iter()
            .all(|tool| tool.call_id != "permission-1")
    );
    let background = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        background.attention_state,
        SessionAttentionState::NeedsAttention
    );
}

#[test]
fn local_permission_selection_marks_tool_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Bash".into(),
        options: vec![],
        details: Some("Path: D:/work/repo/src/app.ts".into()),
        input: None,
    });

    app.mark_tool_permission_selected("permission-1", "allow");

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "permission-1")
        .expect("permission tool should exist");
    assert_eq!(tool.status, ToolStatus::Succeeded);
    assert!(tool.permission_options.is_empty());
    assert_eq!(
        tool.permission_decision.as_deref(),
        Some("Permission selected: allow")
    );
}

fn model_config_state(
    source: SessionConfigSource,
    current: &str,
    choices: &[&str],
) -> SessionConfigState {
    SessionConfigState {
        hydrated: true,
        controls: vec![SessionConfigControl {
            id: "model".into(),
            label: "Model".into(),
            description: None,
            category: SessionConfigCategory::Model,
            source,
            current_value_id: current.into(),
            current_value_label: current.into(),
            choices: choices
                .iter()
                .map(|choice| SessionConfigChoice {
                    id: (*choice).into(),
                    label: (*choice).into(),
                    description: None,
                    provider: None,
                })
                .collect(),
            enabled: true,
        }],
    }
}

fn provider_model_config_state(
    source: SessionConfigSource,
    current: &str,
    choices: &[(&str, &str)],
) -> SessionConfigState {
    SessionConfigState {
        hydrated: true,
        controls: vec![SessionConfigControl {
            id: "model".into(),
            label: "Model".into(),
            description: None,
            category: SessionConfigCategory::Model,
            source,
            current_value_id: current.into(),
            current_value_label: current.into(),
            choices: choices
                .iter()
                .map(|(provider, choice)| SessionConfigChoice {
                    id: (*choice).into(),
                    label: (*choice).into(),
                    description: None,
                    provider: Some((*provider).into()),
                })
                .collect(),
            enabled: true,
        }],
    }
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

fn wait_for_session_attention(
    app: &mut Application,
    session_id: &str,
    expected: SessionAttentionState,
) {
    for _ in 0..100 {
        app.poll_prompt_progress();
        if app
            .session_list()
            .unwrap()
            .into_iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.attention_state == expected)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("session {session_id} did not reach attention state {expected:?}");
}

fn wait_for_session_attention_from_list_refresh(
    app: &mut Application,
    session_id: &str,
    expected: SessionAttentionState,
) -> SessionListItem {
    for _ in 0..100 {
        if let Some(session) = app
            .session_list_after_poll()
            .unwrap()
            .into_iter()
            .find(|session| session.id == session_id)
            && session.attention_state == expected
        {
            return session;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("session {session_id} did not reach attention state {expected:?} from list refresh");
}
