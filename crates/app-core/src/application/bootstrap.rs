use super::*;
use crate::remote_ssh::{RemoteSshCommand, RemoteSshCommandRunner, SystemRemoteSshCommandRunner};
use acp_core::RemoteSshReverseForward;
use std::collections::{BTreeMap, BTreeSet};

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
        // Register the project root with the in-process codex_api_proxy so it
        // forwards `X-Session-Dir` to codebuddy-proxy; the CLI then spawns in
        // this dir and stores its rollout under the matching path slug, which
        // is what lets a later pool miss `--resume` the conversation by id.
        acp_core::set_codex_api_proxy_workspace_root(&workspace_root.to_string_lossy());
        let workspace_root = workspace_root.as_path();
        let requested_agent_command = agent_command.into();
        crate::startup_perf::mark(
            "app/bootstrap/start",
            format!(
                "workspace={} agent_command_len={}",
                workspace_root.display(),
                requested_agent_command.len()
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
        let agent_command = agent_command_for_restored_session(
            most_recent_session,
            requested_agent_command,
            &app_paths,
            false,
        );
        crate::startup_perf::mark(
            "app/bootstrap/restored_agent_command",
            format!(
                "is_codex={} is_claude={} command_len={}",
                crate::settings::is_codex_acp_command(&agent_command),
                crate::settings::is_claude_agent_acp_command(&agent_command),
                agent_command.len()
            ),
        );
        crate::settings::ensure_agent_ready_for_command(&agent_command, &app_paths)?;
        let (mcp_servers, web_tools_mcp) =
            super::sessions::prepare_web_tools_mcp(&app_paths, &agent_command, false)
                .map_err(anyhow::Error::msg)?;

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
        let startup_model = most_recent_session
            .and_then(|session| {
                store
                    .get_session_model_provider_mode(&session.id)
                    .ok()
                    .flatten()
            })
            .map(|(model, provider, _)| {
                super::config::provider_qualified_model_value(&model, provider.as_deref())
            })
            .unwrap_or_else(|| ui.session.model.clone());

        let mut session = crate::startup_perf::measure(
            "app/bootstrap/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: ui.workspace.root.display().to_string(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: startup_model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: crate::settings::agent_env_for_command(&agent_command, &app_paths),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port,
                    remote_ssh: None,
                    mcp_servers: mcp_servers.clone(),
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
                    if let Ok(Some((model, provider, mode))) =
                        store.get_session_model_provider_mode(session_id)
                    {
                        pending_model_restore = Some(ModelSelection::new(model.clone(), provider));
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
                    ui.usage = store
                        .load_session_usage_snapshot(session_id)
                        .unwrap_or_default();
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
                active_agent_label_for_command(
                    &agent_command,
                    store
                        .get_session_agent_cli(&ui.session.id.to_string())
                        .ok()
                        .flatten(),
                )
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
        let _ = crate::startup_perf::measure("app/bootstrap/queue_codex_mode", "", || {
            super::config::queue_codex_agent_mode_for_policy_mode(
                &mut session,
                is_codex_agent_label(&agent_cli_label),
                ui.session.mode.as_deref(),
            )
        });
        let _ = crate::startup_perf::measure("app/bootstrap/update_session_model_mode", "", || {
            store.update_session_model_mode_provider(
                &ui.session.id.to_string(),
                &ui.session.model,
                pending_model_restore
                    .as_ref()
                    .and_then(|selection| selection.provider.as_deref()),
                ui.session.mode.as_deref(),
            )
        });

        let file_tracker = crate::startup_perf::measure(
            "app/bootstrap/file_tracker_new",
            workspace_root.display().to_string(),
            || FileChangeTracker::new(workspace_root),
        );

        // Re-resolve image capabilities and attach the `kodex-image` MCP server
        // for a restored session. The bootstrap path above only prepares
        // `web_tools_mcp`; without this, a restored text-only-model session
        // keeps the default `assumed_native()` capabilities (`native_view=true`,
        // `view_fallback=false`), so image prompts skip degradation and are sent
        // raw to a text-only model, which then emits "image content omitted".
        // The session is already started, so the returned MCP server entry is
        // discarded — the handle is enough for `degrade_prompt_for_image_fallback`
        // to call `view_image` via the internal codex-api-proxy HTTP path.
        let (image_mcp, image_capabilities) =
            match super::sessions::prepare_image_mcp(
                &app_paths,
                &agent_command,
                &startup_model,
                &ui.workspace.root.display().to_string(),
                false,
            ) {
                Ok((_image_servers, handle, caps)) => (handle, caps),
                Err(error) => {
                    crate::startup_perf::mark(
                        "app/bootstrap/image_mcp_failed",
                        error,
                    );
                    (None, workspace_model::ImageCapabilities::default())
                }
            };
        ui.image_capabilities = image_capabilities;
        // `prompt_capabilities.image` gates image attachments and mirrors
        // `image_capabilities.image_capable()` (native_view || view_fallback),
        // matching `reapply_image_capabilities` / the reducer's
        // `PromptCapabilitiesUpdated` handler.
        ui.prompt_capabilities.image = image_capabilities.image_capable();
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
            web_tools_mcp,
            image_mcp,
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

            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            pending_image_degradation: None,
        })
    }

    pub fn bootstrap_remote_linux_with_app_paths(
        remote: RemoteLinuxWorkspace,
        agent_command: impl Into<String>,
        app_paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let prepared = prepare_remote_linux_workspace(remote, agent_command.into())?;
        let PreparedRemoteLinuxWorkspace {
            mut remote,
            agent_command,
            remote_key,
            local_port,
            mut remote_ssh,
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
        let selected_session = existing_sessions.first();
        let agent_command =
            agent_command_for_restored_session(selected_session, agent_command, &app_paths, true);
        if let Some(agent) = agent_id_for_restored_session(selected_session) {
            remote.agent_cli = Some(agent);
            remote.agent_command = Some(agent_command.clone());
            if let workspace_model::WorkspaceLocation::RemoteLinux(ui_remote) =
                &mut ui.workspace.location
            {
                ui_remote.agent_cli = Some(agent);
                ui_remote.agent_command = Some(agent_command.clone());
            }
        }
        let resume_session_id = selected_session.and_then(|session| {
            if store.session_has_activity(&session.id).unwrap_or(false) {
                session.acp_session_id.clone()
            } else {
                let _ = store.clear_acp_session_id(&session.id);
                None
            }
        });
        let skip_replay = resume_session_id.is_some();
        let startup_model = selected_session
            .and_then(|session| {
                store
                    .get_session_model_provider_mode(&session.id)
                    .ok()
                    .flatten()
            })
            .map(|(model, provider, _)| {
                super::config::provider_qualified_model_value(&model, provider.as_deref())
            })
            .unwrap_or_else(|| ui.session.model.clone());
        let remote_runtime = prepare_remote_agent_runtime(&agent_command, &app_paths, &remote_ssh)?;
        remote_ssh.reverse_forwards = remote_runtime.reverse_forwards.clone();

        let mut session = crate::startup_perf::measure(
            "app/bootstrap_remote/session_handle_start",
            format!("resume={}", resume_session_id.is_some()),
            || {
                SessionHandle::start(SessionConfig {
                    workspace_root: remote.remote_path.clone(),
                    app_data_root: app_paths.root().display().to_string(),
                    model: startup_model.clone(),
                    agent_command: agent_command.clone(),
                    agent_env: remote_runtime.agent_env.clone(),
                    resume_session_id,
                    log_id: make_log_id(),
                    acp_port: local_port,
                    remote_ssh: Some(remote_ssh.clone()),
                    mcp_servers: Vec::new(),
                })
            },
        )?;

        let (needs_title, seq_counter, pending_model_restore) = match selected_session {
            Some(recent) => {
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
                    if let Ok(Some((model, provider, mode))) =
                        store.get_session_model_provider_mode(session_id)
                    {
                        pending_model_restore = Some(ModelSelection::new(model.clone(), provider));
                        ui.session.model = model;
                        ui.session.mode = mode;
                    }
                    ui.messages = messages;
                    ui.tools = tools;
                    ui.timeline = timeline;
                    ui.session_changes.clear();
                    ui.review_changes.clear();
                    ui.turn_changes.clear();
                    ui.usage = store
                        .load_session_usage_snapshot(session_id)
                        .unwrap_or_default();
                    let seq = store.next_seq(session_id).unwrap_or(1);
                    let needs_title = is_placeholder_session_title(&recent.title);
                    (needs_title, seq, pending_model_restore)
                } else {
                    let session_id = ui.session.id.to_string();
                    store.create_session(&session_id, &ui.session.model)?;
                    (true, 1, None)
                }
            }
            None => {
                let session_id = ui.session.id.to_string();
                store.create_session(&session_id, &ui.session.model)?;
                (true, 1, None)
            }
        };

        if ui.session.mode.is_none() {
            ui.session.mode = Some("Build".into());
        }
        let agent_cli_label = active_agent_label_for_command(
            &agent_command,
            store
                .get_session_agent_cli(&ui.session.id.to_string())
                .ok()
                .flatten(),
        );
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
        let _ = super::config::queue_codex_agent_mode_for_policy_mode(
            &mut session,
            is_codex_agent_label(&agent_cli_label),
            ui.session.mode.as_deref(),
        );
        let _ = store.update_session_model_mode_provider(
            &ui.session.id.to_string(),
            &ui.session.model,
            pending_model_restore
                .as_ref()
                .and_then(|selection| selection.provider.as_deref()),
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
            web_tools_mcp: None,
            image_mcp: None,
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
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            pending_image_degradation: None,
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
        reverse_forwards: Vec::new(),
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

pub(super) fn find_available_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

#[derive(Debug, Clone, Default)]
pub(super) struct RemoteAgentRuntime {
    pub(super) agent_env: Vec<(String, String)>,
    pub(super) reverse_forwards: Vec<RemoteSshReverseForward>,
}

pub(super) fn prepare_remote_agent_runtime(
    agent_command: &str,
    app_paths: &AppPaths,
    remote_ssh: &RemoteSshSessionConfig,
) -> anyhow::Result<RemoteAgentRuntime> {
    crate::settings::ensure_agent_ready_for_command(agent_command, app_paths)?;

    let is_codex = crate::settings::is_codex_acp_command(agent_command);
    let mut agent_env =
        crate::settings::remote_agent_env_for_command(agent_command, app_paths, None);
    let mut codex_config = if is_codex {
        crate::settings::remote_codex_proxy_config(app_paths, None)?
    } else {
        None
    };
    let mut codex_model_catalog = None;
    let mut remote_codex_home = None;

    if codex_config.is_some() {
        let remote_home = resolve_remote_home(remote_ssh)?;
        let resolved_remote_codex_home = crate::settings::remote_codex_home(&remote_home)
            .ok_or_else(|| anyhow::anyhow!("远程 HOME 解析为空"))?;
        agent_env = crate::settings::remote_agent_env_for_command(
            agent_command,
            app_paths,
            Some(&remote_home),
        );
        codex_config = crate::settings::remote_codex_proxy_config(
            app_paths,
            Some(&resolved_remote_codex_home),
        )?;
        codex_model_catalog = crate::settings::remote_codex_model_catalog_content(app_paths)?;
        remote_codex_home = Some(resolved_remote_codex_home);
    }

    let mut proxy_ports = collect_remote_proxy_ports(&agent_env, codex_config.as_deref());
    ensure_local_proxy_ports_reachable(&proxy_ports)?;
    let proxy_port_map = if proxy_ports.is_empty() {
        BTreeMap::new()
    } else {
        remote_proxy_port_map(remote_ssh, &mut proxy_ports)?
    };
    if !proxy_port_map.is_empty() {
        rewrite_loopback_proxy_ports_in_env(&mut agent_env, &proxy_port_map);
        if let Some(config) = codex_config.as_mut() {
            rewrite_loopback_proxy_ports(config, &proxy_port_map);
        }
    }

    if codex_config.is_some() {
        let remote_codex_home =
            remote_codex_home.ok_or_else(|| anyhow::anyhow!("远程 HOME 解析为空"))?;
        let config = codex_config
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("远程 Codex 配置为空"))?;
        write_remote_codex_config(remote_ssh, &remote_codex_home, config)?;
        if let Some(catalog) = codex_model_catalog.as_deref() {
            write_remote_codex_model_catalog(remote_ssh, &remote_codex_home, catalog)?;
        }
    }

    let reverse_forwards = proxy_port_map
        .into_iter()
        .map(|(local_port, remote_port)| RemoteSshReverseForward {
            remote_port,
            local_port,
        })
        .collect();
    Ok(RemoteAgentRuntime {
        agent_env,
        reverse_forwards,
    })
}

fn resolve_remote_home(remote_ssh: &RemoteSshSessionConfig) -> anyhow::Result<String> {
    let runner = SystemRemoteSshCommandRunner;
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        remote_ssh.ssh_target.clone(),
        remote_ssh.ssh_port,
        "printf '%s\\n' \"$HOME\"",
        remote_ssh.ssh_password.as_deref(),
        Duration::from_secs(8),
    ));
    if output.success {
        let home = output.stdout.trim();
        if !home.is_empty() {
            return Ok(home.to_string());
        }
    }
    anyhow::bail!(
        "{}",
        first_remote_ssh_message("远程 HOME 解析失败", &output)
    )
}

