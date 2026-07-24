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
    Application, InFlightPrompt, ModelSelection, current_timestamp,
    humanize_acp_disconnect_reason,
    sanitize_acp_error_text,
    update_signal::AppUpdate,
    normalize_tracked_path, turn_finished_notice,
};
use acp_core::{ClientEvent, PromptTask, RemoteSshSessionConfig, diff_to_hunks};
use crate::{AppCoreRemoteControl, RemoteControl};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use workspace_model::{
    AgentCliId, ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind,
    DiffQuality, FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest,
    ListChangeSetsRequest, MessageRole, RemoteLinuxWorkspace, SessionAttentionState,
    SessionConfigCategory, SessionConfigChoice, SessionConfigControl, SessionConfigSource,
    SessionConfigState, SessionFileChange, SessionListItem, SessionRuntimeStatus, SessionStatus,
    ThinkingStatus, TimelineItem, ToolDiffPreview, ToolInvocation, ToolStatus, TurnFileChanges,
    UsageContextSnapshot, UsageEvent, UsageEventScope, UsageSummaryRequest, UsageTokenBreakdown,
    WorkspaceLocation,
};

mod change_set_tests;
mod diff_tests;
mod prompt_tests;
mod image_switch_tests;

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

/// Pre-build the `mock-acp-agent` binary exactly once (process-wide) and
/// return its filesystem path. Replaces `cargo run -p mock-acp-agent` which
/// acquired the cargo build lock on every test and serialized 75+ parallel
/// `cargo run` invocations, causing subprocess startup to exceed the 5s
/// polling timeout in `wait_for_control` / `wait_for_session_attention`.
fn mock_agent_command() -> String {
    static BINARY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let path = BINARY.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("app-core should live under crates/app-core")
            .join("Cargo.toml");
        let status = std::process::Command::new(&cargo)
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("-p")
            .arg("mock-acp-agent")
            .arg("--quiet")
            .status()
            .expect("failed to invoke cargo build for mock-acp-agent");
        assert!(status.success(), "cargo build -p mock-acp-agent failed");
        let target_dir = std::env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .and_then(|path| path.parent())
                    .expect("workspace root")
                    .join("target")
                    .to_string_lossy()
                    .into_owned()
            });
        let exe = if cfg!(windows) {
            "mock-acp-agent.exe"
        } else {
            "mock-acp-agent"
        };
        let binary =
            std::path::Path::new(&target_dir).join("debug").join(exe);
        assert!(
            binary.exists(),
            "mock-acp-agent binary not found at {}",
            binary.display()
        );
        binary.to_string_lossy().into_owned()
    });
    shell_words::quote(path).to_string()
}

fn usage_event(model: &str, total_tokens: u64, timestamp: &str) -> UsageEvent {
    UsageEvent {
        scope: UsageEventScope::TurnDelta,
        model: Some(model.into()),
        provider: Some("openai".into()),
        agent_cli: Some("codex-acp".into()),
        timestamp: Some(timestamp.into()),
        tokens: UsageTokenBreakdown {
            input_tokens: Some(total_tokens / 2),
            output_tokens: Some(total_tokens / 2),
            total_tokens: Some(total_tokens),
            ..Default::default()
        },
        context: UsageContextSnapshot {
            used_tokens: Some(total_tokens),
            window_tokens: Some(128_000),
            updated_at: Some(timestamp.into()),
        },
        raw_json: None,
    }
}

#[test]
fn usage_summary_groups_by_model_and_filters_date_range() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&ClientEvent::UsageUpdated {
        usage: usage_event("gpt-5.1", 10, "1751328000"),
    });
    app.apply_event_with_dirty_tracking(&ClientEvent::UsageUpdated {
        usage: usage_event("claude-opus-4.7", 20, "1751414400"),
    });

    // Date filter is a numeric comparison: stored `created_at` is cast to
    // INTEGER and the bound is parsed to epoch seconds. Use ISO bounds
    // (matching what the desktop UI sends) that bracket the second row.
    let rows = app.usage_summary(UsageSummaryRequest {
        from: Some("2025-07-02T00:00:00Z".into()),
        to: Some("2025-07-02T23:59:59Z".into()),
        ..Default::default()
    });

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model.as_deref(), Some("claude-opus-4.7"));
    assert_eq!(rows[0].tokens.total_tokens, Some(20));
}

