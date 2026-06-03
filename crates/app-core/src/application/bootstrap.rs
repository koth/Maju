use super::*;

impl Application {
    pub fn bootstrap(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::bootstrap_with_app_paths(workspace_root, agent_command, AppPaths::resolve()?)
    }

    pub fn bootstrap_with_app_paths(
        workspace_root: impl AsRef<Path>,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let workspace_root = normalize_workspace_root(workspace_root.as_ref());
        let workspace_root = workspace_root.as_path();
        let agent_command = agent_command.into();
        crate::startup_perf::mark(
            "app/bootstrap/start",
            format!(
                "workspace={} agent_command_len={}",
                workspace_root.display(),
                agent_command.len()
            ),
        );
        crate::startup_perf::measure("app/bootstrap/ensure_dirs", "", || {
            app_paths.ensure_standard_dirs()
        })?;
        let _ = crate::attachment_cache::prune_expired_attachments(&app_paths.attachments_dir());
        let mut ui = crate::startup_perf::measure(
            "app/bootstrap/build_initial_ui",
            workspace_root.display().to_string(),
            || build_initial_ui(workspace_root),
        )?;

        let store = crate::startup_perf::measure(
            "app/bootstrap/session_store_open",
            workspace_root.display().to_string(),
            || SessionStore::open(app_paths.root(), workspace_root),
        )?;

        // Read ACP port from settings.
        let settings = crate::startup_perf::measure("app/bootstrap/load_settings", "", || {
            crate::settings::load_app_settings(&app_paths)
        });
        let acp_port = settings.acp_port;

        let existing_sessions =
            crate::startup_perf::measure("app/bootstrap/list_sessions", "", || {
                store.list_sessions().unwrap_or_default()
            });
        crate::startup_perf::mark(
            "app/bootstrap/list_sessions_count",
            existing_sessions.len().to_string(),
        );
        let most_recent_session = existing_sessions.first();
        let requested_agent_label =
            crate::startup_perf::measure("app/bootstrap/agent_label_for_command", "", || {
                crate::settings::agent_label_for_command(&agent_command)
            });
        let persisted_agent_command = most_recent_session
            .and_then(|session| session.agent_cli.as_deref())
            .filter(|label| *label != requested_agent_label)
            .and_then(|label| {
                crate::settings::command_for_agent_label_with_paths(label, &app_paths)
            });
        let agent_command = persisted_agent_command.unwrap_or(agent_command);
        crate::settings::ensure_agent_ready_for_command(&agent_command, &app_paths)?;

        // Check for an existing local session and its agent-side ACP session ID
        // for session/load. Empty local sessions can have a transient ACP id
        // from session/new before the agent has a durable resource.
        let resume_session_id = most_recent_session.and_then(|session| {
            if store.session_has_activity(&session.id).unwrap_or(false) {
                session.acp_session_id.clone()
            } else {
                let _ = store.clear_acp_session_id(&session.id);
                None
            }
        });

        // If resuming an existing session, skip replay events from session/load
        let skip_replay = resume_session_id.is_some();

        let session = crate::startup_perf::measure(
            "app/bootstrap/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: ui.workspace.root.display().to_string(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: ui.session.model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: crate::settings::agent_env_for_command(&agent_command, &app_paths),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port,
                    remote_ssh: None,
                })
            },
        )?;

        // Try to restore the most recent session, otherwise create a new one
        let (needs_title, seq_counter, pending_model_restore) = match existing_sessions.as_slice() {
            [recent, ..] => {
                // list_sessions orders by updated_at DESC
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) =
                    crate::startup_perf::measure("app/bootstrap/load_session", session_id, || {
                        store.load_session(session_id)
                    })
                {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    let mut tools = tools;
                    let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
                    for tool_id in &interrupted_tool_ids {
                        if let Some(tool) =
                            tools.iter().find(|tool| tool.id.to_string() == *tool_id)
                        {
                            let _ = store.update_tool(
                                tool_id,
                                "Interrupted",
                                tool.raw_output.as_deref(),
                                tool.error.as_deref(),
                            );
                        }
                    }
                    let mut pending_model_restore = None;
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        pending_model_restore = Some(model.clone());
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    // Historical diffs are now loaded through scoped change-set APIs.
                    // Keep legacy arrays empty on session restore so they cannot act as
                    // the primary source for review or timeline diff hydration.
                    ui.session_changes.clear();
                    ui.review_changes.clear();
                    ui.turn_changes.clear();
                    let seq =
                        crate::startup_perf::measure("app/bootstrap/next_seq", session_id, || {
                            store.next_seq(session_id).unwrap_or(1)
                        });
                    let needs_title = is_placeholder_session_title(&recent.title);
                    (needs_title, seq, pending_model_restore)
                } else {
                    // Failed to load — create new session
                    let session_id = ui.session.id.to_string();
                    crate::startup_perf::measure(
                        "app/bootstrap/create_session_after_load_failed",
                        &session_id,
                        || store.create_session(&session_id, &ui.session.model),
                    )?;
                    (true, 1, None)
                }
            }
            _ => {
                // No sessions exist — create a new one
                let session_id = ui.session.id.to_string();
                crate::startup_perf::measure(
                    "app/bootstrap/create_session_empty",
                    &session_id,
                    || store.create_session(&session_id, &ui.session.model),
                )?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        // Determine which agent CLI this session is using. Preserve the per-session
        // persisted value when reopening, instead of overwriting it with the global
        // settings default.
        let agent_cli_label =
            crate::startup_perf::measure("app/bootstrap/resolve_session_agent_label", "", || {
                store
                    .get_session_agent_cli(&ui.session.id.to_string())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| crate::settings::agent_label_for_command(&agent_command))
            });
        ui.session.agent_cli = Some(agent_cli_label.clone());
        update_initial_agent_notice(&mut ui, &agent_cli_label);
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_agent_cli", "", || {
            store.update_session_agent_cli(&ui.session.id.to_string(), &agent_cli_label)
        });
        if is_codex_agent_label(&agent_cli_label) {
            let session_id = ui.session.id.to_string();
            if store
                .get_session_codex_provider(&session_id)
                .ok()
                .flatten()
                .is_none()
            {
                let provider = crate::settings::codex_current_provider(&app_paths);
                let _ = crate::startup_perf::measure(
                    "app/bootstrap/update_session_codex_provider",
                    "",
                    || store.update_session_codex_provider(&session_id, &provider),
                );
            }
        }
        let _ = crate::startup_perf::measure("app/bootstrap/set_permission_mode", "", || {
            session.set_permission_mode(ui.session.mode.as_deref().unwrap_or("Build"))
        });
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_model_mode", "", || {
            store.update_session_model_mode(
                &ui.session.id.to_string(),
                &ui.session.model,
                ui.session.mode.as_deref(),
            )
        });

        let file_tracker = crate::startup_perf::measure(
            "app/bootstrap/file_tracker_new",
            workspace_root.display().to_string(),
            || FileChangeTracker::new(workspace_root),
        );
        crate::startup_perf::mark("app/bootstrap/end", "");

        Ok(Self {
            ui,
            session,
            runtime_registry: SessionRuntimeRegistry::default(),
            runtime_clock: RuntimeClock::default(),
            store,
            app_paths,
            agent_command,
            acp_port,
            remote_ssh: None,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay,
            pending_model_restore,
            authoritative_model_selection: None,
            file_tracker,
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            inline_think_filter: InlineThinkFilter::default(),
        })
    }

    pub fn bootstrap_remote_linux_with_app_paths(
        remote: RemoteLinuxWorkspace,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let prepared = prepare_remote_linux_workspace(remote, agent_command.into())?;
        let PreparedRemoteLinuxWorkspace {
            remote,
            agent_command,
            remote_key,
            local_port,
            remote_ssh,
        } = prepared;

        crate::startup_perf::mark(
            "app/bootstrap_remote/start",
            format!(
                "workspace={} ssh_target={} agent_command_len={}",
                remote.remote_path,
                remote.ssh_target,
                agent_command.len()
            ),
        );
        crate::startup_perf::measure("app/bootstrap_remote/ensure_dirs", "", || {
            app_paths.ensure_standard_dirs()
        })?;
        let _ = crate::attachment_cache::prune_expired_attachments(&app_paths.attachments_dir());

        let mut ui = crate::startup_perf::measure(
            "app/bootstrap_remote/build_initial_ui",
            &remote_key,
            || build_initial_remote_ui(remote.clone()),
        )?;
        let workspace_root = ui.workspace.root.clone();
        let workspace_root = workspace_root.as_path();
        let store = crate::startup_perf::measure(
            "app/bootstrap_remote/session_store_open",
            &remote_key,
            || SessionStore::open(app_paths.root(), workspace_root),
        )?;

        let existing_sessions =
            crate::startup_perf::measure("app/bootstrap_remote/list_sessions", "", || {
                store.list_sessions().unwrap_or_default()
            });
        let most_recent_session = existing_sessions.first();
        let resume_session_id = most_recent_session.and_then(|session| {
            if store.session_has_activity(&session.id).unwrap_or(false) {
                session.acp_session_id.clone()
            } else {
                let _ = store.clear_acp_session_id(&session.id);
                None
            }
        });
        let skip_replay = resume_session_id.is_some();
        let session = crate::startup_perf::measure(
            "app/bootstrap_remote/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: remote.remote_path.clone(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: ui.session.model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: Vec::new(),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port: local_port,
                    remote_ssh: Some(remote_ssh.clone()),
                })
            },
        )?;

        let (needs_title, seq_counter, pending_model_restore) = match existing_sessions.as_slice() {
            [recent, ..] => {
                let session_id = &recent.id;
                if let Ok((messages, tools, timeline)) = store.load_session(session_id) {
                    ui.session.id = uuid::Uuid::parse_str(session_id).unwrap_or(ui.session.id);
                    ui.session.title = recent.title.clone();
                    let mut tools = tools;
                    let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
                    for tool_id in &interrupted_tool_ids {
                        if let Some(tool) =
                            tools.iter().find(|tool| tool.id.to_string() == *tool_id)
                        {
                            let _ = store.update_tool(
                                tool_id,
                                "Interrupted",
                                tool.raw_output.as_deref(),
                                tool.error.as_deref(),
                            );
                        }
                    }
                    let mut pending_model_restore = None;
                    if let Ok(Some((model, mode))) = store.get_session_model_mode(session_id) {
                        pending_model_restore = Some(model.clone());
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    ui.session_changes.clear();
                    ui.review_changes.clear();
                    ui.turn_changes.clear();
                    let seq = store.next_seq(session_id).unwrap_or(1);
                    let needs_title = is_placeholder_session_title(&recent.title);
                    (needs_title, seq, pending_model_restore)
                } else {
                    let session_id = ui.session.id.to_string();
                    store.create_session(&session_id, &ui.session.model)?;
                    (true, 1, None)
                }
            }
            _ => {
                let session_id = ui.session.id.to_string();
                store.create_session(&session_id, &ui.session.model)?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        let agent_cli_label = store
            .get_session_agent_cli(&ui.session.id.to_string())
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::settings::agent_label_for_command(&agent_command));
        ui.session.agent_cli = Some(agent_cli_label.clone());
        update_initial_agent_notice(&mut ui, &agent_cli_label);
        let _ = store.update_session_agent_cli(&ui.session.id.to_string(), &agent_cli_label);
        if is_codex_agent_label(&agent_cli_label) {
            let session_id = ui.session.id.to_string();
            if store
                .get_session_codex_provider(&session_id)
                .ok()
                .flatten()
                .is_none()
            {
                let provider = crate::settings::codex_current_provider(&app_paths);
                let _ = store.update_session_codex_provider(&session_id, &provider);
            }
        }
        let _ = session.set_permission_mode(ui.session.mode.as_deref().unwrap_or("Build"));
        let _ = store.update_session_model_mode(
            &ui.session.id.to_string(),
            &ui.session.model,
            ui.session.mode.as_deref(),
        );

        let file_tracker = FileChangeTracker::new(workspace_root);
        crate::startup_perf::mark("app/bootstrap_remote/end", "");

        Ok(Self {
            ui,
            session,
            runtime_registry: SessionRuntimeRegistry::default(),
            runtime_clock: RuntimeClock::default(),
            store,
            app_paths,
            agent_command,
            acp_port: local_port,
            remote_ssh: Some(remote_ssh),
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay,
            pending_model_restore,
            authoritative_model_selection: None,
            file_tracker,
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            inline_think_filter: InlineThinkFilter::default(),
        })
    }
}