fn write_remote_codex_config(
    remote_ssh: &RemoteSshSessionConfig,
    remote_codex_home: &str,
    config: &str,
) -> anyhow::Result<()> {
    let config_path = format!("{}/config.toml", remote_codex_home.trim_end_matches('/'));
    write_remote_text_file(
        remote_ssh,
        remote_codex_home,
        &config_path,
        config,
        "远程 Codex 配置写入失败",
    )
}

fn write_remote_codex_model_catalog(
    remote_ssh: &RemoteSshSessionConfig,
    remote_codex_home: &str,
    catalog: &str,
) -> anyhow::Result<()> {
    let catalog_path = format!(
        "{}/model_catalog.json",
        remote_codex_home.trim_end_matches('/')
    );
    write_remote_text_file(
        remote_ssh,
        remote_codex_home,
        &catalog_path,
        catalog,
        "远程 Codex 模型目录写入失败",
    )
}

fn write_remote_text_file(
    remote_ssh: &RemoteSshSessionConfig,
    remote_dir: &str,
    remote_path: &str,
    content: &str,
    error_prefix: &str,
) -> anyhow::Result<()> {
    let runner = SystemRemoteSshCommandRunner;
    let remote_dir = shell_words::quote(remote_dir);
    let remote_path = shell_words::quote(remote_path);
    let command = format!("mkdir -p {remote_dir} && cat > {remote_path} && test -f {remote_path}");
    let output = runner.run_ssh_command(
        &RemoteSshCommand::new(
            remote_ssh.ssh_target.clone(),
            remote_ssh.ssh_port,
            command,
            remote_ssh.ssh_password.as_deref(),
            Duration::from_secs(12),
        )
        .with_stdin(content.as_bytes().to_vec()),
    );
    if output.success {
        return Ok(());
    }
    anyhow::bail!("{}", first_remote_ssh_message(error_prefix, &output))
}