#[test]
fn usage_updates_persist_restore_and_delete_with_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let session_id = app.ui.session.id.to_string();

    app.apply_event_with_dirty_tracking(&ClientEvent::UsageUpdated {
        usage: usage_event("gpt-5.1", 120, "2026-06-23T00:00:00Z"),
    });

    assert_eq!(app.ui.usage.context.used_tokens, Some(120));
    assert_eq!(app.ui.usage.session_total.total_tokens, Some(120));
    let stored = app.store.load_session_usage_snapshot(&session_id).unwrap();
    assert_eq!(stored.context.used_tokens, Some(120));
    assert_eq!(stored.session_total.total_tokens, Some(120));

    app.session_create(None).unwrap();
    assert_ne!(app.ui.session.id.to_string(), session_id);
    app.session_switch(&session_id).unwrap();
    assert_eq!(app.ui.usage.context.used_tokens, Some(120));
    assert_eq!(app.ui.usage.session_total.total_tokens, Some(120));

    app.session_delete(&session_id).unwrap();
    let rows = app
        .store
        .query_usage_summary(UsageSummaryRequest {
            all_workspaces: true,
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row.session_id.as_deref() != Some(&session_id))
    );
}

#[test]
fn web_tools_mcp_injection_respects_settings_key_and_session_kind() {
    let dir = tempfile::tempdir().unwrap();
    let app_paths = crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex"));

    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codex-acp", false).unwrap();
    assert!(servers.is_empty());
    assert!(handle.is_none());

    crate::settings::save_web_tools_settings(&app_paths, true, "brave").unwrap();
    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codex-acp", false).unwrap();
    assert!(servers.is_empty(), "missing provider key should not inject");
    assert!(handle.is_none());

    crate::settings::save_web_tools_provider_key(&app_paths, "brave", "test-secret").unwrap();
    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codex-acp", false).unwrap();
    assert_eq!(servers.len(), 1);
    let handle = handle.expect("enabled configured web tools should start MCP adapter");
    assert!(handle.url().starts_with("http://127.0.0.1:"));
    drop(handle);

    crate::settings::save_web_tools_settings(&app_paths, true, "tavily").unwrap();
    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codex-acp", false).unwrap();
    assert!(
        servers.is_empty(),
        "Tavily should require its own provider key"
    );
    assert!(handle.is_none());

    crate::settings::save_web_tools_provider_key(&app_paths, "tavily", "tvly-secret").unwrap();
    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "claude-agent-acp", false).unwrap();
    assert_eq!(servers.len(), 1);
    drop(handle.expect("Tavily-configured web tools should start MCP adapter"));

    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codex-acp", true).unwrap();
    assert!(
        servers.is_empty(),
        "remote sessions fail closed for local MCP"
    );
    assert!(handle.is_none());

    let (servers, handle) =
        super::sessions::prepare_web_tools_mcp(&app_paths, "codebuddy", false).unwrap();
    assert!(
        servers.is_empty(),
        "unsupported agent commands should not receive web tools"
    );
    assert!(handle.is_none());
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
fn restored_session_agent_command_prefers_recent_session_agent() {
    let dir = tempfile::tempdir().unwrap();
    let app_paths = crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex"));
    let recent_codebuddy = session_list_item("recent-codebuddy", Some("CodeBuddy"));
    let unlabelled = session_list_item("legacy-unlabelled", None);
    let stale = session_list_item("stale-agent", Some("goose"));

    let restored = super::agent_command_for_restored_session(
        Some(&recent_codebuddy),
        "claude-agent-acp".into(),
        &app_paths,
        false,
    );
    assert!(
        restored.to_ascii_lowercase().contains("codebuddy"),
        "recent CodeBuddy session should restore with CodeBuddy command, got {restored}"
    );
    assert!(!restored.to_ascii_lowercase().contains("claude-agent-acp"));

    let remote_restored = super::agent_command_for_restored_session(
        Some(&recent_codebuddy),
        "claude-agent-acp".into(),
        &app_paths,
        true,
    );
    assert!(remote_restored.to_ascii_lowercase().contains("codebuddy"));
    assert_eq!(
        super::agent_id_for_restored_session(Some(&recent_codebuddy)),
        Some(AgentCliId::Codebuddy)
    );

    let recent_codex = session_list_item("recent-codex", Some("Codex"));
    let bootstrapped_codex =
        "/root/.kodex/remote-agents/codex-acp/current/bin/codex-acp".to_string();
    assert_eq!(
        super::agent_command_for_restored_session(
            Some(&recent_codex),
            bootstrapped_codex.clone(),
            &app_paths,
            true,
        ),
        bootstrapped_codex,
        "remote restore must preserve the verified bootstrapped binary path"
    );

    assert_eq!(
        super::agent_command_for_restored_session(
            Some(&unlabelled),
            "claude-agent-acp".into(),
            &app_paths,
            false,
        ),
        "claude-agent-acp"
    );
    assert_eq!(
        super::agent_command_for_restored_session(
            Some(&stale),
            "claude-agent-acp".into(),
            &app_paths,
            false,
        ),
        "claude-agent-acp",
        "unknown legacy labels should not override the requested default"
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
fn reject_review_file_change_reverts_modified_file_to_baseline() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.rs"), "one\n").unwrap();
    let repo = init_test_git_repo(dir.path());
    commit_paths(&repo, &["main.rs"]);

    let app_paths = crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex"));
    let mut app = Application::bootstrap_with_app_paths(
        dir.path(),
        mock_agent_command(),
        app_paths,
    )
    .unwrap();

    fs::write(dir.path().join("main.rs"), "one\ntwo\n").unwrap();
    app.refresh_repository();
    assert!(
        app.ui
            .repository
            .changed_files
            .iter()
            .any(|file| file.path == PathBuf::from("main.rs")),
        "modified file should appear in repository snapshot"
    );

    app.reject_review_file_change("main.rs").unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        "one\n",
        "reject should restore the file to its git baseline"
    );
    app.refresh_repository();
    assert!(
        app.ui
            .repository
            .changed_files
            .iter()
            .all(|file| file.path != PathBuf::from("main.rs")),
        "repository snapshot should no longer report the reverted file"
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
        "kodex-provider/byok/kimi_code/kimi-for-coding"
    );
    assert_eq!(model_control.current_value_label, "kimi-for-coding");
}

