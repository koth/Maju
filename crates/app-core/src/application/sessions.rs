use super::*;

struct PreparedSessionRuntime {
    workspace_root: String,
    agent_env: Vec<(String, String)>,
    acp_port: u16,
    remote_ssh: Option<RemoteSshSessionConfig>,
    mcp_servers: Vec<acp_core::McpServer>,
    harness_endpoint: Option<String>,
    /// DeepSeek Harness default agent preset for a new session (from settings).
    /// `None` for non-harness agents.
    agent_preset: Option<String>,
    web_tools_mcp: Option<crate::web_tools_mcp::WebToolsLease>,
    image_mcp: Option<crate::image_mcp::ImageMcpLease>,
    image_capabilities: workspace_model::ImageCapabilities,
}

/// Resume overrides for [`Application::runtime_for_stored_session_with_override`].
///
/// The default (`Default`) keeps the stored-session heuristics. The fork
/// branch needs both knobs flipped: its session row is intentionally empty
/// (the transcript is rebuilt from the agent-side replay), so the
/// "no activity → drop stale agent id" cleanup must not fire, and the replay
/// must be applied (a normal resume already holds the transcript in SQLite
/// and skips replayed frames to avoid duplicates).
#[derive(Default)]
struct RuntimeResumeOverride {
    /// Use this agent-side session id as the resume id without the activity
    /// check. For a fork child this is the backend session created by the
    /// fork call moments ago.
    force_agent_session_id: Option<String>,
    /// Keep skip_replay off so the resume replay rebuilds (and persists) the
    /// branch transcript into the otherwise-empty row.
    force_replay: bool,
}

pub(super) fn prepare_web_tools_mcp(
    app_paths: &AppPaths,
    agent_command: &str,
    remote_session: bool,
) -> Result<
    (
        Vec<acp_core::McpServer>,
        Option<crate::web_tools_mcp::WebToolsLease>,
    ),
    String,
> {
    let is_codex = crate::settings::is_codex_acp_command(agent_command);
    let is_claude = crate::settings::is_claude_agent_acp_command(agent_command);
    if remote_session || !(is_codex || is_claude) {
        crate::startup_perf::mark(
            "web_tools_mcp/skipped",
            format!("remote_session={remote_session} is_codex={is_codex} is_claude={is_claude}"),
        );
        return Ok((Vec::new(), None));
    }
    let Some(lease) = web_tools_lease(app_paths)? else {
        return Ok((Vec::new(), None));
    };
    let server = acp_core::http_mcp_server(
        "kodex-web-tools",
        lease.url().to_string(),
        [(
            "x-kodex-web-tools-token".to_string(),
            lease.token().to_string(),
        )],
    );
    Ok((vec![server], Some(lease)))
}

/// Register this session's provider client on the shared `kodex-web-tools`
/// server. `Ok(None)` = web tools disabled or missing the provider key, which
/// is not an error.
pub(crate) fn web_tools_lease(
    app_paths: &AppPaths,
) -> Result<Option<crate::web_tools_mcp::WebToolsLease>, String> {
    let Some(config) = web_tools_config(app_paths)? else {
        return Ok(None);
    };
    let lease = crate::shared_mcp::shared_mcp()
        .web_tools_lease(config)
        .map_err(|error| format!("failed to start Kodex web tools MCP server: {error}"))?;
    crate::startup_perf::mark("web_tools_mcp/ready", format!("url={}", lease.url()));
    Ok(Some(lease))
}

/// The configured web-tools provider, or `None` when the feature is off or its
/// API key is missing.
fn web_tools_config(app_paths: &AppPaths) -> Result<Option<crate::web_tools::WebToolsConfig>, String> {
    let settings = crate::settings::load_app_settings(app_paths);
    if !settings.web_tools.enabled {
        crate::startup_perf::mark(
            "web_tools_mcp/disabled",
            format!("provider={}", settings.web_tools.provider),
        );
        return Ok(None);
    }
    let Some(api_key) =
        crate::settings::web_tools_provider_secret(app_paths, &settings.web_tools.provider)
    else {
        crate::startup_perf::mark(
            "web_tools_mcp/missing_secret",
            format!("provider={}", settings.web_tools.provider),
        );
        return Ok(None);
    };
    crate::startup_perf::mark(
        "web_tools_mcp/start",
        format!("provider={}", settings.web_tools.provider),
    );
    crate::web_tools::WebToolsConfig::for_provider(&settings.web_tools.provider, api_key)
        .map(Some)
        .map_err(|error| format!("failed to prepare Kodex web tools provider: {error}"))
}

/// Prepare the unified `kodex-image` MCP server for a session.
///
/// Mirrors `prepare_web_tools_mcp`: only local codex-acp / kodex-claude
/// sessions are eligible. When image fallback is active (`enabled` &&
/// `auto_enable`), resolves native image capabilities for the active
/// model/provider and starts the image MCP server whose `tools/list` is
/// trimmed to the missing capabilities. When inactive, returns the safe
/// "assume native" capabilities so no fallback override fires.
pub(super) fn prepare_image_mcp(
    app_paths: &AppPaths,
    agent_command: &str,
    model: &str,
    workspace_root: &str,
    remote_session: bool,
) -> Result<
    (
        Vec<acp_core::McpServer>,
        Option<crate::image_mcp::ImageMcpLease>,
        workspace_model::ImageCapabilities,
    ),
    String,
> {
    let is_codex = crate::settings::is_codex_acp_command(agent_command);
    let is_claude = crate::settings::is_claude_agent_acp_command(agent_command);
    let is_harness = crate::settings::is_deepseek_harness_command(agent_command);
    // Even when the image MCP fallback is not attached, resolve `native_view`
    // (the user's per-model declaration first, then the model name) so text-only
    // models correctly gate image attachments instead of being assumed capable
    // (Bug 1), and a vision model is not told it cannot see pictures.
    let provider = if is_codex {
        Some(crate::settings::codex_current_provider(app_paths))
    } else {
        None
    };
    let caps = crate::image_capability::resolve_image_capabilities_for_paths(
        app_paths,
        model,
        provider.as_deref(),
        agent_command,
    );
    if remote_session || !(is_codex || is_claude || is_harness) {
        return Ok((Vec::new(), None, caps));
    }
    let Some((lease, attached_caps)) = image_lease(app_paths, workspace_root, caps)? else {
        return Ok((Vec::new(), None, caps));
    };
    let server = acp_core::http_mcp_server(
        "kodex-image",
        lease.url().to_string(),
        [(
            "x-kodex-image-token".to_string(),
            lease.token().to_string(),
        )],
    );
    Ok((vec![server], Some(lease), attached_caps))
}