#[cfg(test)]
fn remote_proxy_reverse_forwards(
    agent_env: &[(String, String)],
    codex_config: Option<&str>,
) -> Vec<RemoteSshReverseForward> {
    collect_remote_proxy_ports(agent_env, codex_config)
        .into_iter()
        .map(|port| RemoteSshReverseForward {
            remote_port: port,
            local_port: port,
        })
        .collect()
}

fn collect_remote_proxy_ports(
    agent_env: &[(String, String)],
    codex_config: Option<&str>,
) -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    for (_, value) in agent_env {
        collect_loopback_proxy_ports(value, &mut ports);
    }
    if let Some(config) = codex_config {
        collect_loopback_proxy_ports(config, &mut ports);
    }
    ports
}

fn collect_loopback_proxy_ports(value: &str, ports: &mut BTreeSet<u16>) {
    for marker in ["http://127.0.0.1:", "http://localhost:"] {
        let mut rest = value;
        while let Some(index) = rest.find(marker) {
            let after = &rest[index + marker.len()..];
            let digits = after
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>();
            if let Ok(port) = digits.parse::<u16>()
                && port > 0
            {
                ports.insert(port);
            }
            rest = after;
        }
    }
}

pub(super) fn remote_proxy_port_map(
    remote_ssh: &RemoteSshSessionConfig,
    local_ports: &mut BTreeSet<u16>,
) -> anyhow::Result<BTreeMap<u16, u16>> {
    if local_ports.is_empty() {
        return Ok(BTreeMap::new());
    }

    let runner = SystemRemoteSshCommandRunner;
    let ports = local_ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        remote_ssh.ssh_target.clone(),
        remote_ssh.ssh_port,
        format!(
            "python3 - {ports} <<'PY'\n{}\nPY",
            REMOTE_PROXY_PORT_PROBE_SCRIPT
        ),
        remote_ssh.ssh_password.as_deref(),
        Duration::from_secs(8),
    ));
    if !output.success {
        anyhow::bail!(
            "{}",
            first_remote_ssh_message("远程 proxy 端口探测失败", &output)
        );
    }

    let mut port_map = BTreeMap::new();
    for line in output.stdout.lines() {
        let Some((local, remote)) = line.trim().split_once('=') else {
            continue;
        };
        let local_port = local.parse::<u16>().ok();
        let remote_port = remote.parse::<u16>().ok();
        if let (Some(local_port), Some(remote_port)) = (local_port, remote_port)
            && local_ports.contains(&local_port)
            && remote_port > 0
        {
            port_map.insert(local_port, remote_port);
        }
    }

    for local_port in local_ports.iter() {
        port_map.entry(*local_port).or_insert(*local_port);
    }
    Ok(port_map)
}