#[test]
fn pending_model_restore_keeps_provider_model_visible_before_agent_ack() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/byok/timiai/claude-opus-4.8",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    );
    app.ui.session.model = "Minimax M3".into();
    app.pending_model_restore = Some(ModelSelection::new("Minimax M3", Some("timiai".into())));

    let prepared = app.prepare_session_config_update(&provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/byok/timiai/claude-opus-4.8",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    ));

    let model_control = prepared
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(
        model_control.current_value_id,
        "kodex-provider/byok/timiai/Minimax M3"
    );
    assert_eq!(model_control.current_value_label, "Minimax M3");
}

#[test]
fn provider_model_config_refresh_preserves_timiai_minimax_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "kodex-provider/byok/timiai/Minimax M3",
        &[("timiai", "claude-opus-4.8"), ("timiai", "Minimax M3")],
    );
    app.ui.session.model = "Minimax M3".into();
    app.authoritative_model_selection =
        Some(ModelSelection::new("Minimax M3", Some("timiai".into())));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: provider_model_config_state(
            SessionConfigSource::ConfigOption,
            "kodex-provider/byok/timiai/claude-opus-4.8",
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
        "kodex-provider/byok/timiai/Minimax M3"
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
        "kodex-provider/byok/kimi_code/kimi-for-coding"
    );
    assert_eq!(
        app.current_model_provider_for_persistence().as_deref(),
        Some("kimi_code")
    );
}