/// Register this session's capabilities and config on the shared `kodex-image`
/// server.
///
/// `Ok(None)` = image support disabled in settings, which is not an error. On
/// success the returned capabilities have `view_fallback` set: the lease is
/// registered, so a text-only model's attachments can be degraded through
/// `view_image`.
fn image_lease(
    app_paths: &AppPaths,
    workspace_root: &str,
    caps: workspace_model::ImageCapabilities,
) -> Result<Option<(crate::image_mcp::ImageMcpLease, workspace_model::ImageCapabilities)>, String> {
    let Some(config) = image_mcp_config(app_paths, workspace_root)? else {
        return Ok(None);
    };
    // The registration exists: a `view_image` fallback is available, so image
    // attachments are allowed even for text-only models (degraded through
    // `view_image` before reaching the model).
    let mut attached = caps;
    attached.view_fallback = true;
    let lease = crate::shared_mcp::shared_mcp()
        .image_lease(attached, config)
        .map_err(|error| format!("failed to start Kodex image MCP server: {error}"))?;
    crate::startup_perf::mark(
        "image_mcp/ready",
        format!(
            "workspace_root={workspace_root} native_view={} native_generate={} url={}",
            attached.native_view,
            attached.native_generate,
            lease.url()
        ),
    );
    Ok(Some((lease, attached)))
}

/// The session's image MCP config, or `None` when image support is disabled in
/// settings.
fn image_mcp_config(
    app_paths: &AppPaths,
    workspace_root: &str,
) -> Result<Option<crate::image_mcp::ImageMcpConfig>, String> {
    let settings = crate::settings::load_app_settings(app_paths);
    if !settings.image.enabled {
        crate::startup_perf::mark(
            "image_mcp/disabled",
            format!("enabled={}", settings.image.enabled),
        );
        return Ok(None);
    }
    crate::settings::validate_image_settings(&settings.image)
        .map_err(|error| format!("invalid image settings: {error}"))?;
    let view_api_key =
        crate::settings::image_view_provider_secret(app_paths, &settings.image.view.provider);
    let generate_api_key =
        crate::settings::image_generate_api_key(app_paths, &settings.image.generate);
    Ok(Some(crate::image_mcp::ImageMcpConfig {
        workspace_root: std::path::PathBuf::from(workspace_root),
        settings: settings.image.clone(),
        view_api_key,
        generate_api_key,
    }))
}

/// Register the local MCP servers Kodex exposes to `dsh web`, and build the
/// `dsh-mcp-client` rows that mount them.
///
/// The ACP channels receive these servers through `SessionConfig.mcp_servers`.
/// The harness has no such seam — `dsh web` ignores ACP MCP config — so the
/// rows travel through Kodex's `--patch` overlay instead (see
/// [`crate::dsh_bringup`]), pointing at the *same* shared servers.
///
/// The harness process serves every dsh session, so its image registration
/// cannot be trimmed per session model the way an ACP session's is: it
/// advertises everything the configured image settings can serve and lets the
/// model choose. Generated images land under Kodex's data root
/// ([`AppPaths::harness_image_output_root`]), not a session workspace.
///
/// Both servers are optional and a failure to start one only removes its tools.
pub(crate) fn harness_exposed_mcp(app_paths: &AppPaths) -> crate::dsh_bringup::HarnessExposedMcp {
    let mut exposed = crate::dsh_bringup::HarnessExposedMcp::default();

    match web_tools_lease(app_paths) {
        Ok(Some(lease)) => {
            exposed.rows.push(dsh_mcp_row(
                "kodex-web-tools-mcp",
                "kodex_web_tools",
                lease.url(),
                "x-kodex-web-tools-token",
                lease.token(),
            ));
            exposed.web_tools = Some(lease);
        }
        Ok(None) => {}
        Err(error) => crate::startup_perf::mark("dsh/web_tools_mcp_failed", error),
    }

    // The harness reaches models through Kodex's own BYOK routes, so nothing on
    // that side can view, generate, or edit an image natively: offer every tool
    // the image settings can serve.
    let caps = workspace_model::ImageCapabilities {
        native_view: false,
        native_generate: false,
        native_edit: false,
        view_fallback: false,
    };
    let output_root = app_paths.harness_image_output_root();
    match image_lease(app_paths, &output_root.display().to_string(), caps) {
        Ok(Some((lease, _caps))) => {
            exposed.rows.push(dsh_mcp_row(
                "kodex-image-mcp",
                "kodex_image",
                lease.url(),
                "x-kodex-image-token",
                lease.token(),
            ));
            exposed.image = Some(lease);
        }
        Ok(None) => {}
        Err(error) => crate::startup_perf::mark("dsh/image_mcp_failed", error),
    }

    exposed
}

fn dsh_mcp_row(
    id: &str,
    server_name: &str,
    url: &str,
    header_name: &str,
    header_value: &str,
) -> dsh_bridge::HarnessMcpServer {
    dsh_bridge::HarnessMcpServer {
        id: id.to_string(),
        server_name: server_name.to_string(),
        url: url.to_string(),
        header_name: header_name.to_string(),
        header_value: header_value.to_string(),
    }
}

fn remote_machine_profile_from_workspace(
    remote: &RemoteLinuxWorkspace,
) -> workspace_model::RemoteMachineProfile {
    workspace_model::RemoteMachineProfile {
        id: remote.profile_id.unwrap_or_else(uuid::Uuid::new_v4),
        display_name: remote.display_name(),
        ssh_target: remote.ssh_target.clone(),
        ssh_port: remote.ssh_port,
        created_at_ms: 0,
        updated_at_ms: 0,
        last_validation: None,
    }
}

fn session_status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Streaming => "Streaming",
        SessionStatus::WaitingForTool => "WaitingForTool",
        SessionStatus::Interrupted => "Interrupted",
    }
}

impl Application {
    pub(super) fn should_auto_reconnect_after_clean_exit(&self) -> bool {
        false
    }

    pub(super) fn is_codex_acp_session(&self) -> bool {
        self.ui
            .session
            .agent_cli
            .as_deref()
            .map(is_codex_agent_label)
            .unwrap_or_else(|| {
                let command = self.agent_command.to_ascii_lowercase();
                command.contains("codex-acp") || command.contains("kodex-acp")
            })
    }