struct PreparedRemoteLinuxWorkspace {
    remote: RemoteLinuxWorkspace,
    agent_command: String,
    remote_key: String,
    local_port: u16,
    remote_ssh: RemoteSshSessionConfig,
}

fn prepare_remote_linux_workspace(
    mut remote: RemoteLinuxWorkspace,
    fallback_agent_command: String,
) -> anyhow::Result<PreparedRemoteLinuxWorkspace> {
    remote.ssh_target = remote.ssh_target.trim().to_string();
    remote.remote_path = remote.remote_path.trim().to_string();
    if remote.ssh_target.is_empty() {
        anyhow::bail!("SSH target cannot be empty");
    }
    if remote.ssh_port == Some(0) {
        anyhow::bail!("SSH port must be between 1 and 65535");
    }
    if !remote.remote_path.starts_with('/') {
        anyhow::bail!("Remote workspace path must be absolute");
    }

    let agent_command = remote
        .agent_command
        .as_ref()
        .filter(|command| !command.trim().is_empty())
        .cloned()
        .unwrap_or(fallback_agent_command)
        .trim()
        .to_string();
    if agent_command.is_empty() {
        anyhow::bail!("Remote agent command cannot be empty");
    }

    let local_port = remote.local_port.unwrap_or(find_available_loopback_port()?);
    let remote_port = remote.remote_port.unwrap_or(local_port);
    remote.local_port = Some(local_port);
    remote.remote_port = Some(remote_port);
    let remote_key = remote.key();
    let remote_ssh = RemoteSshSessionConfig {
        ssh_target: remote.ssh_target.clone(),
        ssh_port: remote.ssh_port,
        remote_workspace_root: remote.remote_path.clone(),
        local_port,
        remote_port,
        ssh_command: None,
        ssh_password: remote
            .ssh_password
            .clone()
            .filter(|password| !password.is_empty()),
    };

    Ok(PreparedRemoteLinuxWorkspace {
        remote,
        agent_command,
        remote_key,
        local_port,
        remote_ssh,
    })
}