#[test]
fn persist_session_model_mode_stores_provider_qualified_model_value() {
    // The session must persist the model name WITH its provider so reopening
    // a historical session restores the provider actually used, not just the
    // bare model id (which may be shared across multiple providers).
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    app.ui.session.model = "claude-opus-4.8".into();
    app.authoritative_model_selection =
        Some(ModelSelection::new("claude-opus-4.8", Some("timiai".into())));
    app.persist_session_model_mode();

    let session_id = app.ui.session.id.to_string();
    let (stored_model, stored_provider, _mode) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    // The persisted `model` column embeds the provider so it survives reopen
    // even if the separate `model_provider` column were ever empty.
    assert_eq!(stored_model, "kodex-provider/byok/timiai/claude-opus-4.8");
    assert_eq!(stored_provider.as_deref(), Some("timiai"));
    // The display form strips the provider prefix back to the bare label.
    assert_eq!(
        super::config::display_model_from_persisted(&stored_model),
        "claude-opus-4.8"
    );
    assert_eq!(super::config::provider_from_model_value(&stored_model), Some("timiai"));
}

#[test]
fn requalify_persisted_model_recovers_provider_when_column_null() {
    // A historical session may have its provider only embedded in the
    // provider-qualified `model` value while the separate `model_provider`
    // column is NULL (pre-migration rows). Re-qualification must recover the
    // provider from the qualified value instead of downgrading it to a bare
    // model name, otherwise the provider is lost across reopens.
    use super::config::requalify_persisted_model;

    let (model, provider) = requalify_persisted_model(
        "claude-opus-4.8",
        "kodex-provider/byok/timiai/claude-opus-4.8",
        None,
    );
    assert_eq!(model, "kodex-provider/byok/timiai/claude-opus-4.8");
    assert_eq!(provider.as_deref(), Some("timiai"));

    // An explicit provider column wins and is preserved.
    let (model, provider) = requalify_persisted_model(
        "claude-opus-4.8",
        "kodex-provider/byok/timiai/claude-opus-4.8",
        Some("timiai"),
    );
    assert_eq!(model, "kodex-provider/byok/timiai/claude-opus-4.8");
    assert_eq!(provider.as_deref(), Some("timiai"));

    // A bare model with no recoverable provider is not fabricated; the caller
    // must not invent a provider here.
    let (model, provider) = requalify_persisted_model("shared-model", "shared-model", None);
    assert_eq!(model, "shared-model");
    assert!(provider.is_none());
}

#[test]
fn restore_pending_model_selection_skips_guess_when_provider_unrecoverable() {
    // When a session's provider cannot be recovered (e.g. a pre-migration row
    // downgraded to a bare model name) and the model is offered by more than
    // one provider, restore must not silently commit the first matching
    // provider. It leaves `pending_model_restore` and
    // `authoritative_model_selection` untouched so a later, richer config
    // update can resolve it instead of switching the session to a guessed
    // provider.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let state = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "shared-model",
        &[("timiai", "shared-model"), ("commandcode", "shared-model")],
    );
    app.ui.session.model = "shared-model".into();
    app.pending_model_restore = Some(ModelSelection::new("shared-model", None));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: state.clone(),
    });

    assert!(
        app.authoritative_model_selection.is_none(),
        "must not commit a guessed provider when the model is ambiguous"
    );
    assert!(
        app.pending_model_restore.is_some(),
        "pending restore must remain so a richer config update can resolve it"
    );
}

#[test]
fn prepare_session_config_does_not_guess_provider_for_ambiguous_bare_model() {
    // Repro for "reopen session A → shows session B's provider": a session
    // persisted with a bare model id and no model_provider (pre-migration or
    // bare-persisted row) is reopened while the live agent catalog offers the
    // same model id under two providers. `prepare_session_config_update` must
    // NOT silently qualify the model control to the first matching provider,
    // because that both displays the wrong provider and corrupts the persisted
    // row so every later reopen keeps the guessed provider.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let session_id = app.ui.session.id.to_string();
    app.store
        .update_session_model_mode_provider(&session_id, "shared-model", None, Some("Build"))
        .unwrap();
    app.ui.session.model = "shared-model".into();
    app.pending_model_restore = Some(ModelSelection::new("shared-model", None));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: provider_model_config_state(
            SessionConfigSource::ConfigOption,
            "shared-model",
            &[("commandcode", "shared-model"), ("timiai", "shared-model")],
        ),
    });

    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert!(
        super::config::provider_from_model_value(&model_control.current_value_id).is_none(),
        "must not commit a guessed provider when the bare model is ambiguous: {}",
        model_control.current_value_id
    );

    let (stored_model, stored_provider, _) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "shared-model");
    assert!(
        stored_provider.is_none(),
        "persisted provider must stay NULL instead of guessing: {:?}",
        stored_provider
    );
}