fn ensure_local_proxy_ports_reachable(local_ports: &BTreeSet<u16>) -> anyhow::Result<()> {
    for port in local_ports {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], *port));
        let stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(
                |error| {
                    anyhow::anyhow!(
                        "本地 Codex API proxy 未监听 127.0.0.1:{}，远程模型请求无法通过 SSH 反向隧道转发：{}",
                        port,
                        error
                    )
                },
            )?;
        drop(stream);
    }
    Ok(())
}

const REMOTE_PROXY_PORT_PROBE_SCRIPT: &str = r#"
import socket
import sys

def reserve(preferred, used):
    attempts = []
    if preferred > 0:
        attempts.append(preferred)
    attempts.append(0)
    for port in attempts:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            sock.bind(("127.0.0.1", port))
            actual = sock.getsockname()[1]
            if actual not in used:
                return sock, actual
        except OSError:
            sock.close()
            continue
    raise SystemExit(1)

sockets = []
used = set()
for arg in sys.argv[1:]:
    local = int(arg)
    sock, remote = reserve(local, used)
    sockets.append(sock)
    used.add(remote)
    print(f"{local}={remote}", flush=True)
"#;

fn rewrite_loopback_proxy_ports_in_env(
    agent_env: &mut [(String, String)],
    port_map: &BTreeMap<u16, u16>,
) {
    for (_, value) in agent_env {
        rewrite_loopback_proxy_ports(value, port_map);
    }
}