fn normalize_workspace_root(workspace_root: &Path) -> std::path::PathBuf {
    if workspace_root.is_absolute() {
        return workspace_root.to_path_buf();
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(workspace_root))
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}

fn find_available_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use workspace_model::{AgentCliId, RemoteLinuxWorkspace};

    fn remote_fixture() -> RemoteLinuxWorkspace {
        RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target: " alice@devbox ".into(),
            ssh_port: Some(2222),
            remote_path: " /srv/project ".into(),
            ssh_password: None,
            agent_cli: Some(AgentCliId::CodexAcp),
            agent_command: None,
            local_port: Some(3456),
            remote_port: None,
        }
    }

    fn prepare_error(remote: RemoteLinuxWorkspace, fallback_agent_command: &str) -> String {
        match prepare_remote_linux_workspace(remote, fallback_agent_command.into()) {
            Ok(_) => panic!("expected remote workspace preparation to fail"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn remote_workspace_preparation_normalizes_identity_and_builds_session_config() {
        let prepared =
            prepare_remote_linux_workspace(remote_fixture(), "codex-acp".into()).unwrap();

        assert_eq!(prepared.remote_key, "ssh://alice@devbox:2222/srv/project");
        assert_eq!(prepared.remote.ssh_target, "alice@devbox");
        assert_eq!(prepared.remote.ssh_port, Some(2222));
        assert_eq!(prepared.remote.remote_path, "/srv/project");
        assert_eq!(prepared.local_port, 3456);
        assert_eq!(prepared.remote.remote_port, Some(3456));
        assert_eq!(prepared.agent_command, "codex-acp");
        assert_eq!(prepared.remote_ssh.ssh_target, "alice@devbox");
        assert_eq!(prepared.remote_ssh.ssh_port, Some(2222));
        assert_eq!(prepared.remote_ssh.remote_workspace_root, "/srv/project");
        assert_eq!(prepared.remote_ssh.local_port, 3456);
        assert_eq!(prepared.remote_ssh.remote_port, 3456);
    }

    #[test]
    fn remote_workspace_bootstrap_builds_remote_snapshot_and_session_config() {
        let dir = tempfile::tempdir().unwrap();
        let app = Application::bootstrap_remote_linux_with_app_paths(
            remote_fixture(),
            "codex-acp",
            AppPaths::from_root(dir.path().join("home").join(".kodex")),
        )
        .unwrap();

        assert!(app.is_remote_workspace());
        assert_eq!(app.ui.workspace.name, "project");
        assert_eq!(
            app.ui.workspace.root,
            std::path::PathBuf::from("ssh://alice@devbox:2222/srv/project")
        );
        let remote_ssh = app.remote_ssh_session_config().unwrap();
        assert_eq!(remote_ssh.ssh_target, "alice@devbox");
        assert_eq!(remote_ssh.ssh_port, Some(2222));
        assert_eq!(remote_ssh.remote_workspace_root, "/srv/project");
        assert_eq!(remote_ssh.local_port, 3456);
        assert_eq!(remote_ssh.remote_port, 3456);
    }

    #[test]
    fn remote_workspace_rejects_shell_resolution_for_local_only_commands() {
        let dir = tempfile::tempdir().unwrap();
        let app = Application::bootstrap_remote_linux_with_app_paths(
            remote_fixture(),
            "codex-acp",
            AppPaths::from_root(dir.path().join("home").join(".kodex")),
        )
        .unwrap();

        assert_eq!(
            app.resolve_workspace_entry_for_shell("Cargo.toml")
                .unwrap_err(),
            "Remote workspaces do not support local filesystem commands yet"
        );
    }

    #[test]
    fn remote_workspace_preparation_rejects_missing_target_before_session_start() {
        let mut remote = remote_fixture();
        remote.ssh_target = "  ".into();

        assert_eq!(
            prepare_error(remote, "codex-acp"),
            "SSH target cannot be empty"
        );
    }

    #[test]
    fn remote_workspace_preparation_rejects_relative_remote_path_before_session_start() {
        let mut remote = remote_fixture();
        remote.remote_path = "project".into();

        assert_eq!(
            prepare_error(remote, "codex-acp"),
            "Remote workspace path must be absolute"
        );
    }

    #[test]
    fn remote_workspace_preparation_rejects_zero_ssh_port() {
        let mut remote = remote_fixture();
        remote.ssh_port = Some(0);

        assert_eq!(
            prepare_error(remote, "codex-acp"),
            "SSH port must be between 1 and 65535"
        );
    }

    #[test]
    fn remote_workspace_preparation_rejects_empty_agent_command() {
        let mut remote = remote_fixture();
        remote.agent_command = Some(" ".into());

        assert_eq!(
            prepare_error(remote, " "),
            "Remote agent command cannot be empty"
        );
    }

    #[test]
    #[ignore = "requires a reachable real SSH host and KODEX_REAL_REMOTE_* env vars"]
    fn real_remote_ssh_tcp_smoke_opens_and_streams_prompt() {
        let ssh_target = std::env::var("KODEX_REAL_REMOTE_SSH_TARGET")
            .expect("KODEX_REAL_REMOTE_SSH_TARGET is required");
        let ssh_port = std::env::var("KODEX_REAL_REMOTE_SSH_PORT")
            .ok()
            .map(|value| value.parse::<u16>().expect("valid SSH port"));
        let remote_path =
            std::env::var("KODEX_REAL_REMOTE_PATH").expect("KODEX_REAL_REMOTE_PATH is required");
        let agent_command = std::env::var("KODEX_REAL_REMOTE_AGENT_COMMAND")
            .expect("KODEX_REAL_REMOTE_AGENT_COMMAND is required");

        let dir = tempfile::tempdir().unwrap();
        let remote = RemoteLinuxWorkspace {
            profile_id: None,
            ssh_target,
            ssh_port,
            remote_path,
            ssh_password: std::env::var("KODEX_REAL_REMOTE_SSH_PASSWORD").ok(),
            agent_cli: None,
            agent_command: Some(agent_command),
            local_port: None,
            remote_port: None,
        };
        let app_paths = AppPaths::from_root(dir.path().join("home").join(".kodex"));
        let mut app = Application::bootstrap_remote_linux_with_app_paths(
            remote,
            "unused-agent-command",
            app_paths.clone(),
        )
        .unwrap();

        wait_for_remote_session_start(&mut app, &app_paths);
        let message_count_before_prompt = app.ui.messages.len();
        let tool_count_before_prompt = app.ui.tools.len();
        app.send_prompt_background("remote ssh tcp smoke").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.poll_prompt_progress();
            if !app.has_in_flight_prompt()
                && app
                    .ui
                    .messages
                    .iter()
                    .skip(message_count_before_prompt)
                    .any(|message| message.role == MessageRole::Assistant)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        assert!(
            !app.has_in_flight_prompt(),
            "prompt did not finish: status={:?} session_alive={} last_error={:?} messages={:?} tools={:?} logs={}",
            app.ui.session.status,
            app.session.is_alive(),
            app.session.last_error(),
            app.ui.messages,
            app.ui.tools,
            smoke_runtime_logs(&app_paths)
        );
        assert!(
            app.ui.messages.iter().any(|message| {
                message.role == MessageRole::Assistant
                    && message
                        .body
                        .contains("Remote ACP session connected over SSH-forwarded TCP")
            }),
            "assistant response was not streamed: messages={:?} logs={}",
            app.ui.messages,
            smoke_runtime_logs(&app_paths)
        );
        assert!(
            app.ui
                .tools
                .iter()
                .skip(tool_count_before_prompt)
                .any(|tool| tool.call_id == "remote-tool-1"
                    && matches!(tool.status, ToolStatus::Succeeded)),
            "remote tool activity was not streamed: tools={:?} logs={}",
            app.ui.tools,
            smoke_runtime_logs(&app_paths)
        );
    }

    fn wait_for_remote_session_start(app: &mut Application, app_paths: &AppPaths) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            app.poll_prompt_progress();
            if !app.session.id.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        panic!(
            "remote ACP session did not start: status={:?} session_alive={} last_error={:?} messages={:?} tools={:?} logs={}",
            app.ui.session.status,
            app.session.is_alive(),
            app.session.last_error(),
            app.ui.messages,
            app.ui.tools,
            smoke_runtime_logs(app_paths)
        );
    }

    fn smoke_runtime_logs(app_paths: &AppPaths) -> String {
        let logs_dir = app_paths.logs_dir();
        let Ok(entries) = std::fs::read_dir(&logs_dir) else {
            return format!("<no logs at {}>", logs_dir.display());
        };
        let mut logs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("acp-notifications-") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => logs.push(format!("{}:\n{}", name, content)),
                Err(error) => logs.push(format!("{}: <read failed: {}>", name, error)),
            }
        }
        if logs.is_empty() {
            format!("<no acp notification logs at {}>", logs_dir.display())
        } else {
            logs.join("\n---\n")
        }
    }
}