#[test]
fn restore_pending_model_keeps_qualified_provider_against_bare_agent_choices() {
    // A session stored as provider p1 + qualified model m is reopened. The
    // agent (which may run with a different global provider) reports a bare
    // current model and bare choices without provider meta. The qualified
    // provider stored on the session must survive this config refresh.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let session_id = app.ui.session.id.to_string();
    app.store
        .update_session_model_mode_provider(
            &session_id,
            "kodex-provider/byok/timiai/shared-model",
            Some("timiai"),
            Some("Build"),
        )
        .unwrap();
    app.ui.session.model = "shared-model".into();
    app.pending_model_restore = Some(ModelSelection::new(
        "kodex-provider/byok/timiai/shared-model",
        Some("timiai".into()),
    ));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: model_config_state(
            SessionConfigSource::SessionModel,
            "shared-model",
            &["shared-model", "other-model"],
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
        "kodex-provider/byok/timiai/shared-model",
        "qualified provider must survive a bare-choice agent config update"
    );

    let (stored_model, stored_provider, _) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "kodex-provider/byok/timiai/shared-model");
    assert_eq!(stored_provider.as_deref(), Some("timiai"));
}

#[test]
fn byok_custom_source_provider_round_trips_through_qualified_model_value() {
    // User-configured BYOK sources are named `custom_*` (e.g. custom_quest).
    // Before the fix `byok_source_provider_id` only knew the built-in ids, so a
    // correctly-encoded `kodex-provider/byok/custom_quest/<model>` decoded back
    // to the generic "byok" and was re-persisted as the malformed
    // `kodex-provider/byok/<model>` — losing the real provider across reopens.
    use super::config::{provider_from_model_value, provider_qualified_model_value};

    let qualified = provider_qualified_model_value("glm-5.2", Some("custom_quest"));
    assert_eq!(qualified, "kodex-provider/byok/custom_quest/glm-5.2");
    assert_eq!(provider_from_model_value(&qualified), Some("custom_quest"));

    // The generic "byok" wrapper is never a valid provider to embed: qualifying
    // with it would produce the malformed `kodex-provider/byok/<model>`.
    assert_eq!(provider_qualified_model_value("glm-5.2", Some("byok")), "glm-5.2");
    // A malformed persisted value (no source segment) is unrecoverable, not
    // the generic "byok", so the skip-guess restore path stays engaged.
    assert_eq!(provider_from_model_value("kodex-provider/byok/glm-5.2"), None);
}

#[test]
fn persist_session_model_mode_stores_custom_byok_source_provider() {
    // Repro: selecting a user-configured BYOK source (custom_*) must persist the
    // real source provider, not the generic "byok". Before the fix the decoder
    // did not recognize custom_* ids, so the authoritative selection's provider
    // leaked in as "byok" and the model column was written as the malformed
    // `kodex-provider/byok/<model>`.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    app.ui.session.model = "glm-5.2".into();
    app.authoritative_model_selection = Some(ModelSelection::new(
        "kodex-provider/byok/custom_quest/glm-5.2",
        Some("custom_quest".into()),
    ));
    app.persist_session_model_mode();

    let session_id = app.ui.session.id.to_string();
    let (stored_model, stored_provider, _mode) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "kodex-provider/byok/custom_quest/glm-5.2");
    assert_eq!(stored_provider.as_deref(), Some("custom_quest"));
    assert_eq!(
        super::config::provider_from_model_value(&stored_model),
        Some("custom_quest")
    );
}