fn rewrite_loopback_proxy_ports(value: &mut String, port_map: &BTreeMap<u16, u16>) {
    for (local_port, remote_port) in port_map {
        if local_port == remote_port {
            continue;
        }
        for host in ["127.0.0.1", "localhost"] {
            let from = format!("http://{host}:{local_port}");
            let to = format!("http://{host}:{remote_port}");
            *value = value.replace(&from, &to);
        }
    }
}

fn first_remote_ssh_message(prefix: &str, output: &crate::remote_ssh::RemoteSshOutput) -> String {
    if output.timed_out {
        return format!("{prefix}：SSH 命令超时");
    }
    let message = crate::remote_ssh::first_nonempty(&output.stderr, &output.stdout)
        .unwrap_or_else(|| "SSH 命令失败但没有输出".into());
    format!("{prefix}：{message}")
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
    fn remote_proxy_reverse_forwards_extract_loopback_ports_from_env_and_config() {
        let forwards = remote_proxy_reverse_forwards(
            &[
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "http://127.0.0.1:17852/v1/providers/timiai".into(),
                ),
                ("OTHER".into(), "nothing".into()),
            ],
            Some(r#"base_url = "http://localhost:17853/v1""#),
        );

        assert_eq!(
            forwards,
            vec![
                RemoteSshReverseForward {
                    remote_port: 17852,
                    local_port: 17852,
                },
                RemoteSshReverseForward {
                    remote_port: 17853,
                    local_port: 17853,
                },
            ]
        );
    }

    #[test]
    fn remote_proxy_reverse_forwards_ignore_non_proxy_env() {
        let forwards = remote_proxy_reverse_forwards(&[("TOKEN".into(), "secret".into())], None);

        assert!(forwards.is_empty());
    }

    #[test]
    fn remote_proxy_port_rewrite_updates_env_and_config_urls() {
        let mut env = vec![(
            "ANTHROPIC_BASE_URL".into(),
            "http://127.0.0.1:17851/v1/providers/kimi_code".into(),
        )];
        let mut config = r#"
[model_providers.byok]
base_url = "http://localhost:17851/v1"
"#
        .to_string();
        let port_map = BTreeMap::from([(17851, 24001)]);

        rewrite_loopback_proxy_ports_in_env(&mut env, &port_map);
        rewrite_loopback_proxy_ports(&mut config, &port_map);

        assert_eq!(env[0].1, "http://127.0.0.1:24001/v1/providers/kimi_code");
        assert!(config.contains(r#"base_url = "http://localhost:24001/v1""#));
        assert!(!config.contains(":17851"));
    }

    #[test]
    fn remote_proxy_port_rewrite_keeps_same_port_mapping_unchanged() {
        let mut value = "http://127.0.0.1:17851/v1".to_string();
        let port_map = BTreeMap::from([(17851, 17851)]);

        rewrite_loopback_proxy_ports(&mut value, &port_map);

        assert_eq!(value, "http://127.0.0.1:17851/v1");
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