    #[cfg(test)]
    pub(super) fn agent_command_for_new_session(&self, agent: Option<AgentCliId>) -> String {
        match agent {
            Some(agent) if self.remote_agent_selection_matches_current(agent) => {
                self.agent_command.clone()
            }
            Some(agent) => self
                .command_for_agent_in_current_workspace(agent)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                }),
            None => self.agent_command.clone(),
        }
    }

    fn prepare_agent_command_for_new_session(
        &self,
        agent: Option<AgentCliId>,
    ) -> Result<String, String> {
        match agent {
            Some(agent) if self.remote_agent_selection_matches_current(agent) => {
                Ok(self.agent_command.clone())
            }
            Some(agent) if self.is_remote_workspace() => self.bootstrap_remote_agent_command(agent),
            Some(agent) => Ok(self
                .command_for_agent_in_current_workspace(agent)
                .unwrap_or_else(|| {
                    crate::settings::resolve_agent_command_with_settings(&self.app_paths)
                })),
            None => Ok(self.agent_command.clone()),
        }
    }

    fn prepare_agent_command_for_stored_label(
        &self,
        label: &str,
    ) -> Result<Option<String>, String> {
        if !self.is_remote_workspace() {
            return Ok(self.command_for_agent_label_in_current_workspace(label));
        }
        let Some(agent) = crate::settings::agent_id_for_label(label) else {
            return Ok(None);
        };
        if self.remote_agent_selection_matches_current(agent) {
            return Ok(Some(self.agent_command.clone()));
        }
        self.bootstrap_remote_agent_command(agent).map(Some)
    }

    fn bootstrap_remote_agent_command(&self, agent: AgentCliId) -> Result<String, String> {
        let remote = self.current_remote_workspace().ok_or_else(|| {
            "Remote workspace is missing metadata; reopen the remote directory first".to_string()
        })?;
        let profile = remote
            .profile_id
            .and_then(|profile_id| {
                crate::remote_profiles::get_remote_machine_profile(&self.app_paths, profile_id).ok()
            })
            .unwrap_or_else(|| remote_machine_profile_from_workspace(&remote));
        let ssh_password = remote.ssh_password.as_deref().or_else(|| {
            self.remote_ssh
                .as_ref()
                .and_then(|ssh| ssh.ssh_password.as_deref())
        });

        crate::remote_bootstrap::bootstrap_remote_agent(
            crate::remote_bootstrap::RemoteAgentBootstrapRequest {
                request_id: uuid::Uuid::new_v4(),
                profile: &profile,
                remote_path: &remote.remote_path,
                ssh_password,
                agent_cli: agent,
            },
            &crate::remote_ssh::SystemRemoteSshCommandRunner,
            |_| {},
        )
        .map(|bootstrap| bootstrap.agent_command)
        .map_err(|e| e.to_string())
    }

    fn current_remote_workspace(&self) -> Option<RemoteLinuxWorkspace> {
        match &self.ui.workspace.location {
            workspace_model::WorkspaceLocation::RemoteLinux(remote) => Some(remote.clone()),
            workspace_model::WorkspaceLocation::Local => None,
        }
    }

    fn remote_agent_selection_matches_current(&self, agent: AgentCliId) -> bool {
        if !self.is_remote_workspace() {
            return false;
        }
        if matches!(
            &self.ui.workspace.location,
            workspace_model::WorkspaceLocation::RemoteLinux(remote)
                if remote.agent_cli == Some(agent)
        ) {
            return true;
        }
        crate::settings::agent_label_for_id(agent)
            .is_some_and(|label| self.ui.session.agent_cli.as_deref() == Some(label))
    }

    pub(super) fn command_for_agent_in_current_workspace(
        &self,
        agent: AgentCliId,
    ) -> Option<String> {
        if self.is_remote_workspace() {
            crate::settings::remote_linux_command_for_agent(agent)
        } else {
            crate::settings::command_for_agent_with_paths(agent, &self.app_paths)
        }
    }

    pub(super) fn command_for_agent_label_in_current_workspace(
        &self,
        label: &str,
    ) -> Option<String> {
        if self.is_remote_workspace() {
            crate::settings::remote_linux_command_for_agent_label(label)
        } else {
            crate::settings::command_for_agent_label_with_paths(label, &self.app_paths)
        }
    }

    fn prepare_session_runtime(
        &self,
        agent_command: &str,
        model: &str,
        preset_override: Option<String>,
    ) -> Result<PreparedSessionRuntime, String> {
        self.prepare_session_runtime_for_resume(agent_command, model, preset_override, false, None)
    }

    fn prepare_session_runtime_for_resume(
        &self,
        agent_command: &str,
        model: &str,
        preset_override: Option<String>,
        resuming_existing_session: bool,
        // The session's own persisted workspace root. Resuming a stored
        // session must use the workspace the session was created in, not the
        // workspace currently open in the UI: for dsh-harness sessions the
        // harness rejects a resume whose cwd differs from the persisted one
        // (`session-conflict`). New sessions pass `None` (they belong to the
        // current workspace). Remote workspaces ignore this — their root
        // comes from the SSH config.
        workspace_root_override: Option<String>,
    ) -> Result<PreparedSessionRuntime, String> {
        if self.is_remote_workspace() {
            return self.prepare_remote_session_runtime(agent_command);
        }

        // DeepSeek Harness: no ACP subprocess, no codex/web-tools MCP.
        // Kodex writes the dsh settings document, spawns `dsh web`, and the
        // returned endpoint selects the harness backend via
        // `SessionConfig.harness_endpoint`. `mcp_servers` stays empty because
        // `dsh web` ignores ACP MCP config; the same local servers are mounted
        // on the harness process through Kodex's `--patch` overlay instead
        // (`harness_exposed_mcp`), and the `kodex-image` handle is *also* kept
        // per session so text-only models accept attachments degraded through
        // the view model — mirroring the codex text-only path.
        if crate::settings::is_deepseek_harness_command(agent_command) {
            let workspace_root = workspace_root_override
                .clone()
                .filter(|root| !root.is_empty())
                .unwrap_or_else(|| self.session_config_workspace_root(None));
            // Kodex's local MCP servers are mounted on the harness process by
            // the bring-up itself (one set per `dsh web`, reused by every
            // session), so nothing extra is passed here.
            let harness_endpoint =
                crate::dsh_bringup::dsh_bringup().ensure_harness_endpoint(&self.app_paths)?;
            // Per-session preset override wins over the global `dsh_default_preset`
            // setting; fall back to the configured default when none is supplied.
            // When RESUMING, the stored preset is passed through so the UI can
            // restore it — dsh-bridge strips it from `session.create` to avoid
            // the `agent-preset-conflict` error.
            let agent_preset = preset_override
                .filter(|preset| !preset.trim().is_empty())
                .or_else(|| {
                    crate::settings::load_app_settings(&self.app_paths).dsh_default_preset
                });
            // Attach the `kodex-image` fallback when image settings are enabled
            // so text-only harness models (e.g. DeepSeek) accept image
            // attachments degraded through the view model — mirroring the
            // codex text-only path. The MCP server entry is discarded: the
            // harness ignores ACP MCP config, and app-core only needs the
            // handle for prompt-level degradation. A misconfigured view
            // provider must not block session creation, so fall back to "no
            // image support" on error (matching the bootstrap path).
            let (image_mcp, image_capabilities) =
                match prepare_image_mcp(&self.app_paths, agent_command, model, &workspace_root, false)
                {
                    Ok((_image_servers, handle, caps)) => (handle, caps),
                    Err(error) => {
                        crate::startup_perf::mark("dsh/image_mcp_failed", error);
                        (None, workspace_model::ImageCapabilities::default())
                    }
                };
            // The harness's own `kodex-image` registration is process-wide, so
            // point it at this session's capabilities: the model that is active
            // decides whether `view_image` is mounted at all. A vision model
            // must not be handed a tool that says it cannot see images.
            crate::dsh_bringup::dsh_bringup()
                .update_harness_image_capabilities(image_capabilities);
            return Ok(PreparedSessionRuntime {
                workspace_root,
                agent_env: Vec::new(),
                acp_port: 0,
                remote_ssh: None,
                mcp_servers: Vec::new(),
                harness_endpoint: Some(harness_endpoint),
                agent_preset,
                web_tools_mcp: None,
                image_mcp,
                image_capabilities,
            });
        }

        crate::settings::ensure_agent_ready_for_command(agent_command, &self.app_paths)
            .map_err(|e| e.to_string())?;
        let workspace_root = workspace_root_override
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| self.session_config_workspace_root(None));
        let (mut mcp_servers, web_tools_mcp) =
            prepare_web_tools_mcp(&self.app_paths, agent_command, false)?;
        let (image_servers, image_mcp, image_capabilities) = prepare_image_mcp(
            &self.app_paths,
            agent_command,
            model,
            &workspace_root,
            false,
        )?;
        mcp_servers.extend(image_servers);
        Ok(PreparedSessionRuntime {
            workspace_root,
            agent_env: crate::settings::agent_env_for_command(agent_command, &self.app_paths),
            acp_port: self.acp_port,
            remote_ssh: None,
            mcp_servers,
            harness_endpoint: None,
            agent_preset: None,
            web_tools_mcp,
            image_mcp,
            image_capabilities,
        })
    }

    fn prepare_remote_session_runtime(
        &self,
        agent_command: &str,
    ) -> Result<PreparedSessionRuntime, String> {
        let mut remote_ssh = self.remote_ssh.clone().ok_or_else(|| {
            "Remote workspace is not connected; reopen the remote directory first".to_string()
        })?;
        let local_port =
            super::bootstrap::find_available_loopback_port().map_err(|e| e.to_string())?;
        let mut agent_ports = std::collections::BTreeSet::from([local_port]);
        let port_map = super::bootstrap::remote_proxy_port_map(&remote_ssh, &mut agent_ports)
            .map_err(|e| e.to_string())?;
        let remote_port = port_map.get(&local_port).copied().unwrap_or(local_port);
        remote_ssh.local_port = local_port;
        remote_ssh.remote_port = remote_port;
        remote_ssh.reverse_forwards.clear();
        let workspace_root = self.session_config_workspace_root(Some(&remote_ssh));

        let remote_runtime = super::bootstrap::prepare_remote_agent_runtime(
            agent_command,
            &self.app_paths,
            &remote_ssh,
        )
        .map_err(|e| e.to_string())?;
        remote_ssh.reverse_forwards = remote_runtime.reverse_forwards;

        Ok(PreparedSessionRuntime {
            workspace_root,
            agent_env: remote_runtime.agent_env,
            acp_port: local_port,
            remote_ssh: Some(remote_ssh),
            mcp_servers: Vec::new(),
            harness_endpoint: None,
            agent_preset: None,
            web_tools_mcp: None,
            image_mcp: None,
            image_capabilities: workspace_model::ImageCapabilities::assumed_native(),
        })
    }

    pub(super) fn session_config_workspace_root(
        &self,
        remote_ssh: Option<&RemoteSshSessionConfig>,
    ) -> String {
        remote_ssh
            .map(|config| config.remote_workspace_root.clone())
            .unwrap_or_else(|| self.ui.workspace.root.display().to_string())
    }

    // ── Session management ──

    pub fn session_list(&self) -> Result<Vec<SessionListItem>, String> {
        let mut sessions = self.store.list_sessions().map_err(|e| e.to_string())?;
        self.runtime_registry
            .annotate_sessions(&mut sessions, &self.ui.session.id.to_string());
        self.annotate_visible_session_summary(&mut sessions);
        Ok(sessions)
    }

    pub fn session_list_after_poll(&mut self) -> Result<Vec<SessionListItem>, String> {
        self.poll_prompt_progress();
        self.session_list()
    }

    pub fn session_list_for_visibility(
        &self,
        workspace_visible: bool,
    ) -> Result<Vec<SessionListItem>, String> {
        let mut sessions = self.session_list()?;
        if !workspace_visible {
            self.annotate_visible_session_as_background(&mut sessions);
        }
        Ok(sessions)
    }

    pub fn session_list_after_poll_for_visibility(
        &mut self,
        workspace_visible: bool,
    ) -> Result<Vec<SessionListItem>, String> {
        self.poll_prompt_progress();
        self.session_list_for_visibility(workspace_visible)
    }

    fn annotate_visible_session_summary(&self, sessions: &mut [SessionListItem]) {
        let visible_session_id = self.ui.session.id.to_string();
        let Some(item) = sessions
            .iter_mut()
            .find(|session| session.id == visible_session_id)
        else {
            return;
        };

        item.title = self.ui.session.title.clone();
        item.status = session_status_label(&self.ui.session.status).to_string();
        if self.ui.session.agent_cli.is_some() {
            item.agent_cli = self.ui.session.agent_cli.clone();
        }
    }

    fn annotate_visible_session_as_background(&self, sessions: &mut [SessionListItem]) {
        let visible_session_id = self.ui.session.id.to_string();
        let Some(item) = sessions
            .iter_mut()
            .find(|session| session.id == visible_session_id)
        else {
            return;
        };

        if self.runtime_needs_attention() {
            item.attention_state = SessionAttentionState::NeedsAttention;
        }

        item.runtime_status =
            if self.in_flight_prompt.is_some() || self.pending_image_degradation.is_some() {
                SessionRuntimeStatus::BackgroundRunning
            } else {
                SessionRuntimeStatus::BackgroundIdle
            };
    }

    pub fn session_switch(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            self.runtime_registry.clear_attention(id);
            // The phone may be re-entering this session with empty local state
            // (app restart, machine re-selection). Broadcast so the relay event
            // source wakes; the bridge resets its patch cursor on SwitchSession
            // requests, so this wake re-pushes a Full snapshot instead of
            // letting the phone sit on its 1.5s GetState fallback.
            self.broadcast_ui_updated();
            return Ok(());
        }

        self.ensure_codex_provider_matches_for_resume(id)?;
        let target_runtime = if let Some(runtime) = self.runtime_registry.remove(id) {
            runtime
        } else {
            self.runtime_for_stored_session(id)?
        };

        let background_runtime = self.install_runtime_as_visible(target_runtime);
        self.runtime_registry.insert(background_runtime);
        self.ui.session.status =
            if self.in_flight_prompt.is_some() || self.pending_image_degradation.is_some() {
                SessionStatus::Streaming
            } else {
                self.ui.session.status.clone()
            };
        self.poll_current_runtime_progress();
        self.bump_revision();
        // Remote (phone) entry into a session relies on the pushed Full
        // snapshot: bump_revision alone does not signal subscribers, so the
        // relay event source would stay asleep after a switch and the phone
        // would wait on its sync fallback. Broadcast like every other UI
        // change so AppUpdateEventSource wakes and pushes the Full delta.
        self.broadcast_ui_updated();
        Ok(())
    }

    /// Rewrite the desktop's local handoff digest into a readable briefing with
    /// one model call.
    ///
    /// Uses the provider configured for **session titles**
    /// (`settings.session_title`) through the same local `codex_api_proxy` route
    /// the image-view fallback uses, so any configured BYOK provider works
    /// without new surface area. Blocking by design — the desktop command awaits
    /// it on the blocking pool, matching `session_fork` — and every failure is
    /// returned as a message for the dialog to show while it keeps the local
    /// digest.
    pub fn generate_handoff_summary(&self, material: &str) -> Result<String, String> {
        let Some((provider, model)) = crate::settings::session_title_route(&self.app_paths) else {
            return Err("未配置交接摘要模型（设置 → 会话标题）".to_string());
        };
        // BYOK keys are shared across families, so the image-view lookup is the
        // right accessor for any configured provider.
        let api_key = crate::settings::image_view_provider_secret(&self.app_paths, &provider);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to create handoff runtime: {error}"))?;
        runtime.block_on(crate::handoff::polish_handoff(
            &provider,
            &model,
            api_key.as_deref(),
            material,
        ))
    }

    pub fn session_create(
        &mut self,
        agent: Option<AgentCliId>,
        preset: Option<String>,
    ) -> Result<(), String> {
        // Reuse the current session when it has no activity yet: opening a
        // workspace bootstraps an empty placeholder session, so a fresh
        // "��建对话" from the sidebar would otherwise create a second empty
        // session alongside the bootstrap one. Activating the placeholder
        // (optionally switching the agent) avoids the duplicate.
        if self.ui.workspace.root == self.app_paths.chats_workspace_root()
            && !self
                .store
                .session_has_activity(&self.ui.session.id.to_string())
                .unwrap_or(true)
        {
            self.poll_current_runtime_progress();
            self.bump_revision();
            return Ok(());
        }
        let runtime = self.runtime_for_new_session(agent, preset)?;
        let background_runtime = self.install_runtime_as_visible(runtime);
        self.runtime_registry.insert(background_runtime);
        self.bump_revision();
        Ok(())
    }

    /// Fork the conversation from a completed assistant message
    /// ("从这里创建聊天分支"). Creates a branch session on the agent backend
    /// seeded with the history through the end of the selected turn, mirrors it
    /// as a local session row, and switches the visible session to the branch.
    pub fn session_fork(
        &mut self,
        message_id: &str,
        mode: workspace_model::SessionForkMode,
    ) -> Result<workspace_model::SessionForkOutcome, String> {
        // The fork cut lands at a completed turn; an in-flight turn cannot be
        // forked, and the source session must be idle (the backend rejects a
        // fork anchored inside an open turn anyway — fail with a clear message
        // before touching the backend).
        if self.in_flight_prompt.is_some() || self.ui.session.status != SessionStatus::Idle {
            return Err("会话运行中无法分叉，请等当前轮次结束".into());
        }
        match mode {
            workspace_model::SessionForkMode::Workspace => {}
            workspace_model::SessionForkMode::Worktree => {
                return Err("新工作树分叉暂未开放，请选择当前工作空间分支".into());
            }
        }
        self.ensure_local_workspace_for("fork conversations")?;

        let at_user_turn = self.fork_turn_ordinal_for_message(message_id)?;
        // Content anchor: the ordinal alone can mis-cut on agent backends whose
        // turn counter diverges from kodex's turn-opening count (the dsh
        // harness counts injected turns like subagent notifications and
        // splice-joined prompts). The prompt text pins the exact turn.
        let (user_message_text, user_message_occurrence) =
            self.fork_prompt_anchor(message_id);
        // Backend fork → child agent-side session id (harness session id for
        // dsh, ACP session id for codex). Blocking reply: the fork is a fast
        // control-plane call, and the local row needs the id before it can be
        // created. The caller holds the app mutex, but the fork must finish
        // first — same trade as `session_switch`'s resume handshake.
        let child_agent_session_id = self
            .session
            .fork_session(at_user_turn, user_message_text, user_message_occurrence)
            .map_err(|error| error.to_string())?;

        // Local branch row in this workspace's store. The transcript itself is
        // NOT copied locally: the backend replays the child's seeded history
        // on resume (dsh `session.history` replay / ACP `session/load`), which
        // rebuilds the UI and repopulates the child's rows.
        let child_id = uuid::Uuid::new_v4();
        let source_session_id = self.ui.session.id.to_string();
        let (source_model, source_provider, source_mode) = self
            .store
            .get_session_model_provider_mode(&source_session_id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| {
                (
                    self.ui.session.model.clone(),
                    self.current_model_provider_for_persistence(),
                    self.ui.session.mode.clone(),
                )
            });
        self.store
            .create_session(&child_id.to_string(), &source_model)
            .map_err(|e| e.to_string())?;
        let child_title = if self.ui.session.title.trim().is_empty() {
            "分叉会话".to_string()
        } else {
            format!("{} · 分支", self.ui.session.title.trim())
        };
        let _ = self
            .store
            .update_session_title(&child_id.to_string(), &child_title);
        let _ = self
            .store
            .update_acp_session_id(&child_id.to_string(), &child_agent_session_id);
        let _ = self.store.update_session_model_mode_provider(
            &child_id.to_string(),
            &source_model,
            source_provider.as_deref(),
            source_mode.as_deref(),
        );
        if let Some(preset) = self
            .store
            .get_session_agent_preset(&source_session_id)
            .ok()
            .flatten()
        {
            let _ = self
                .store
                .update_session_agent_preset(&child_id.to_string(), Some(&preset));
        }
        if let Some(agent_cli) = self.ui.session.agent_cli.clone() {
            let _ = self
                .store
                .update_session_agent_cli(&child_id.to_string(), &agent_cli);
        }

        // Install the branch as the visible session via the stored-session
        // resume path; the old visible session parks as a background runtime.
        // Overrides: the child row is empty by design, so (1) resume the
        // backend fork session explicitly — the activity-gated lookup would
        // otherwise clear the id and the branch would boot as a blank session
        // with no context — and (2) keep the replay applied so the harness/ACP
        // history rebuilds and persists the branch transcript.
        let runtime = self.runtime_for_stored_session_with_override(
            &child_id.to_string(),
            RuntimeResumeOverride {
                force_agent_session_id: Some(child_agent_session_id.clone()),
                force_replay: true,
            },
        )?;
        let background_runtime = self.install_runtime_as_visible(runtime);
        self.runtime_registry.insert(background_runtime);
        self.bump_revision();
        self.broadcast_ui_updated();

        Ok(workspace_model::SessionForkOutcome {
            session_id: child_id,
            workspace_root: None,
            worktree_branch: None,
        })
    }

    /// Resolve the 1-based fork-turn ordinal for a message: the ordinal of the
    /// turn containing it, counted by turn-opening user messages. Steers join
    /// the current turn and `/compact` intercepts never start a turn (mirrors
    /// the frontend retry heuristic), so both are excluded from the count —
    /// keeping the ordinal aligned with the backend's completed-turn count.
    ///
    /// Walks the session's FULL persisted history: the visible UI only holds a
    /// tail window on long sessions, and the fork picker must be able to anchor
    /// on any turn.
    pub(super) fn fork_turn_ordinal_for_message(
        &self,
        message_id: &str,
    ) -> Result<u64, String> {
        let session_id = self.ui.session.id.to_string();
        let messages = self
            .store
            .load_session_messages(&session_id)
            .map_err(|e| e.to_string())?;
        let mut turn_ordinal: u64 = 0;
        let mut target_turn: Option<u64> = None;
        for message in &messages {
            let is_target = message.id.to_string() == message_id;
            match message.role {
                workspace_model::MessageRole::User => {
                    let is_turn_opening =
                        !message.is_steer && !message.body.trim().eq_ignore_ascii_case("/compact");
                    if is_turn_opening {
                        turn_ordinal += 1;
                    }
                    if is_target {
                        // A turn-opening prompt forks through its own turn; a
                        // steer or /compact joins the current turn.
                        target_turn = Some(turn_ordinal);
                    }
                }
                workspace_model::MessageRole::Assistant | workspace_model::MessageRole::System => {
                    if is_target {
                        target_turn = Some(turn_ordinal);
                    }
                }
            }
            if target_turn.is_some() {
                break;
            }
        }
        match target_turn {
            Some(turn) if turn >= 1 => Ok(turn),
            Some(_) => Err("无法分叉：该消息之前还没有已完成的对话轮次".into()),
            None => Err("无法分叉：未找到该消息".into()),
        }
    }

    /// Content anchor for the fork target: the target turn's opening prompt
    /// text plus how many times that exact text appeared among the session's
    /// turn-opening prompts up to (and including) the target. The dsh bridge
    /// matches the harness `user/message` event carrying this text to cut at
    /// the correct turn even when the harness turn counter diverges from
    /// kodex's count (injected turns, splice-joined prompts). Returns
    /// `(None, 0)` when no anchor can be built — the caller then falls back
    /// to the legacy ordinal anchoring.
    pub(super) fn fork_prompt_anchor(&self, message_id: &str) -> (Option<String>, u64) {
        let Ok(messages) = self
            .store
            .load_session_messages(&self.ui.session.id.to_string())
        else {
            return (None, 0);
        };
        fn is_turn_opening(message: &workspace_model::ChatMessage) -> bool {
            message.role == workspace_model::MessageRole::User
                && !message.is_steer
                && !message.body.trim().eq_ignore_ascii_case("/compact")
        }
        let Some(target_index) = messages
            .iter()
            .position(|message| message.id.to_string() == message_id)
        else {
            return (None, 0);
        };
        // A steer/compact/system selection belongs to the turn that owns it.
        let Some(target) = messages[..=target_index]
            .iter()
            .rev()
            .find(|message| is_turn_opening(message))
        else {
            return (None, 0);
        };
        let occurrence = messages[..=target_index]
            .iter()
            .filter(|message| is_turn_opening(message) && message.body == target.body)
            .count() as u64;
        (Some(target.body.clone()), occurrence.max(1))
    }

    /// Branch points for the fork picker: every completed turn of the session,
    /// built from the FULL persisted history (the UI window may hide older
    /// turns). Each candidate anchors on the turn's opening user prompt —
    /// forking from it keeps turns 1..N.
    pub fn session_fork_candidates(
        &self,
    ) -> Result<Vec<workspace_model::SessionForkCandidate>, String> {
        let session_id = self.ui.session.id.to_string();
        let messages = self
            .store
            .load_session_messages(&session_id)
            .map_err(|e| e.to_string())?;

        const EXCERPT_CHARS: usize = 160;
        fn excerpt(text: &str) -> String {
            let trimmed: String = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let mut owned = trimmed;
            if owned.chars().count() > EXCERPT_CHARS {
                owned = owned.chars().take(EXCERPT_CHARS).collect::<String>() + "…";
            }
            owned
        }

        let mut candidates: Vec<workspace_model::SessionForkCandidate> = Vec::new();
        for message in &messages {
            match message.role {
                workspace_model::MessageRole::User => {
                    let is_turn_opening = !message.is_steer
                        && !message.body.trim().eq_ignore_ascii_case("/compact")
                        && !message.body.trim().is_empty();
                    if is_turn_opening {
                        candidates.push(workspace_model::SessionForkCandidate {
                            turn_ordinal: candidates.len() as u64 + 1,
                            user_message_id: message.id,
                            user_excerpt: excerpt(&message.body),
                            reply_excerpt: String::new(),
                        });
                    }
                }
                workspace_model::MessageRole::Assistant => {
                    if message.body.trim().is_empty() {
                        continue;
                    }
                    // The turn's latest non-empty reply previews the outcome of
                    // forking through it.
                    if let Some(last) = candidates.last_mut() {
                        last.reply_excerpt = excerpt(&message.body);
                    }
                }
                workspace_model::MessageRole::System => {}
            }
        }
        Ok(candidates)
    }

    pub fn session_delete(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            let replacement_id = self
                .store
                .list_sessions()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|session| session.id != id)
                .map(|session| session.id);

            if let Some(replacement_id) = replacement_id {
                self.session_switch(&replacement_id)?;
            } else {
                self.session_create(None, None)?;
            }
        }

        if let Some(mut runtime) = self.runtime_registry.remove_all_state(id) {
            runtime.session.shutdown();
        }
        self.store.delete_session(id).map_err(|e| e.to_string())
    }

    pub fn session_archive(&mut self, id: &str) -> Result<(), String> {
        if self.ui.session.id.to_string() == id {
            let replacement_id = self
                .store
                .list_sessions()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|session| session.id != id)
                .map(|session| session.id);

            if let Some(replacement_id) = replacement_id {
                self.session_switch(&replacement_id)?;
            } else {
                self.session_create(None, None)?;
            }
        }

        if let Some(mut runtime) = self.runtime_registry.remove_all_state(id) {
            runtime.session.shutdown();
        }
        self.store.archive_session(id).map_err(|e| e.to_string())
    }

    pub fn reconnect_session(&mut self) -> Result<(), String> {
        self.ensure_codex_provider_matches_for_resume(&self.ui.session.id.to_string())?;

        let session_id = self.ui.session.id.to_string();
        let has_activity = self
            .store
            .session_has_activity(&session_id)
            .unwrap_or(false);
        let resume_id = if has_activity && !self.session.id.is_empty() {
            Some(self.session.id.clone())
        } else {
            self.resume_acp_session_id_for_stored_session(&session_id)
        };

        let resume_id_for_handle = resume_id.clone();
        let has_resume_id = resume_id_for_handle.is_some();
        let agent_command = self.agent_command.clone();
        // On reconnect, restore the session's own persisted preset (from a
        // previous `session.create` ack) so dsh does not fall back to the
        // global default — the preset is fixed at creation and dsh rejects a
        // conflicting preset on resume with `agent-preset-conflict`.
        let reconnect_preset = self
            .store
            .get_session_agent_preset(&session_id)
            .ok()
            .flatten();
        let prepared_runtime = self.prepare_session_runtime_for_resume(
            &agent_command,
            &self.ui.session.model,
            reconnect_preset,
            has_resume_id,
            // The active session belongs to the current workspace by
            // construction — no override needed.
            None,
        )?;
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: self.ui.session.model.clone(),
            agent_command: agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: resume_id,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
            mcp_servers: prepared_runtime.mcp_servers.clone(),
            harness_endpoint: prepared_runtime.harness_endpoint.clone(),
            agent_preset: prepared_runtime.agent_preset.clone(),
        })
        .map_err(|e| e.to_string())?;
        if let Some(acp_id) = resume_id_for_handle {
            session.id = acp_id;
        }
        let current_mode = self.ui.session.mode.as_deref().unwrap_or("Build");
        let _ = session.set_permission_mode(current_mode);
        let _ = super::config::queue_codex_agent_mode_for_policy_mode(
            &mut session,
            self.is_codex_acp_session(),
            Some(current_mode),
        );

        self.session = session;
        self.agent_command = agent_command;
        self.acp_port = prepared_runtime.acp_port;
        self.remote_ssh = prepared_runtime.remote_ssh;
        self.web_tools_mcp = prepared_runtime.web_tools_mcp;
        self.image_mcp = prepared_runtime.image_mcp;
        self.ui.image_capabilities = prepared_runtime.image_capabilities;
        self.ui.session.status = SessionStatus::Idle;
        self.ui.prompt_capabilities = Default::default();
        self.ui.available_commands.clear();
        self.ui.agent_plan.clear();
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut self.ui.tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = self
                .ui
                .tools
                .iter()
                .find(|tool| tool.id.to_string() == *tool_id)
            {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }
        self.in_flight_prompt = None;
        self.current_turn_user_message_id = None;
        self.agent_title_received = false;
        self.provisional_prompt_title = None;
        self.skip_replay = has_resume_id;
        let reconnect_provider = self.current_model_provider_for_persistence();
        let reconnect_model = super::config::provider_qualified_model_value(
            &self.ui.session.model,
            reconnect_provider.as_deref(),
        );
        self.pending_model_restore = Some(ModelSelection::new(reconnect_model, reconnect_provider));
        self.authoritative_model_selection = None;
        self.bump_revision();
        Ok(())
    }

    pub(super) fn resume_acp_session_id_for_stored_session(&self, id: &str) -> Option<String> {
        if self.store.session_has_activity(id).unwrap_or(false) {
            self.store.get_acp_session_id(id).unwrap_or(None)
        } else {
            let _ = self.store.clear_acp_session_id(id);
            None
        }
    }

    fn runtime_for_stored_session(&mut self, id: &str) -> Result<SessionRuntime, String> {
        self.runtime_for_stored_session_with_override(id, RuntimeResumeOverride::default())
    }

    /// Stored-session runtime install with resume overrides — see
    /// [`RuntimeResumeOverride`]. Used by `session_fork` to resume the freshly
    /// forked backend session and let its replay rebuild the empty branch row.
    fn runtime_for_stored_session_with_override(
        &mut self,
        id: &str,
        resume_override: RuntimeResumeOverride,
    ) -> Result<SessionRuntime, String> {
        // Windowed history load: only the most recent entries are
        // materialized; older history pages in on demand. Keeps long sessions
        // from loading their entire message/tool history into memory.
        let history_window = self
            .store
            .load_session_window_by_turns(
                id,
                SESSION_HISTORY_WINDOW,
                SESSION_HISTORY_WINDOW_MIN_TURNS,
            )
            .map_err(|e| e.to_string())?;
        let history_total_count = history_window.total_count;
        let history_earliest_seq = history_window.earliest_seq;
        let (messages, mut tools, timeline) = (
            history_window.messages,
            history_window.tools,
            history_window.timeline,
        );
        let interrupted_tool_ids = interrupt_incomplete_tools(&mut tools);
        for tool_id in &interrupted_tool_ids {
            if let Some(tool) = tools.iter().find(|tool| tool.id.to_string() == *tool_id) {
                let _ = self.store.update_tool(
                    tool_id,
                    "Interrupted",
                    tool.raw_output.as_deref(),
                    tool.error.as_deref(),
                );
            }
        }

        let (model, model_provider, mode) = self
            .store
            .get_session_model_provider_mode(id)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| {
                (
                    self.ui.session.model.clone(),
                    self.current_model_provider_for_persistence(),
                    self.ui.session.mode.clone(),
                )
            });
        let mode = mode.or_else(|| Some("Build".into()));
        let stored_agent_cli = self.store.get_session_agent_cli(id).unwrap_or(None);
        let current_agent_label = self.ui.session.agent_cli.as_deref();
        let session_agent_command = if stored_agent_cli.as_deref() == current_agent_label {
            self.agent_command.clone()
        } else if let Some(label) = stored_agent_cli.as_deref() {
            self.prepare_agent_command_for_stored_label(label)?
                .unwrap_or_else(|| self.agent_command.clone())
        } else {
            self.agent_command.clone()
        };
        // A fork child row is empty by design — the fork's backend session id
        // is authoritative even without local activity, so it bypasses the
        // activity-gated lookup (which would clear it and drop the context).
        let resume_acp_id = match &resume_override.force_agent_session_id {
            Some(forced) => Some(forced.clone()),
            None => self.resume_acp_session_id_for_stored_session(id),
        };
        let has_resume_id = resume_acp_id.is_some();
        // The replay is skipped only when the transcript is already restored
        // from SQLite. The fork child's row is empty — its replay IS the
        // transcript — so force_replay keeps skip_replay off and lets the
        // replayed frames rebuild (and persist) the branch history.
        let skip_replay = has_resume_id && !resume_override.force_replay;
        // Restore the session's own persisted preset so a switch/resume does
        // not fall back to the global default (which can conflict with the
        // session's fixed preset and cause dsh to reject the resume).
        let stored_preset = self.store.get_session_agent_preset(id).ok().flatten();
        // Resume in the session's OWN workspace, not whatever workspace the UI
        // has open right now: a dsh resume whose cwd differs from the
        // persisted session's fails with `session-conflict` (the remote-control
        // switch path once hit exactly this when the desktop had another
        // workspace active).
        let stored_workspace_root = self
            .store
            .get_session_workspace_root(id)
            .ok()
            .flatten()
            .filter(|root| !root.is_empty());
        let prepared_runtime = self.prepare_session_runtime_for_resume(
            &session_agent_command,
            &model,
            stored_preset,
            has_resume_id,
            stored_workspace_root,
        )?;
        let agent_cli_label =
            active_agent_label_for_command(&session_agent_command, stored_agent_cli);
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: model.clone(),
            agent_command: session_agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: resume_acp_id,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
            mcp_servers: prepared_runtime.mcp_servers.clone(),
            harness_endpoint: prepared_runtime.harness_endpoint.clone(),
            agent_preset: prepared_runtime.agent_preset.clone(),
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode(mode.as_deref().unwrap_or("Build"));
        let _ = super::config::queue_codex_agent_mode_for_policy_mode(
            &mut session,
            is_codex_agent_label(&agent_cli_label),
            mode.as_deref(),
        );

        let mut ui = self.ui.clone();
        let pending_model_restore =
            Some(ModelSelection::new(model.clone(), model_provider.clone()));
        ui.session.id = uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        // `model` is stored provider-qualified; show the bare label in the UI
        // while keeping the qualified value in `pending_model_restore` for
        // provider-aware restore.
        ui.session.model = super::config::display_model_from_persisted(&model);
        // For dsh sessions the mode slot carries the agent preset (not the
        // ACP Plan/Build permission mode), so restore the persisted preset
        // over the generic mode value.
        ui.session.mode = if crate::settings::is_deepseek_harness_command(&session_agent_command) {
            self.store
                .get_session_agent_preset(id)
                .ok()
                .flatten()
                .or(mode)
        } else {
            mode
        };
        ui.session.agent_cli = Some(agent_cli_label);
        ui.session_config = Default::default();
        ui.prompt_capabilities = Default::default();
        ui.image_capabilities = prepared_runtime.image_capabilities;
        ui.available_commands.clear();
        ui.agent_plan.clear();
        ui.messages = messages;
        ui.tools = tools;
        ui.timeline = timeline;
        ui.session.status = SessionStatus::Idle;
        ui.session_changes.clear();
        ui.review_changes.clear();
        ui.turn_changes.clear();
        // Repair change sets whose turn finalize never ran (app closed before
        // the turn finished): the review panel cannot select a Pending set
        // without a message id, so it would otherwise stay invisible forever.
        let _ = self.store.repair_pending_agent_turn_change_sets();
        // Transient per-turn state must NOT be inherited from the currently
        // visible session: `thinking_status`/`thinking_text` are live-run-only
        // fields (never persisted, never reconstructed on resume), and
        // `pending_steers` belong to the originating session's active turn.
        // Without clearing, switching to a stored session while another session
        // is mid-think leaks that session's thinking indicator and text into
        // the freshly loaded (idle) session's UI.
        ui.thinking_status = None;
        ui.thinking_text.clear();
        ui.pending_steers.clear();
        ui.usage = self
            .store
            .load_session_usage_snapshot(id)
            .unwrap_or_default();

        let sessions = self.store.list_sessions().unwrap_or_default();
        if let Some(s) = sessions.iter().find(|s| s.id == id) {
            ui.session.title = s.title.clone();
        }
        if let Some(agent_label) = ui.session.agent_cli.clone() {
            update_initial_agent_notice(&mut ui, &agent_label);
            if is_codex_agent_label(&agent_label) {
                let provider = crate::settings::codex_current_provider(&self.app_paths);
                let _ = self.store.update_session_codex_provider(id, &provider);
            }
        }
        // Re-qualify so the persisted `model` column keeps the provider
        // embedded (ui.session.model holds the bare display label). When the
        // separate `model_provider` column is NULL (e.g. pre-migration
        // sessions), recover the provider from the qualified `model` value so
        // re-qualification does not downgrade `kodex-provider/<p>/<m>` to a
        // bare model name and lose the provider across session reopens.
        let (persisted_model, effective_provider) = super::config::requalify_persisted_model(
            &ui.session.model,
            &model,
            model_provider.as_deref(),
        );
        let _ = self.store.update_session_model_mode_provider(
            id,
            &persisted_model,
            effective_provider.as_deref(),
            ui.session.mode.as_deref(),
        );

        let seq_counter = self.store.next_seq(id).unwrap_or(1);
        let needs_title = is_placeholder_session_title(&ui.session.title);
        Ok(SessionRuntime {
            local_session_id: ui.session.id,
            ui,
            session,
            agent_command: session_agent_command,
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh,
            web_tools_mcp: prepared_runtime.web_tools_mcp,
            image_mcp: prepared_runtime.image_mcp,
            in_flight_prompt: None,
            seq_counter,
            needs_title,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay,
            pending_model_restore,
            authoritative_model_selection: None,
            file_tracker: FileChangeTracker::new(&self.ui.workspace.root),
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            last_viewed: self.runtime_now(),
            idle_since: None,
            runtime_status: SessionRuntimeStatus::Active,
            attention_state: SessionAttentionState::None,
            pending_image_degradation: None,
            history_total_count,
            history_earliest_seq,
            conversation_change_set_signature: 0,
            conversation_change_set_turn_cache: HashMap::new(),
        })
    }

    fn runtime_for_new_session(
        &mut self,
        agent: Option<AgentCliId>,
        preset: Option<String>,
    ) -> Result<SessionRuntime, String> {
        let new_id = uuid::Uuid::new_v4();
        let initial_model = if crate::settings::is_deepseek_harness_command(&self.agent_command) {
            String::new()
        } else {
            AGENT_DEFAULT_MODEL_LABEL.to_string()
        };
        self.store
            .create_session(&new_id.to_string(), &initial_model)
            .map_err(|e| e.to_string())?;

        let agent_command = self.prepare_agent_command_for_new_session(agent)?;
        let prepared_runtime = self.prepare_session_runtime(&agent_command, &initial_model, preset)?;

        let agent_cli_label = crate::settings::agent_label_for_command(&agent_command);
        let mut session = SessionHandle::start(SessionConfig {
            workspace_root: prepared_runtime.workspace_root,
            app_data_root: self.app_paths.root().display().to_string(),
            model: initial_model.clone(),
            agent_command: agent_command.clone(),
            agent_env: prepared_runtime.agent_env,
            resume_session_id: None,
            log_id: make_log_id(),
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh.clone(),
            mcp_servers: prepared_runtime.mcp_servers.clone(),
            harness_endpoint: prepared_runtime.harness_endpoint.clone(),
            agent_preset: prepared_runtime.agent_preset.clone(),
        })
        .map_err(|e| e.to_string())?;
        let _ = session.set_permission_mode("Build");
        let _ = super::config::queue_codex_agent_mode_for_policy_mode(
            &mut session,
            is_codex_agent_label(&agent_cli_label),
            Some("Build"),
        );

        let mut ui = self.ui.clone();
        ui.session.id = new_id;
        ui.session.title = "新会话".to_string();
        ui.session.model = initial_model;
        ui.session.mode = Some("Build".into());
        ui.session.agent_cli = Some(agent_cli_label.clone());
        if let Some(agent) = agent {
            if let workspace_model::WorkspaceLocation::RemoteLinux(remote) =
                &mut ui.workspace.location
            {
                remote.agent_cli = Some(agent);
                remote.agent_command = Some(agent_command.clone());
            }
        }
        ui.session_config = Default::default();
        ui.prompt_capabilities = Default::default();
        ui.image_capabilities = prepared_runtime.image_capabilities;
        ui.session.status = SessionStatus::Idle;
        ui.available_commands.clear();
        ui.agent_plan.clear();
        ui.messages.clear();
        ui.tools.clear();
        ui.timeline.clear();
        ui.session_changes.clear();
        ui.review_changes.clear();
        ui.turn_changes.clear();
        // Transient per-turn state must NOT be inherited from the currently
        // visible session (same rationale as `runtime_for_stored_session`).
        ui.thinking_status = None;
        ui.thinking_text.clear();
        ui.pending_steers.clear();
        ui.usage = Default::default();

        let _ = self.store.update_session_model_mode(
            &new_id.to_string(),
            &ui.session.model,
            ui.session.mode.as_deref(),
        );
        let _ = self
            .store
            .update_session_agent_cli(&new_id.to_string(), &agent_cli_label);
        if is_codex_agent_label(&agent_cli_label) {
            let provider = crate::settings::codex_current_provider(&self.app_paths);
            let _ = self
                .store
                .update_session_codex_provider(&new_id.to_string(), &provider);
        }

        Ok(SessionRuntime {
            local_session_id: new_id,
            ui,
            session,
            agent_command,
            acp_port: prepared_runtime.acp_port,
            remote_ssh: prepared_runtime.remote_ssh,
            web_tools_mcp: prepared_runtime.web_tools_mcp,
            image_mcp: prepared_runtime.image_mcp,
            in_flight_prompt: None,
            seq_counter: 1,
            needs_title: true,
            agent_title_received: false,
            provisional_prompt_title: None,
            skip_replay: false,
            pending_model_restore: None,
            authoritative_model_selection: None,
            file_tracker: FileChangeTracker::new(&self.ui.workspace.root),
            dirty_tool_call_ids: HashSet::new(),
            review_changes_started: false,
            current_turn_user_message_id: None,
            pending_tool_write_detections: Vec::new(),
            inline_think_filter: InlineThinkFilter::default(),
            last_viewed: self.runtime_now(),
            idle_since: None,
            runtime_status: SessionRuntimeStatus::Active,
            attention_state: SessionAttentionState::None,
            pending_image_degradation: None,
            history_total_count: 0,
            history_earliest_seq: None,
            conversation_change_set_signature: 0,
            conversation_change_set_turn_cache: HashMap::new(),
        })
    }
}