#[test]
fn restore_pending_model_keeps_custom_byok_source_provider_against_bare_choices() {
    // Step-5 repro: a session stored as custom_* + qualified model is reopened
    // while the agent reports a bare current model and bare choices (no provider
    // meta). The custom_* source provider must survive the config refresh
    // instead of being downgraded to the generic "byok" (which would then snap
    // to whichever session wrote last).
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let session_id = app.ui.session.id.to_string();
    app.store
        .update_session_model_mode_provider(
            &session_id,
            "kodex-provider/byok/custom_quest/glm-5.2",
            Some("custom_quest"),
            Some("Build"),
        )
        .unwrap();
    app.ui.session.model = "glm-5.2".into();
    app.pending_model_restore = Some(ModelSelection::new(
        "kodex-provider/byok/custom_quest/glm-5.2",
        Some("custom_quest".into()),
    ));

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: model_config_state(
            SessionConfigSource::SessionModel,
            "glm-5.2",
            &["glm-5.2", "other-model"],
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
        "kodex-provider/byok/custom_quest/glm-5.2",
        "custom_* source provider must survive a bare-choice agent config update"
    );

    let (stored_model, stored_provider, _) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "kodex-provider/byok/custom_quest/glm-5.2");
    assert_eq!(stored_provider.as_deref(), Some("custom_quest"));
}

#[test]
fn current_model_provider_for_persistence_skips_generic_byok_wrapper() {
    // A legacy/restore selection may carry the generic "byok" wrapper as its
    // provider while the value still embeds the real source provider. The
    // generic id must be skipped (fall through to decoding the value) instead
    // of being written to the model_provider column, which would corrupt the
    // row and snap reopens to another session's provider.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    app.authoritative_model_selection = Some(ModelSelection::new(
        "kodex-provider/byok/custom_quest/glm-5.2",
        Some("byok".into()),
    ));
    assert_eq!(
        app.current_model_provider_for_persistence().as_deref(),
        Some("custom_quest"),
        "must fall through the generic byok wrapper to the embedded source provider"
    );
}

#[test]
fn persist_session_model_mode_preserves_existing_provider_when_live_selection_is_ambiguous() {
    // Repro path:
    // 1. Session A stored as provider1 + shared model m
    // 2. Later a config event only carries bare model m (shared by provider1/2)
    // 3. Without a live authoritative selection, persistence must keep provider1
    //    instead of writing NULL / bare m. Otherwise reopen infers provider2.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let session_id = app.ui.session.id.to_string();
    app.store
        .update_session_model_mode_provider(
            &session_id,
            "kodex-provider/byok/timiai/shared-model",
            Some("timiai"),
            Some("Build"),
        )
        .unwrap();

    app.ui.session.model = "shared-model".into();
    app.authoritative_model_selection = None;
    app.pending_model_restore = None;
    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "shared-model",
        &[("timiai", "shared-model"), ("commandcode", "shared-model")],
    );

    app.persist_session_model_mode();

    let (stored_model, stored_provider, _mode) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "kodex-provider/byok/timiai/shared-model");
    assert_eq!(stored_provider.as_deref(), Some("timiai"));
}

#[test]
fn persist_session_model_mode_uses_pending_restore_provider_before_agent_ack() {
    // While a restored session is waiting for agent config ack, intermediate
    // SessionConfig events must still persist the pending provider instead of
    // falling back to bare-model inference / NULL.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let session_id = app.ui.session.id.to_string();
    app.ui.session.model = "shared-model".into();
    app.authoritative_model_selection = None;
    app.pending_model_restore = Some(ModelSelection::new(
        "kodex-provider/byok/timiai/shared-model",
        Some("timiai".into()),
    ));
    app.ui.session_config = provider_model_config_state(
        SessionConfigSource::ConfigOption,
        "shared-model",
        &[("commandcode", "shared-model"), ("timiai", "shared-model")],
    );

    app.persist_session_model_mode();

    let (stored_model, stored_provider, _mode) = app
        .store
        .get_session_model_provider_mode(&session_id)
        .unwrap()
        .expect("session metadata should exist");
    assert_eq!(stored_model, "kodex-provider/byok/timiai/shared-model");
    assert_eq!(stored_provider.as_deref(), Some("timiai"));
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
fn hidden_workspace_visible_session_is_listed_as_background_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let session_id = app.ui.session.id.to_string();

    app.send_prompt_background("keep running while workspace hidden")
        .unwrap();

    let hidden_session = app
        .session_list_for_visibility(false)
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(
        hidden_session.runtime_status,
        SessionRuntimeStatus::BackgroundRunning
    );
    assert_eq!(hidden_session.status, "Streaming");
}

