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

        // Check for existing session and its ACP session ID for --resume. Empty local sessions can
        // have a transient ACP id from session/new before the agent has a durable resource.
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
            store,
            app_paths,
            agent_command,
            acp_port,
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

fn normalize_workspace_root(workspace_root: &Path) -> std::path::PathBuf {
    if workspace_root.is_absolute() {
        return workspace_root.to_path_buf();
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(workspace_root))
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}