#[test]
fn hidden_workspace_visible_session_permission_needs_attention() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let session_id = app.ui.session.id.to_string();

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Write".into(),
        options: vec![],
        details: None,
        input: None,
    });

    let hidden_session = app
        .session_list_for_visibility(false)
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(
        hidden_session.attention_state,
        SessionAttentionState::NeedsAttention
    );
    assert_eq!(hidden_session.status, "WaitingForTool");
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
fn switching_away_from_pending_permission_marks_background_session_attention() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Write".into(),
        options: vec![],
        details: None,
        input: None,
    });

    app.session_create(None).unwrap();

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

#[test]
fn local_permission_selection_uses_option_kind_for_allow_display() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Bash".into(),
        options: vec![workspace_model::PermissionOption {
            id: "approved".into(),
            label: "Yes".into(),
            kind: "AllowOnce".into(),
        }],
        details: Some("Path: D:/work/repo/src/app.ts".into()),
        input: None,
    });

    app.mark_tool_permission_selected("permission-1", "approved");

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "permission-1")
        .expect("permission tool should exist");
    assert_eq!(
        tool.permission_decision.as_deref(),
        Some("Permission selected: Allow")
    );
    assert_eq!(tool.summary, "Permission selected: Allow");
}

#[test]
fn local_codex_patch_reject_selection_marks_edit_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "patch-1".into(),
        name: "Edit".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "approved".into(),
                label: "Yes".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "abort".into(),
                label: "No, provide feedback".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some("Path: D:/work/repo/src/app.ts".into()),
        input: None,
    });

    app.mark_tool_permission_selected("patch-1", "abort");

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "patch-1")
        .expect("permission tool should exist");
    assert_eq!(tool.status, ToolStatus::Succeeded);
    assert_eq!(tool.permission_decision.as_deref(), Some("编辑已拒绝"));
    assert_eq!(tool.summary, "编辑已拒绝");
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
                    provider_label: None,
                })
                .collect(),
            enabled: true,
        }],
    }
}

#[test]
fn session_config_update_fills_custom_provider_label_from_settings() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    crate::settings::save_custom_provider(
        &app.app_paths,
        workspace_model::CustomProviderInput {
            provider_id: Some("custom".to_string()),
            label: "Lab Provider".into(),
            endpoint: "https://api.lab.test/v1/chat/completions".into(),
            protocol: workspace_model::CustomProviderProtocol::ChatCompletions,
            api_key: "lab-secret".into(),
            model_list_url: None,
            port: None,
        },
    )
    .unwrap();

    let prepared = app.prepare_session_config_update(&provider_model_config_state(
        SessionConfigSource::SessionModel,
        "lab-model",
        &[("custom", "lab-model")],
    ));

    let choice = &prepared.controls[0].choices[0];
    assert_eq!(choice.provider.as_deref(), Some("custom"));
    assert_eq!(choice.provider_label.as_deref(), Some("Lab Provider"));
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
                    provider_label: None,
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

#[test]
fn broadcast_emits_ui_updated_on_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let mut rx = app.subscribe_updates();

    app.apply_event_with_dirty_tracking(&ClientEvent::UsageUpdated {
        usage: usage_event("gpt-5.1", 10, "1751328000"),
    });

    let update = rx.try_recv().expect("UiUpdated signal should be broadcast");
    match update {
        AppUpdate::UiUpdated { revision } => {
            assert_eq!(revision, app.ui.revision);
        }
        other => panic!("expected UiUpdated, got {other:?}"),
    }
}

#[test]
fn broadcast_emits_permission_requested_for_tool_permission_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let mut rx = app.subscribe_updates();

    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "call-1".to_string(),
        name: "shell".to_string(),
        options: Vec::new(),
        details: None,
        input: Some(workspace_model::PermissionInputRequest::default()),
    });

    let mut saw_permission = false;
    while let Ok(update) = rx.try_recv() {
        if let AppUpdate::PermissionRequested { tool_call_id, .. } = update {
            assert_eq!(tool_call_id, "call-1");
            saw_permission = true;
        }
    }
    assert!(saw_permission, "PermissionRequested signal should be broadcast");
}

/// In-process loopback: validates the `RemoteControl` gateway end-to-end
/// without a network. Wraps a real `Application` (mock ACP) in
/// `AppCoreRemoteControl`, subscribes to update signals, drives an event by
/// locking the shared handle (simulating the agent producing output), and
/// asserts the subscriber receives `UiUpdated`, then exercises the trait
/// command methods (`create_session`, `get_state`).
#[test]
fn loopback_remote_control_drives_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let app = Arc::new(Mutex::new(app));
    let control = AppCoreRemoteControl::new(app.clone(), || Ok(Vec::new()));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut rx = control.subscribe_updates();

    {
        let mut app = app.lock().unwrap();
        app.apply_event_with_dirty_tracking(&ClientEvent::UsageUpdated {
            usage: usage_event("gpt-5.1", 10, "1751328000"),
        });
    }

    let update = rx
        .try_recv()
        .expect("loopback subscriber should receive UiUpdated after an event");
    assert!(matches!(update, AppUpdate::UiUpdated { .. }));

    let session_id = rt
        .block_on(control.create_session(None, None))
        .expect("create_session via trait should succeed");
    assert!(!session_id.is_empty());

    let snapshot = rt
        .block_on(control.get_state())
        .expect("get_state via trait should succeed");
    assert_eq!(snapshot.session.id.to_string(), session_id);

    let mut rx2 = control.subscribe_updates();
    {
        let mut app = app.lock().unwrap();
        app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
            id: "loopback-call".to_string(),
            name: "shell".to_string(),
            options: Vec::new(),
            details: None,
            input: Some(workspace_model::PermissionInputRequest::default()),
        });
    }
    let mut saw_permission = false;
    while let Ok(update) = rx2.try_recv() {
        if let AppUpdate::PermissionRequested { tool_call_id, .. } = update {
            assert_eq!(tool_call_id, "loopback-call");
            saw_permission = true;
        }
    }
    assert!(saw_permission, "loopback should surface PermissionRequested");
}

/// Remote-mode permission gating: in full-access mode, a destructive
/// permission is auto-resolved locally, but NOT when the prompt originated
/// from a remote (relay/phone) caller — the phone must approve explicitly.
#[test]
fn remote_mode_blocks_full_access_auto_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.ui.session.mode = Some("full-access".to_string());

    // Inject a tool with a destructive permission request (a shell tool with
    // an "allow" option) so auto-resolve would fire in local full-access mode.
    let mut tool = workspace_model::ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "remote-gate-call".to_string(),
        parent_call_id: None,
        name: "shell".to_string(),
        kind: "permission".to_string(),
        summary: "等待权限".to_string(),
        status: workspace_model::ToolStatus::Running,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: None,
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: vec![workspace_model::PermissionOption {
            id: "allow".to_string(),
            label: "Allow".to_string(),
            kind: "allow".to_string(),
        }],
        permission_input: None,
        permission_decision: None,
        can_stop: false,
        stop_kind: None,
        stop_status: None,
    };
    app.ui.tools.push(tool);

    // Remote mode: auto-resolve is suppressed; the phone must approve.
    // `auto_resolve` short-circuits on `remote_mode` before touching the
    // session, so the tool's permission stays unresolved.
    app.set_remote_mode(true);
    let delivered = app.auto_resolve_full_access_permission_if_applicable("remote-gate-call");
    assert!(
        !delivered,
        "remote mode must NOT auto-resolve; the phone must approve"
    );
    assert!(app.is_remote_mode());
    let tool = app
        .ui
        .tools
        .iter()
        .find(|t| t.call_id == "remote-gate-call")
        .unwrap();
    assert!(
        tool.permission_decision.is_none(),
        "remote mode must leave the permission unresolved for the phone"
    );
}
