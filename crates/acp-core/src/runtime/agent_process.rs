use super::ShutdownSignal;
use super::process::{
    agent_spawn_command, apply_process_cwd_and_pwd, hide_console_window, parse_env_assignment,
};
use crate::codex_api_proxy::{
    configure_codex_api_proxy_model_provider_map, ensure_codex_api_proxy,
};
use crate::events::{RemoteSshReverseForward, SessionConfig};
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::{Client, ConnectTo, Lines, Role};
use anyhow::anyhow;
use futures::{StreamExt, channel::mpsc as futures_mpsc};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use serde_json::json;
#[cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const REMOTE_AGENT_READY_MARKER: &str = "__KODEX_ACP_REMOTE_AGENT_READY__";
const REMOTE_STREAMABLE_HTTP_ENDPOINT_MARKER: &str = "ACP streamable-http endpoint:";
const REMOTE_AGENT_LISTEN_ATTEMPTS: u16 = 150;
const REMOTE_AGENT_READY_TIMEOUT: Duration = Duration::from_secs(35);
const REMOTE_SSH_CONNECT_TIMEOUT_SECS: u64 = 5;
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";
const STREAMABLE_HTTP_PATH: &str = "/api/v1/acp";
const CODEBUDDY_RESOLVE_INTERRUPTION_METHOD: &str = "_codebuddy.ai/resolveInterruption";
const CODEBUDDY_REQUEST_HEADER: &str = "X-CodeBuddy-Request";
const CODEBUDDY_ACP_CONNECTION_ID_HEADER: &str = "acp-connection-id";
const ACP_CONNECTION_ID_HEADER: &str = "Acp-Connection-Id";
const ACP_SESSION_ID_HEADER: &str = "Acp-Session-Id";
const ACP_SESSION_TOKEN_HEADER: &str = "acp-session-token";
const CODEBUDDY_ACP_CONNECT_ATTEMPTS: u16 = 80;

pub(super) struct HiddenAgentProcess {
    pub(super) command: PathBuf,
    pub(super) args: Vec<String>,
    pub(super) env: Vec<(String, String)>,
    pub(super) current_dir: PathBuf,
    log_config: Option<SessionConfig>,
    shutdown_signal: ShutdownSignal,
}

impl HiddenAgentProcess {
    pub(super) fn from_config(config: &SessionConfig) -> anyhow::Result<Self> {
        let mut process = Self::from_command(&config.agent_command, &config.workspace_root)?;
        process.env.extend(config.agent_env.clone());
        let agent_command = config.agent_command.to_ascii_lowercase();
        if uses_codex_api_proxy(&agent_command) {
            register_codex_api_proxy_from_env(&process.env);
        }
        process.log_config = Some(config.clone());
        Ok(process)
    }

    pub(super) fn from_command(
        command: &str,
        current_dir: impl Into<PathBuf>,
    ) -> anyhow::Result<Self> {
        let args = shell_words::split(command).map_err(|err| anyhow!(err.to_string()))?;
        if args.is_empty() {
            return Err(anyhow!("agent command cannot be empty"));
        }

        let mut env = Vec::new();
        let mut command_index = 0;
        for (index, arg) in args.iter().enumerate() {
            if let Some((name, value)) = parse_env_assignment(arg) {
                env.push((name, value));
                command_index = index + 1;
            } else {
                break;
            }
        }

        let Some(command) = args.get(command_index) else {
            return Err(anyhow!("agent command missing executable"));
        };

        Ok(Self {
            command: PathBuf::from(command),
            args: args[command_index + 1..].to_vec(),
            env,
            current_dir: current_dir.into(),
            log_config: None,
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    pub(super) fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
}

impl ConnectTo<Client> for HiddenAgentProcess {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        let mut command = agent_spawn_command(&self.command, &self.args);
        apply_process_cwd_and_pwd(&mut command, &self.current_dir);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command.env("PWD", self.current_dir.as_os_str());
        // Ensure PATH includes common directories for CLI tools (e.g. node, python)
        // GUI apps on macOS may not inherit the shell's PATH.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = std::env::split_paths(&path).collect::<Vec<_>>();
        for extra in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        let new_path = std::env::join_paths(paths).unwrap_or_default();
        command.env("PATH", new_path);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        hide_console_window(&mut command);

        let mut child = command
            .spawn()
            .map_err(agent_client_protocol::Error::into_internal_error)?;

        let child_stdin = child.stdin.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdin")
        })?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdout")
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stderr")
        })?;

        let (stdout_tx, incoming_lines) = futures_mpsc::unbounded::<std::io::Result<String>>();
        thread::spawn(move || read_agent_stdout(child_stdout, stdout_tx));

        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || read_agent_stderr(child_stderr, stderr_tx));

        let (stdin_tx, stdin_rx) = mpsc::channel::<String>();

        // Some ACP agents shut down their ACP handler when
        // stdin reaches EOF.  We must keep the stdin handle alive for the entire
        // lifetime of the child process — not just until the write channel closes
        // or connect_to returns.  Spawn a dedicated thread that holds the handle
        // and only drops it when the child exits (detected via child_monitor).
        let shared_stdin: Arc<Mutex<Option<std::process::ChildStdin>>> =
            Arc::new(Mutex::new(Some(child_stdin)));
        let keepalive_stdin = shared_stdin.clone();
        // Use a channel so we can signal the keepalive thread to exit.
        let (stdin_keepalive_tx, stdin_keepalive_rx) = mpsc::channel::<()>();
        thread::spawn(move || {
            // Block until signaled or the sender is dropped (process exit).
            let _ = stdin_keepalive_rx.recv();
            if let Ok(mut guard) = keepalive_stdin.lock() {
                guard.take();
            }
        });
        thread::spawn(move || write_agent_stdin(shared_stdin, stdin_rx));

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor = monitor_hidden_agent_child(
            child_guard.handle(),
            stderr_rx,
            log_config.clone(),
            "stdio-agent",
        );
        let outgoing_sink = futures::sink::unfold(stdin_tx, |tx, line: String| async move {
            tx.send(line).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent stdin closed")
            })?;
            Ok::<_, std::io::Error>(tx)
        });

        let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
            Lines::new(outgoing_sink, incoming_lines),
            client,
        );

        // Hold stdin_keepalive_tx for the entire duration of the select! so the
        // keepalive thread doesn't drop the stdin handle prematurely.
        let _keepalive = stdin_keepalive_tx;
        let _child_guard = child_guard;

        tokio::pin!(protocol);
        tokio::pin!(child_monitor);
        tokio::select! {
            result = &mut protocol => result,
            result = &mut child_monitor => match result {
                Ok(()) => {
                    if let Some(config) = log_config.as_ref() {
                        let _ = append_runtime_event_log(
                            config,
                            "agent/clean_exit_waiting_for_protocol",
                            &json!({ "reason": "agent process exited before protocol completed" }),
                        );
                    }
                    (&mut protocol).await
                }
                Err(error) => Err(error),
            },
        }
    }
}

fn uses_codex_api_proxy(agent_command: &str) -> bool {
    agent_command.contains("codex-acp")
        || agent_command.contains("kodex-acp")
        || agent_command.contains("claude-agent-acp")
}

fn register_codex_api_proxy_from_env(env: &[(String, String)]) {
    for (env_key, provider) in [
        ("TIMIAI_API_KEY", "timiai"),
        ("COMMANDCODE_API_KEY", "commandcode"),
        ("DEEPSEEK_API_KEY", "deepseek"),
        ("KIMI_CODE_API_KEY", "kimi_code"),
        ("XIAOMI_MIMO_API_KEY", "xiaomi_mimo"),
    ] {
        if let Some((_, api_key)) = env.iter().find(|(name, _)| name == env_key) {
            ensure_codex_api_proxy(provider, api_key);
        }
    }
    if let Some((_, provider_map)) = env
        .iter()
        .find(|(name, _)| name == "KODEX_MODEL_PROVIDER_MAP")
    {
        configure_codex_api_proxy_model_provider_map(provider_map);
    }
}

/// ACP transport that spawns the agent process and connects via TCP.
///
/// Used for agents that listen on a TCP port instead of communicating over
/// stdio. The agent is spawned with `--port <port>` and the client connects to
/// `127.0.0.1:<port>`.
pub(super) struct TcpAgentProcess {
    command: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    current_dir: PathBuf,
    port: u16,
    log_config: SessionConfig,
    shutdown_signal: ShutdownSignal,
}

impl TcpAgentProcess {
    pub(super) fn from_config(config: &SessionConfig) -> anyhow::Result<Self> {
        let parsed =
            HiddenAgentProcess::from_command(&config.agent_command, &config.workspace_root)?;
        let mut env = parsed.env;
        env.extend(config.agent_env.clone());
        let agent_command = config.agent_command.to_ascii_lowercase();
        if uses_codex_api_proxy(&agent_command) {
            register_codex_api_proxy_from_env(&env);
        }
        Ok(Self {
            command: parsed.command,
            args: parsed.args,
            env,
            current_dir: parsed.current_dir,
            port: config.acp_port,
            log_config: config.clone(),
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    pub(super) fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
}

pub(super) struct RemoteSshAgentProcess {
    ssh_command: PathBuf,
    ssh_target: String,
    ssh_port: Option<u16>,
    ssh_password: Option<String>,
    remote_workspace_root: String,
    agent_command: PathBuf,
    agent_args: Vec<String>,
    agent_env: Vec<(String, String)>,
    local_port: u16,
    remote_port: u16,
    reverse_forwards: Vec<RemoteSshReverseForward>,
    acp_transport: RemoteAcpTransport,
    log_config: SessionConfig,
    shutdown_signal: ShutdownSignal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RemoteAcpTransport {
    Tcp,
    AcpStreamableHttp,
    CodeBuddyServeHttp,
}

#[derive(Clone, Debug, Default)]
struct RemoteAgentReady {
    endpoint_port: Option<u16>,
}

impl RemoteSshAgentProcess {
    pub(super) fn from_config(config: &SessionConfig) -> anyhow::Result<Self> {
        let remote = config
            .remote_ssh
            .clone()
            .ok_or_else(|| anyhow!("remote SSH config is required"))?;
        let parsed = HiddenAgentProcess::from_command(
            &config.agent_command,
            PathBuf::from(&remote.remote_workspace_root),
        )?;
        let mut agent_env = parsed.env;
        agent_env.extend(config.agent_env.clone());
        Ok(Self {
            ssh_command: remote
                .ssh_command
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ssh")),
            ssh_target: remote.ssh_target,
            ssh_port: remote.ssh_port,
            ssh_password: remote.ssh_password.filter(|password| !password.is_empty()),
            remote_workspace_root: remote.remote_workspace_root,
            agent_command: parsed.command,
            acp_transport: detect_remote_acp_transport(&parsed.args),
            agent_args: parsed.args,
            agent_env,
            local_port: remote.local_port,
            remote_port: remote.remote_port,
            reverse_forwards: remote.reverse_forwards,
            log_config: config.clone(),
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    pub(super) fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
}

fn detect_remote_acp_transport(args: &[String]) -> RemoteAcpTransport {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--acp-transport=") {
            if value.eq_ignore_ascii_case("streamable-http") {
                return RemoteAcpTransport::AcpStreamableHttp;
            }
        }
        if arg == "--acp-transport"
            && iter
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case("streamable-http"))
        {
            return RemoteAcpTransport::AcpStreamableHttp;
        }
        if arg == "--serve" {
            return RemoteAcpTransport::CodeBuddyServeHttp;
        }
    }
    RemoteAcpTransport::Tcp
}

/// Wrapper that dispatches to either stdio or TCP transport.
pub(super) enum AgentTransport {
    Stdio(HiddenAgentProcess),
    Tcp(TcpAgentProcess),
    RemoteSsh(RemoteSshAgentProcess),
}

impl ConnectTo<Client> for AgentTransport {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        match self {
            AgentTransport::Stdio(agent) => agent.connect_to(client).await,
            AgentTransport::Tcp(agent) => agent.connect_to(client).await,
            AgentTransport::RemoteSsh(agent) => agent.connect_to(client).await,
        }
    }
}

impl ConnectTo<Client> for TcpAgentProcess {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        // Build the command, appending --port <port>
        let mut command = agent_spawn_command(&self.command, &self.args);
        command.args(["--port", &self.port.to_string()]);
        apply_process_cwd_and_pwd(&mut command, &self.current_dir);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command.env("PWD", self.current_dir.as_os_str());
        // Ensure PATH includes common directories
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = std::env::split_paths(&path).collect::<Vec<_>>();
        for extra in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        let new_path = std::env::join_paths(paths).unwrap_or_default();
        command.env("PATH", new_path);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        hide_console_window(&mut command);

        let mut child = command
            .spawn()
            .map_err(agent_client_protocol::Error::into_internal_error)?;

        // Keep stdin handle alive for the entire session. Some TCP ACP agents exit
        // when stdin reaches EOF, regardless of whether it uses stdio or TCP.
        let _child_stdin = child.stdin.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stdin")
        })?;

        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open agent stderr")
        })?;

        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || read_agent_stderr(child_stderr, stderr_tx));

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor = monitor_hidden_agent_child(
            child_guard.handle(),
            stderr_rx,
            Some(log_config.clone()),
            "tcp-agent",
        );

        let stream = connect_loopback_tcp(self.port, "agent", Some(&log_config)).await?;
        let protocol = connect_tcp_stream(stream, client)?;

        let _child_guard = child_guard;

        tokio::pin!(protocol);
        tokio::pin!(child_monitor);
        tokio::select! {
            result = &mut protocol => result,
            result = &mut child_monitor => match result {
                Ok(()) => {
                    let _ = append_runtime_event_log(
                        &log_config,
                        "agent/clean_exit_waiting_for_protocol",
                        &json!({ "reason": "agent process exited before protocol completed" }),
                    );
                    (&mut protocol).await
                }
                Err(error) => Err(error),
            },
        }
    }
}

impl ConnectTo<Client> for RemoteSshAgentProcess {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        if self.ssh_target.trim().is_empty() {
            return Err(agent_client_protocol::util::internal_error(
                "remote SSH target cannot be empty",
            ));
        }
        if !self.remote_workspace_root.starts_with('/') {
            return Err(agent_client_protocol::util::internal_error(
                "remote workspace path must be absolute",
            ));
        }

        let reverse_forward_guards = self
            .spawn_reverse_forwards(Some(&self.log_config))
            .map_err(|error| {
                agent_client_protocol::util::internal_error(format!(
                    "failed to establish remote reverse forward: {error}"
                ))
            })?;
        let _reverse_forward_guards = reverse_forward_guards;

        let is_streamable_http = self.acp_transport != RemoteAcpTransport::Tcp;
        let remote_command = if is_streamable_http {
            build_remote_streamable_agent_command(
                &self.remote_workspace_root,
                &self.agent_command,
                &self.agent_args,
                &self.agent_env,
                self.remote_port,
            )
        } else {
            build_remote_agent_command(
                &self.remote_workspace_root,
                &self.agent_command,
                &self.agent_args,
                &self.agent_env,
                self.remote_port,
            )
        };
        let has_password = self.ssh_password.is_some();
        let args = if is_streamable_http {
            build_remote_ssh_command_args(
                &self.ssh_target,
                self.ssh_port,
                remote_command,
                has_password,
            )
        } else {
            build_remote_ssh_args(
                &self.ssh_target,
                self.ssh_port,
                self.local_port,
                self.remote_port,
                remote_command,
                has_password,
            )
        };
        let mut command = agent_spawn_command(&self.ssh_command, &args);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        if let Some(password) = self.ssh_password.as_deref() {
            configure_ssh_askpass(&mut command, password).map_err(|error| {
                agent_client_protocol::util::internal_error(format!(
                    "failed to configure SSH password prompt: {error}"
                ))
            })?;
        }
        hide_console_window(&mut command);

        let mut child = command
            .spawn()
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::util::internal_error("failed to open ssh stderr")
        })?;

        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<RemoteAgentReady>();
        let live_stderr = Arc::new(Mutex::new(String::new()));
        let reader_live_stderr = live_stderr.clone();
        let acp_transport = self.acp_transport;
        thread::spawn(move || {
            read_remote_ssh_stderr(
                child_stderr,
                stderr_tx,
                reader_live_stderr,
                ready_tx,
                acp_transport,
            )
        });

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor = monitor_hidden_agent_child(
            child_guard.handle(),
            stderr_rx,
            Some(log_config.clone()),
            "remote-ssh-agent",
        );

        let _child_guard = child_guard;

        let ready = wait_remote_agent_ready(ready_rx, live_stderr, Some(&log_config));
        tokio::pin!(ready);
        tokio::pin!(child_monitor);
        let ready = tokio::select! {
            result = &mut ready => result?,
            result = &mut child_monitor => {
                return match result {
                    Ok(()) => Err(agent_client_protocol::util::internal_error(
                        "ssh remote agent process exited before ACP TCP became reachable",
                    )),
                    Err(error) => Err(error),
                };
            }
        };

        match self.acp_transport {
            RemoteAcpTransport::Tcp => {
                let stream = connect_loopback_tcp(
                    self.local_port,
                    "remote SSH ACP forward",
                    Some(&log_config),
                )
                .await?;
                let protocol = connect_tcp_stream(stream, client)?;
                tokio::pin!(protocol);
                tokio::select! {
                    result = &mut protocol => result,
                    result = &mut child_monitor => match result {
                        Ok(()) => {
                            let _ = append_runtime_event_log(
                                &log_config,
                                "agent/remote_ssh_exit_waiting_for_protocol",
                                &json!({ "reason": "ssh remote agent process exited before protocol completed" }),
                            );
                            (&mut protocol).await
                        }
                        Err(error) => Err(error),
                    },
                }
            }
            RemoteAcpTransport::AcpStreamableHttp | RemoteAcpTransport::CodeBuddyServeHttp => {
                let remote_port = ready.endpoint_port.unwrap_or(self.remote_port);
                let forward_args = build_remote_ssh_forward_args(
                    &self.ssh_target,
                    self.ssh_port,
                    self.local_port,
                    remote_port,
                    has_password,
                );
                let mut forward_command = agent_spawn_command(&self.ssh_command, &forward_args);
                forward_command
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped());
                if let Some(password) = self.ssh_password.as_deref() {
                    configure_ssh_askpass(&mut forward_command, password).map_err(|error| {
                        agent_client_protocol::util::internal_error(format!(
                            "failed to configure SSH password prompt: {error}"
                        ))
                    })?;
                }
                hide_console_window(&mut forward_command);
                let mut forward_child = forward_command
                    .spawn()
                    .map_err(agent_client_protocol::Error::into_internal_error)?;
                let forward_stderr = forward_child.stderr.take().ok_or_else(|| {
                    agent_client_protocol::util::internal_error("failed to open ssh forward stderr")
                })?;
                let (forward_stderr_tx, forward_stderr_rx) = std::sync::mpsc::channel::<String>();
                thread::spawn(move || read_agent_stderr(forward_stderr, forward_stderr_tx));
                let forward_guard =
                    AgentChildGuard::new(forward_child, self.shutdown_signal.clone());
                let forward_monitor = monitor_hidden_agent_child(
                    forward_guard.handle(),
                    forward_stderr_rx,
                    Some(log_config.clone()),
                    "remote-ssh-forward",
                );
                let _forward_guard = forward_guard;
                let stream = connect_loopback_tcp(
                    self.local_port,
                    "remote SSH ACP streamable-http forward",
                    Some(&log_config),
                )
                .await?;
                drop(stream);
                let endpoint = format!(
                    "http://127.0.0.1:{}{}",
                    self.local_port, STREAMABLE_HTTP_PATH
                );
                let protocol = connect_streamable_http_endpoint(
                    endpoint,
                    client,
                    self.acp_transport,
                    Some(log_config.clone()),
                )?;
                tokio::pin!(protocol);
                tokio::pin!(forward_monitor);
                tokio::select! {
                    result = &mut protocol => result,
                    result = &mut child_monitor => match result {
                        Ok(()) => {
                            let _ = append_runtime_event_log(
                                &log_config,
                                "agent/remote_ssh_exit_waiting_for_protocol",
                                &json!({ "reason": "ssh remote agent process exited before protocol completed" }),
                            );
                            (&mut protocol).await
                        }
                        Err(error) => Err(error),
                    },
                    result = &mut forward_monitor => match result {
                        Ok(()) => {
                            let _ = append_runtime_event_log(
                                &log_config,
                                "agent/remote_ssh_forward_exit_waiting_for_protocol",
                                &json!({ "reason": "ssh port forward exited before protocol completed" }),
                            );
                            (&mut protocol).await
                        }
                        Err(error) => Err(error),
                    },
                }
            }
        }
    }
}

impl RemoteSshAgentProcess {
    fn spawn_reverse_forwards(
        &self,
        log_config: Option<&SessionConfig>,
    ) -> anyhow::Result<Vec<AgentChildGuard>> {
        let mut guards = Vec::new();
        for forward in &self.reverse_forwards {
            let guard = self.spawn_reverse_forward(forward, log_config)?;
            guards.push(guard);
        }
        Ok(guards)
    }

    fn spawn_reverse_forward(
        &self,
        forward: &RemoteSshReverseForward,
        log_config: Option<&SessionConfig>,
    ) -> anyhow::Result<AgentChildGuard> {
        let has_password = self.ssh_password.is_some();
        let args = build_remote_ssh_reverse_forward_args(
            &self.ssh_target,
            self.ssh_port,
            forward.remote_port,
            forward.local_port,
            has_password,
        );
        let mut command = agent_spawn_command(&self.ssh_command, &args);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        if let Some(password) = self.ssh_password.as_deref() {
            configure_ssh_askpass(&mut command, password)?;
        }
        hide_console_window(&mut command);

        let mut child = command.spawn()?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to open ssh reverse forward stderr"))?;
        let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || read_agent_stderr(child_stderr, stderr_tx));

        thread::sleep(Duration::from_millis(250));
        if let Some(status) = child.try_wait()? {
            let stderr = stderr_rx
                .recv_timeout(Duration::from_millis(200))
                .unwrap_or_default();
            if let Some(config) = log_config {
                let _ = append_runtime_event_log(
                    config,
                    "agent/reverse_forward_exit",
                    &json!({
                        "remotePort": forward.remote_port,
                        "localPort": forward.local_port,
                        "status": status.to_string(),
                        "stderr": stderr,
                    }),
                );
            }
            anyhow::bail!(
                "ssh reverse forward 127.0.0.1:{} -> 127.0.0.1:{} exited with {}",
                forward.remote_port,
                forward.local_port,
                status
            );
        }

        if let Some(config) = log_config {
            let _ = append_runtime_event_log(
                config,
                "agent/reverse_forward_started",
                &json!({
                    "remotePort": forward.remote_port,
                    "localPort": forward.local_port,
                }),
            );
        }

        Ok(AgentChildGuard::new(child, self.shutdown_signal.clone()))
    }
}

pub(super) fn build_remote_agent_command(
    remote_workspace_root: &str,
    command: &Path,
    args: &[String],
    env: &[(String, String)],
    remote_port: u16,
) -> String {
    let port_hex = format!("{remote_port:04X}");
    let agent_invocation = build_remote_agent_invocation(command, args, env, remote_port);
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote_single(remote_workspace_root));
    parts.push("|| exit $?;".to_string());
    parts.push(format!(
        "KODEX_REMOTE_ACP_PORT_HEX={};",
        shell_quote_single(&port_hex)
    ));
    parts.push(format!("{agent_invocation} &"));
    parts.push("kodex_agent_pid=$!;".to_string());
    parts.push("trap 'kill \"$kodex_agent_pid\" 2>/dev/null' EXIT HUP INT TERM;".to_string());
    parts.push("kodex_i=0;".to_string());
    parts.push("while ! awk -v port=\"$KODEX_REMOTE_ACP_PORT_HEX\" '$4==\"0A\" && toupper($2) ~ \":\" port \"$\" { found=1 } END { exit found ? 0 : 1 }' /proc/net/tcp /proc/net/tcp6 2>/dev/null; do".to_string());
    parts.push("if ! kill -0 \"$kodex_agent_pid\" 2>/dev/null; then wait \"$kodex_agent_pid\"; exit $?; fi;".to_string());
    parts.push("kodex_i=$((kodex_i + 1));".to_string());
    parts.push(format!(
        "if [ \"$kodex_i\" -ge {REMOTE_AGENT_LISTEN_ATTEMPTS} ]; then echo {} >&2; kill \"$kodex_agent_pid\" 2>/dev/null; wait \"$kodex_agent_pid\"; exit 124; fi;",
        shell_quote_single(&format!(
            "remote ACP agent did not listen on 127.0.0.1:{remote_port}"
        ))
    ));
    parts.push("sleep 0.2;".to_string());
    parts.push("done".to_string());
    parts.push(";".to_string());
    parts.push(format!(
        "echo {} >&2;",
        shell_quote_single(REMOTE_AGENT_READY_MARKER)
    ));
    parts.push("wait \"$kodex_agent_pid\"".to_string());
    parts.join(" ")
}

pub(super) fn build_remote_streamable_agent_command(
    remote_workspace_root: &str,
    command: &Path,
    args: &[String],
    env: &[(String, String)],
    remote_port: u16,
) -> String {
    let agent_invocation = build_remote_agent_invocation(command, args, env, remote_port);
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote_single(remote_workspace_root));
    parts.push("|| exit $?;".to_string());
    parts.push(format!("{agent_invocation} &"));
    parts.push("kodex_agent_pid=$!;".to_string());
    parts.push("trap 'kill \"$kodex_agent_pid\" 2>/dev/null' EXIT HUP INT TERM;".to_string());
    parts.push("wait \"$kodex_agent_pid\"".to_string());
    parts.join(" ")
}

fn build_remote_agent_invocation(
    command: &Path,
    args: &[String],
    env: &[(String, String)],
    remote_port: u16,
) -> String {
    let mut parts = Vec::new();
    for (name, value) in env {
        parts.push(format!("{name}={}", shell_quote_single(value)));
    }
    parts.push(shell_quote_single(&command.to_string_lossy()));
    parts.extend(args.iter().map(|arg| shell_quote_single(arg)));
    parts.push("--port".to_string());
    parts.push(remote_port.to_string());
    parts.join(" ")
}

pub(super) fn build_remote_ssh_args(
    ssh_target: &str,
    ssh_port: Option<u16>,
    local_port: u16,
    remote_port: u16,
    remote_command: String,
    has_password: bool,
) -> Vec<String> {
    let mut args = remote_ssh_base_args(has_password, false);
    args.extend([
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
    ]);
    if let Some(ssh_port) = ssh_port {
        args.push("-p".to_string());
        args.push(ssh_port.to_string());
    }
    args.extend([ssh_target.to_string(), remote_command]);
    args
}

pub(super) fn build_remote_ssh_command_args(
    ssh_target: &str,
    ssh_port: Option<u16>,
    remote_command: String,
    has_password: bool,
) -> Vec<String> {
    let mut args = remote_ssh_base_args(has_password, false);
    if let Some(ssh_port) = ssh_port {
        args.push("-p".to_string());
        args.push(ssh_port.to_string());
    }
    args.extend([ssh_target.to_string(), remote_command]);
    args
}

pub(super) fn build_remote_ssh_forward_args(
    ssh_target: &str,
    ssh_port: Option<u16>,
    local_port: u16,
    remote_port: u16,
    has_password: bool,
) -> Vec<String> {
    let mut args = remote_ssh_base_args(has_password, false);
    args.extend([
        "-N".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
    ]);
    if let Some(ssh_port) = ssh_port {
        args.push("-p".to_string());
        args.push(ssh_port.to_string());
    }
    args.push(ssh_target.to_string());
    args
}

pub(super) fn build_remote_ssh_reverse_forward_args(
    ssh_target: &str,
    ssh_port: Option<u16>,
    remote_port: u16,
    local_port: u16,
    has_password: bool,
) -> Vec<String> {
    let mut args = remote_ssh_base_args(has_password, false);
    args.extend([
        "-N".to_string(),
        "-R".to_string(),
        format!("127.0.0.1:{remote_port}:127.0.0.1:{local_port}"),
    ]);
    if let Some(ssh_port) = ssh_port {
        args.push("-p".to_string());
        args.push(ssh_port.to_string());
    }
    args.push(ssh_target.to_string());
    args
}

fn remote_ssh_base_args(has_password: bool, use_multiplex: bool) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
    ];
    if has_password {
        args.push("NumberOfPasswordPrompts=1".to_string());
    } else {
        args.push("BatchMode=yes".to_string());
    }
    args.extend([
        "-o".to_string(),
        format!("ConnectTimeout={REMOTE_SSH_CONNECT_TIMEOUT_SECS}"),
    ]);
    if use_multiplex {
        args.extend(ssh_multiplex_args());
    } else {
        args.extend(ssh_disable_multiplex_args());
    }
    args
}

#[cfg(unix)]
fn ssh_disable_multiplex_args() -> Vec<String> {
    vec![
        "-o".to_string(),
        "ControlMaster=no".to_string(),
        "-o".to_string(),
        "ControlPath=none".to_string(),
        "-o".to_string(),
        "ControlPersist=no".to_string(),
    ]
}

#[cfg(not(unix))]
fn ssh_disable_multiplex_args() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
fn ssh_multiplex_args() -> Vec<String> {
    let Some(control_path) = ssh_control_path_template() else {
        return Vec::new();
    };
    vec![
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        "ControlPersist=300".to_string(),
        "-o".to_string(),
        format!("ControlPath={control_path}"),
    ]
}

#[cfg(not(unix))]
fn ssh_multiplex_args() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
fn ssh_control_path_template() -> Option<String> {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let dir = PathBuf::from(home).join(".kodex").join("ssh-control");
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            let path = dir.join("%C");
            let path = path.to_string_lossy().into_owned();
            if path.len() < 95 {
                return Some(path);
            }
        }
    }

    let user = std::env::var("USER")
        .ok()
        .map(|value| sanitize_control_path_part(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "user".into());
    Some(format!("/tmp/kodex-ssh-{user}-%C"))
}

#[cfg(unix)]
fn sanitize_control_path_part(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                Some(ch)
            } else {
                None
            }
        })
        .take(32)
        .collect()
}

fn configure_ssh_askpass(
    command: &mut std::process::Command,
    password: &str,
) -> anyhow::Result<()> {
    let askpass = std::env::current_exe()?;
    command
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "kodex")
        .env(KODEX_SSH_ASKPASS_ENV, "1")
        .env(KODEX_SSH_ASKPASS_PASSWORD_ENV, password);
    Ok(())
}

fn shell_quote_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn connect_loopback_tcp(
    port: u16,
    label: &str,
    log_config: Option<&SessionConfig>,
) -> agent_client_protocol::Result<tokio::net::TcpStream> {
    connect_loopback_tcp_with_retry(
        port,
        label,
        log_config,
        50,
        Duration::from_millis(200),
        Duration::from_millis(200),
    )
    .await
}

pub(super) async fn connect_loopback_tcp_with_retry(
    port: u16,
    label: &str,
    log_config: Option<&SessionConfig>,
    attempts: usize,
    connect_timeout: Duration,
    retry_delay: Duration,
) -> agent_client_protocol::Result<tokio::net::TcpStream> {
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().map_err(|e| {
        agent_client_protocol::util::internal_error(format!("invalid address: {e}"))
    })?;

    let mut last_err = None;
    let mut connected = None;
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        match std::net::TcpStream::connect_timeout(&addr, connect_timeout) {
            Ok(s) => {
                connected = Some(s);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < attempts {
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
    }

    let stream = connected.ok_or_else(|| {
        agent_client_protocol::util::internal_error(format!(
            "failed to connect to {label} at 127.0.0.1:{port}: {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    })?;
    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/tcp_connected",
            &json!({ "label": label, "port": port }),
        );
    }
    stream.set_nonblocking(true).map_err(|e| {
        agent_client_protocol::util::internal_error(format!("set_nonblocking: {e}"))
    })?;
    tokio::net::TcpStream::from_std(stream).map_err(|e| {
        agent_client_protocol::util::internal_error(format!("TcpStream::from_std: {e}"))
    })
}

pub(super) fn connect_tcp_stream(
    tcp_stream: tokio::net::TcpStream,
    client: impl ConnectTo<<Client as Role>::Counterpart>,
) -> agent_client_protocol::Result<
    impl std::future::Future<Output = agent_client_protocol::Result<()>>,
> {
    let (read_half, write_half) = tcp_stream.into_split();
    let reader = tokio::io::BufReader::new(read_half);
    let incoming_lines = futures::stream::unfold(reader, |mut reader| async move {
        let mut line = String::new();
        match tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await {
            Ok(0) => None,
            Ok(_) => {
                trim_line_ending(&mut line);
                Some((Ok(line), reader))
            }
            Err(e) => Some((Err(e), reader)),
        }
    });

    let writer = tokio::io::BufWriter::new(write_half);
    let outgoing_sink = futures::sink::unfold(writer, |mut writer, line: String| async move {
        use tokio::io::AsyncWriteExt;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))?;
        Ok::<_, std::io::Error>(writer)
    });

    Ok(agent_client_protocol::ConnectTo::<Client>::connect_to(
        Lines::new(outgoing_sink, incoming_lines),
        client,
    ))
}

pub(super) fn connect_streamable_http_endpoint(
    endpoint: String,
    client: impl ConnectTo<<Client as Role>::Counterpart>,
    acp_transport: RemoteAcpTransport,
    log_config: Option<SessionConfig>,
) -> agent_client_protocol::Result<
    impl std::future::Future<Output = agent_client_protocol::Result<()>>,
> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| {
            agent_client_protocol::util::internal_error(format!(
                "failed to create streamable-http client: {error}"
            ))
        })?;
    let (incoming_tx, incoming_lines) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let connection_id = Arc::new(Mutex::new(None));
    let session_token = Arc::new(Mutex::new(None));
    let get_task = Arc::new(Mutex::new(None));
    let last_initialize = Arc::new(Mutex::new(None));
    let current_session_id = Arc::new(Mutex::new(None));
    let current_session_cwd = Arc::new(Mutex::new(None));
    let state = StreamableHttpState {
        http: http.clone(),
        endpoint: endpoint.clone(),
        incoming_tx: incoming_tx.clone(),
        connection_id: connection_id.clone(),
        session_token: session_token.clone(),
        get_task: get_task.clone(),
        acp_transport,
        last_initialize: last_initialize.clone(),
        current_session_id: current_session_id.clone(),
        current_session_cwd: current_session_cwd.clone(),
        log_config: log_config.clone(),
    };

    if let Some(config) = log_config.as_ref() {
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_connecting",
            &json!({
                "endpoint": endpoint,
                "connection_id": connection_id
            }),
        );
    }

    let outgoing_sink = futures::sink::unfold(state, |state, line: String| async move {
        post_streamable_http_line(&state, line).await?;
        Ok::<_, std::io::Error>(state)
    });

    let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
        Lines::new(outgoing_sink, incoming_lines),
        client,
    );

    let connect_http = http.clone();
    let connect_endpoint = endpoint.clone();
    let connect_connection_id = connection_id.clone();
    let connect_session_token = session_token.clone();
    let connect_log_config = log_config.clone();
    Ok(async move {
        match acp_transport {
            RemoteAcpTransport::AcpStreamableHttp => {
                acp_streamable_http_connect(
                    &connect_endpoint,
                    &connect_http,
                    &connect_connection_id,
                    &connect_session_token,
                    connect_log_config.as_ref(),
                )
                .await
                .map_err(|error| {
                    agent_client_protocol::util::internal_error(format!(
                        "ACP streamable-http connect failed: {error}"
                    ))
                })?;
            }
            RemoteAcpTransport::CodeBuddyServeHttp => {
                codebuddy_acp_connect(
                    &connect_endpoint,
                    &connect_http,
                    &connect_connection_id,
                    &connect_session_token,
                    connect_log_config.as_ref(),
                )
                .await
                .map_err(|error| {
                    agent_client_protocol::util::internal_error(format!(
                        "CodeBuddy ACP streamable-http connect failed: {error}"
                    ))
                })?;
            }
            RemoteAcpTransport::Tcp => {}
        }
        let state = StreamableHttpState {
            http,
            endpoint,
            incoming_tx,
            connection_id,
            session_token,
            get_task: get_task.clone(),
            acp_transport,
            last_initialize,
            current_session_id,
            current_session_cwd,
            log_config,
        };
        maybe_spawn_streamable_http_get(&state);
        let result = protocol.await;
        if let Ok(mut guard) = get_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
        result
    })
}

#[derive(Clone)]
struct StreamableHttpState {
    http: reqwest::Client,
    endpoint: String,
    incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    connection_id: Arc<Mutex<Option<String>>>,
    session_token: Arc<Mutex<Option<String>>>,
    get_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    acp_transport: RemoteAcpTransport,
    last_initialize: Arc<Mutex<Option<serde_json::Value>>>,
    current_session_id: Arc<Mutex<Option<String>>>,
    current_session_cwd: Arc<Mutex<Option<String>>>,
    log_config: Option<SessionConfig>,
}

async fn acp_streamable_http_connect(
    endpoint: &str,
    http: &reqwest::Client,
    connection_id: &Arc<Mutex<Option<String>>>,
    session_token: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    let connect_url = format!("{endpoint}/connect");
    let mut last_error = None;
    let mut response = None;
    for attempt in 0..CODEBUDDY_ACP_CONNECT_ATTEMPTS {
        match codebuddy_http_headers(
            http.post(&connect_url).header(ACCEPT, "application/json"),
            connection_id,
        )
        .send()
        .await
        {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if let Some(config) = log_config {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/acp_streamable_http_connect_retry",
                        &json!({
                            "url": connect_url,
                            "attempt": attempt + 1,
                            "attempts": CODEBUDDY_ACP_CONNECT_ATTEMPTS,
                            "error": last_error.as_deref()
                        }),
                    );
                }
                if attempt + 1 < CODEBUDDY_ACP_CONNECT_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
    let Some(response) = response else {
        return Err(io_other(format!(
            "request failed after {CODEBUDDY_ACP_CONNECT_ATTEMPTS} attempts for {connect_url}: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )));
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "ACP streamable-http connect failed with status {status}: {body}"
        )));
    }
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    let payload = response.text().await.unwrap_or_default();
    remember_acp_connection_id_from_payload(&payload, connection_id);
    remember_acp_session_token_from_payload(&payload, session_token);
    let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
    let session_token = session_token.lock().ok().and_then(|guard| guard.clone());
    if connection_id.is_none() {
        return Err(io_other(format!(
            "ACP streamable-http connect response missing connection id: {payload}"
        )));
    }
    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/acp_streamable_http_connected",
            &json!({ "connection_id": connection_id, "session_token_present": session_token.is_some() }),
        );
    }
    Ok(())
}

async fn codebuddy_acp_connect(
    endpoint: &str,
    http: &reqwest::Client,
    connection_id: &Arc<Mutex<Option<String>>>,
    session_token: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    let connect_url = format!("{endpoint}/connect");
    let mut last_error = None;
    let mut response = None;
    for attempt in 0..CODEBUDDY_ACP_CONNECT_ATTEMPTS {
        match codebuddy_http_headers(
            http.post(&connect_url).header(ACCEPT, "application/json"),
            connection_id,
        )
        .send()
        .await
        {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if let Some(config) = log_config {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/codebuddy_acp_connect_retry",
                        &json!({
                            "url": connect_url,
                            "attempt": attempt + 1,
                            "attempts": CODEBUDDY_ACP_CONNECT_ATTEMPTS,
                            "error": last_error.as_deref()
                        }),
                    );
                }
                if attempt + 1 < CODEBUDDY_ACP_CONNECT_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
    let Some(response) = response else {
        return Err(io_other(format!(
            "request failed after {CODEBUDDY_ACP_CONNECT_ATTEMPTS} attempts for {connect_url}: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )));
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "CodeBuddy ACP connect failed with status {status}: {body}"
        )));
    }
    let payload = response.text().await.map_err(io_other)?;
    let (id, token) = codebuddy_connect_parts_from_payload(&payload)?;
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(id.clone());
    }
    if let Some(token) = token.as_ref() {
        if let Ok(mut guard) = session_token.lock() {
            *guard = Some(token.clone());
        }
    }
    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/codebuddy_acp_connected",
            &json!({ "connection_id": id, "session_token_present": token.is_some() }),
        );
    }
    Ok(())
}

fn codebuddy_connect_parts_from_payload(
    payload: &str,
) -> std::io::Result<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| io_other(format!("invalid CodeBuddy ACP connect response: {error}")))?;
    let data = value.get("data").unwrap_or(&value);
    let id = data
        .get("connectionId")
        .or_else(|| data.get("connection_id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| io_other("CodeBuddy ACP connect response missing connectionId"))?;
    let token = data
        .get("sessionToken")
        .or_else(|| data.get("session_token"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    Ok((id.to_string(), token))
}

async fn post_streamable_http_line(
    state: &StreamableHttpState,
    line: String,
) -> std::io::Result<()> {
    let body = serde_json::from_str::<serde_json::Value>(&line)
        .map_err(|error| io_other(format!("invalid ACP JSON-RPC message: {error}")))?;
    let expects_response_body = streamable_http_message_expects_response_body(&body);
    for attempt in 0..2 {
        match send_streamable_http_request_with_retry(state, &body).await {
            Ok(response) => {
                if !expects_response_body {
                    let result = handle_streamable_http_ack_response(
                        response,
                        &state.connection_id,
                        state.log_config.as_ref(),
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            maybe_spawn_streamable_http_get(state);
                            return Ok(());
                        }
                        Err(error)
                            if attempt == 0 && streamable_connection_not_found_error(&error) =>
                        {
                            reconnect_streamable_http_for_retry(state, &body).await?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                let result = handle_streamable_http_response(
                    response,
                    &state.connection_id,
                    &state.incoming_tx,
                    state.log_config.as_ref(),
                )
                .await;
                match result {
                    Ok(()) => {
                        maybe_spawn_streamable_http_get(state);
                        return Ok(());
                    }
                    Err(error) if attempt == 0 && streamable_connection_not_found_error(&error) => {
                        reconnect_streamable_http_for_retry(state, &body).await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if attempt == 0 && streamable_connection_not_found_error(&error) => {
                reconnect_streamable_http_for_retry(state, &body).await?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

async fn send_streamable_http_request_with_retry(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) -> std::io::Result<reqwest::Response> {
    remember_streamable_http_outgoing_request(state, body);
    let session_id = streamable_http_session_id_for_message(state, body);
    let mut last_error = None;
    for attempt in 0..50 {
        let request = state
            .http
            .post(&state.endpoint)
            .header(ACCEPT, "text/event-stream, application/json");
        let request = codebuddy_http_headers(request, &state.connection_id);
        let request = acp_session_token_header(request, &state.session_token);
        let request = acp_session_header(request, session_id.as_deref());
        match request.json(body).send().await {
            Ok(response) => {
                if response.status().as_u16() == 409 {
                    let status = response.status();
                    let response_body = response.text().await.unwrap_or_default();
                    if codebuddy_connection_not_found_response(&response_body) {
                        reconnect_streamable_http_for_retry(state, &body).await?;
                        if attempt + 1 < 50 {
                            continue;
                        }
                    }
                    let connection_id = state
                        .connection_id
                        .lock()
                        .ok()
                        .and_then(|guard| guard.clone());
                    return Err(io_other(format!(
                        "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {response_body}"
                    )));
                }
                return Ok(response);
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if attempt + 1 < 50 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
    Err(io_other(format!(
        "streamable-http ACP request failed after retries: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

fn remember_streamable_http_outgoing_request(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) {
    if message_has_method(body, "initialize") {
        if let Ok(mut guard) = state.last_initialize.lock() {
            *guard = Some(body.clone());
        }
    }

    if let Some(session_id) = acp_session_id_from_message(body) {
        if let Ok(mut guard) = state.current_session_id.lock() {
            *guard = Some(session_id);
        }
    }

    if let Some(cwd) = acp_session_cwd_from_message(body) {
        if let Ok(mut guard) = state.current_session_cwd.lock() {
            *guard = Some(cwd);
        }
    }
}

fn streamable_http_session_id_for_message(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) -> Option<String> {
    acp_session_id_from_message(body).or_else(|| {
        state
            .current_session_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    })
}

fn streamable_http_message_expects_response_body(body: &serde_json::Value) -> bool {
    !streamable_http_message_is_response(body)
        && !message_has_method(body, CODEBUDDY_RESOLVE_INTERRUPTION_METHOD)
}

fn streamable_http_message_is_response(body: &serde_json::Value) -> bool {
    match body {
        serde_json::Value::Array(items) => {
            !items.is_empty() && items.iter().all(streamable_http_message_is_response)
        }
        serde_json::Value::Object(object) => {
            object.get("method").is_none()
                && object.get("id").is_some()
                && (object.get("result").is_some() || object.get("error").is_some())
        }
        _ => false,
    }
}

async fn reconnect_streamable_http_for_retry(
    state: &StreamableHttpState,
    retry_body: &serde_json::Value,
) -> std::io::Result<()> {
    if let Ok(mut guard) = state.get_task.lock() {
        if let Some(task) = guard.take() {
            task.abort();
        }
    }
    if let Ok(mut guard) = state.connection_id.lock() {
        guard.take();
    }
    if let Ok(mut guard) = state.session_token.lock() {
        guard.take();
    }

    reconnect_streamable_http_transport(state).await?;

    if let Some(config) = state.log_config.as_ref() {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_reconnected",
            &json!({
                "retry_method": acp_method_from_message(retry_body)
            }),
        );
    }

    maybe_spawn_streamable_http_get(state);

    if message_has_method(retry_body, "initialize") {
        return Ok(());
    }

    let initialize = state
        .last_initialize
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .ok_or_else(|| io_other("CodeBuddy ACP reconnect cannot replay initialize request"))?;
    send_streamable_http_control_request(state, initialize, "initialize").await?;

    if message_has_method(retry_body, "session/new")
        || message_has_method(retry_body, "session/load")
    {
        return Ok(());
    }

    let session_id = acp_session_id_from_message(retry_body).or_else(|| {
        state
            .current_session_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    });
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let cwd = state
        .current_session_cwd
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| ".".to_string());
    send_streamable_http_control_request(
        state,
        codebuddy_session_load_request(&session_id, &cwd),
        "session/load",
    )
    .await
}

async fn reconnect_streamable_http_transport(state: &StreamableHttpState) -> std::io::Result<()> {
    match state.acp_transport {
        RemoteAcpTransport::AcpStreamableHttp => {
            acp_streamable_http_connect(
                &state.endpoint,
                &state.http,
                &state.connection_id,
                &state.session_token,
                state.log_config.as_ref(),
            )
            .await
        }
        RemoteAcpTransport::CodeBuddyServeHttp => {
            codebuddy_acp_connect(
                &state.endpoint,
                &state.http,
                &state.connection_id,
                &state.session_token,
                state.log_config.as_ref(),
            )
            .await
        }
        RemoteAcpTransport::Tcp => Err(io_other(
            "cannot reconnect TCP transport as streamable-http",
        )),
    }
}

async fn send_streamable_http_control_request(
    state: &StreamableHttpState,
    body: serde_json::Value,
    label: &str,
) -> std::io::Result<()> {
    let session_id = acp_session_id_from_message(&body);
    let request = state
        .http
        .post(&state.endpoint)
        .header(ACCEPT, "text/event-stream, application/json");
    let request = codebuddy_http_headers(request, &state.connection_id);
    let request = acp_session_token_header(request, &state.session_token);
    let request = acp_session_header(request, session_id.as_deref());
    let response = request.json(&body).send().await.map_err(io_other)?;
    if !response.status().is_success() {
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "CodeBuddy ACP reconnect {label} failed with status {status}: {response_body}"
        )));
    }
    remember_acp_connection_id_from_headers(response.headers(), &state.connection_id);
    let response_body = response.text().await.unwrap_or_default();
    remember_acp_connection_id_from_payload(&response_body, &state.connection_id);
    remember_acp_session_token_from_payload(&response_body, &state.session_token);
    Ok(())
}

fn codebuddy_session_load_request(session_id: &str, cwd: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": "kodex-codebuddy-reconnect-session-load",
        "method": "session/load",
        "params": {
            "sessionId": session_id,
            "cwd": cwd,
            "mcpServers": []
        }
    })
}

fn codebuddy_connection_not_found_response(body: &str) -> bool {
    body.to_ascii_lowercase().contains("connection not found")
}

fn streamable_connection_not_found_error(error: &std::io::Error) -> bool {
    codebuddy_connection_not_found_response(&error.to_string())
}

fn maybe_spawn_streamable_http_get(state: &StreamableHttpState) {
    if state
        .connection_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .is_none()
    {
        return;
    }
    let should_spawn = state
        .get_task
        .lock()
        .map(|guard| guard.is_none())
        .unwrap_or(false);
    if !should_spawn {
        return;
    }

    let http = state.http.clone();
    let endpoint = state.endpoint.clone();
    let incoming_tx = state.incoming_tx.clone();
    let connection_id = state.connection_id.clone();
    let session_token = state.session_token.clone();
    let log_config = state.log_config.clone();
    let get_task_store = state.get_task.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = run_streamable_http_get(
            http,
            endpoint,
            incoming_tx,
            connection_id,
            session_token,
            log_config,
        )
        .await
        {
            let _ = error;
        }
        if let Ok(mut guard) = get_task_store.lock() {
            guard.take();
        }
    });
    if let Ok(mut guard) = state.get_task.lock() {
        if guard.is_none() {
            *guard = Some(task);
        } else {
            task.abort();
        }
    } else {
        task.abort();
    }
}

async fn run_streamable_http_get(
    http: reqwest::Client,
    endpoint: String,
    incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    connection_id: Arc<Mutex<Option<String>>>,
    session_token: Arc<Mutex<Option<String>>>,
    log_config: Option<SessionConfig>,
) -> std::io::Result<()> {
    let request = http.get(&endpoint).header(ACCEPT, "text/event-stream");
    let request = codebuddy_http_headers(request, &connection_id);
    let request = acp_session_token_header(request, &session_token);
    let response = request.send().await.map_err(io_other)?;
    if !response.status().is_success() {
        if let Some(config) = log_config.as_ref() {
            let _ = append_runtime_event_log(
                config,
                "agent/streamable_http_get_ignored",
                &json!({ "status": response.status().as_u16() }),
            );
        }
        return Ok(());
    }
    handle_streamable_http_response(response, &connection_id, &incoming_tx, log_config.as_ref())
        .await
}

async fn handle_streamable_http_ack_response(
    response: reqwest::Response,
    connection_id: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        return Err(io_other(format!(
            "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {body}"
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    if !content_type.contains("text/event-stream") {
        let body = response.text().await.unwrap_or_default();
        remember_acp_connection_id_from_payload(&body, connection_id);
    }

    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_response_ack",
            &json!({ "contentType": content_type }),
        );
    }
    Ok(())
}

async fn handle_streamable_http_response(
    response: reqwest::Response,
    connection_id: &Arc<Mutex<Option<String>>>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        return Err(io_other(format!(
            "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {body}"
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    if content_type.contains("text/event-stream") {
        spawn_streamable_http_sse_consumer(response, incoming_tx.clone(), log_config.cloned());
    } else {
        let body = response.text().await.map_err(io_other)?;
        remember_acp_connection_id_from_payload(&body, connection_id);
        feed_streamable_http_payload(&body, incoming_tx)?;
    }

    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_response",
            &json!({ "contentType": content_type }),
        );
    }
    Ok(())
}

fn spawn_streamable_http_sse_consumer(
    response: reqwest::Response,
    incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    log_config: Option<SessionConfig>,
) {
    tokio::spawn(async move {
        let result = consume_streamable_http_sse(response, &incoming_tx).await;
        if let Some(config) = log_config.as_ref() {
            match result {
                Ok(()) => {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/streamable_http_sse_finished",
                        &json!({}),
                    );
                }
                Err(error) => {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/streamable_http_sse_error",
                        &json!({ "error": error.to_string() }),
                    );
                }
            }
        }
    });
}

async fn consume_streamable_http_sse(
    response: reqwest::Response,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut event_data = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(io_other)?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        drain_sse_buffer(&mut buffer, &mut event_data, incoming_tx)?;
    }
    if !buffer.is_empty() {
        buffer.push('\n');
        drain_sse_buffer(&mut buffer, &mut event_data, incoming_tx)?;
    }
    flush_sse_event(&mut event_data, incoming_tx)?;
    Ok(())
}

fn drain_sse_buffer(
    buffer: &mut String,
    event_data: &mut Vec<String>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    while let Some(line_end) = buffer.find('\n') {
        let mut line = buffer[..line_end].to_string();
        buffer.drain(..=line_end);
        if line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            flush_sse_event(event_data, incoming_tx)?;
        } else if let Some(data) = line.strip_prefix("data:") {
            event_data.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
    Ok(())
}

fn flush_sse_event(
    event_data: &mut Vec<String>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    if event_data.is_empty() {
        return Ok(());
    }
    let payload = event_data.join("\n");
    event_data.clear();
    feed_streamable_http_payload(&payload, incoming_tx)
}

fn feed_streamable_http_payload(
    payload: &str,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(());
    }
    incoming_tx
        .unbounded_send(Ok(payload.to_string()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "ACP receiver closed"))
}

fn codebuddy_http_headers(
    request: reqwest::RequestBuilder,
    connection_id: &Arc<Mutex<Option<String>>>,
) -> reqwest::RequestBuilder {
    let request = request.header(CODEBUDDY_REQUEST_HEADER, "1");
    let value = connection_id.lock().ok().and_then(|guard| guard.clone());
    let Some(value) = value else {
        return request;
    };
    match HeaderValue::from_str(&value) {
        Ok(value) => request.header(CODEBUDDY_ACP_CONNECTION_ID_HEADER, value),
        Err(_) => request,
    }
}

fn acp_session_header(
    request: reqwest::RequestBuilder,
    session_id: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(session_id) = session_id else {
        return request;
    };
    match HeaderValue::from_str(session_id) {
        Ok(value) => request.header(ACP_SESSION_ID_HEADER, value),
        Err(_) => request,
    }
}

fn acp_session_token_header(
    request: reqwest::RequestBuilder,
    session_token: &Arc<Mutex<Option<String>>>,
) -> reqwest::RequestBuilder {
    let token = session_token.lock().ok().and_then(|guard| guard.clone());
    let Some(token) = token else {
        return request;
    };
    match HeaderValue::from_str(&token) {
        Ok(value) => request.header(ACP_SESSION_TOKEN_HEADER, value),
        Err(_) => request,
    }
}

fn acp_session_id_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("params")
        .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn acp_session_cwd_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("params")
        .and_then(|params| params.get("cwd"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn acp_method_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("method")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn message_has_method(message: &serde_json::Value, method: &str) -> bool {
    match message {
        serde_json::Value::Array(items) => {
            items.iter().any(|item| message_has_method(item, method))
        }
        _ => message
            .get("method")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == method),
    }
}

fn remember_acp_connection_id_from_headers(
    headers: &reqwest::header::HeaderMap,
    connection_id: &Arc<Mutex<Option<String>>>,
) {
    let Some(value) = headers
        .get(ACP_CONNECTION_ID_HEADER)
        .or_else(|| headers.get(CODEBUDDY_ACP_CONNECTION_ID_HEADER))
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(value.to_string());
    }
}

fn remember_acp_connection_id_from_payload(
    payload: &str,
    connection_id: &Arc<Mutex<Option<String>>>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let id = value
        .get("result")
        .and_then(|result| {
            result
                .get("connectionId")
                .or_else(|| result.get("connection_id"))
        })
        .or_else(|| {
            value.get("data").and_then(|data| {
                data.get("connectionId")
                    .or_else(|| data.get("connection_id"))
            })
        })
        .or_else(|| value.get("connectionId"))
        .or_else(|| value.get("connection_id"))
        .and_then(|value| value.as_str());
    let Some(id) = id else {
        return;
    };
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(id.to_string());
    }
}

fn remember_acp_session_token_from_payload(
    payload: &str,
    session_token: &Arc<Mutex<Option<String>>>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let token = value
        .get("result")
        .and_then(|result| {
            result
                .get("sessionToken")
                .or_else(|| result.get("session_token"))
        })
        .or_else(|| {
            value.get("data").and_then(|data| {
                data.get("sessionToken")
                    .or_else(|| data.get("session_token"))
            })
        })
        .or_else(|| value.get("sessionToken"))
        .or_else(|| value.get("session_token"))
        .and_then(|value| value.as_str());
    let Some(token) = token else {
        return;
    };
    if let Ok(mut guard) = session_token.lock() {
        *guard = Some(token.to_string());
    }
}

fn io_other(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, error.to_string())
}

fn read_agent_stdout(
    stdout: std::process::ChildStdout,
    tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                trim_line_ending(&mut line);
                if tx.unbounded_send(Ok(line)).is_err() {
                    break;
                }
            }
            Err(err) => {
                let _ = tx.unbounded_send(Err(err));
                break;
            }
        }
    }
}

fn read_agent_stderr(stderr: std::process::ChildStderr, tx: mpsc::Sender<String>) {
    let mut reader = BufReader::new(stderr);
    let mut collected = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                trim_line_ending(&mut line);
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
            }
            Err(_) => break,
        }
    }
    let _ = tx.send(collected);
}

fn read_remote_ssh_stderr(
    stderr: std::process::ChildStderr,
    tx: mpsc::Sender<String>,
    live_stderr: Arc<Mutex<String>>,
    ready_tx: tokio::sync::oneshot::Sender<RemoteAgentReady>,
    acp_transport: RemoteAcpTransport,
) {
    let mut reader = BufReader::new(stderr);
    let mut collected = String::new();
    let mut ready_tx = Some(ready_tx);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                trim_line_ending(&mut line);
                if let Some(ready) = remote_agent_ready_from_line(&line, acp_transport) {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(ready);
                    }
                    if line == REMOTE_AGENT_READY_MARKER {
                        continue;
                    }
                }
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
                if let Ok(mut live) = live_stderr.lock() {
                    if !live.is_empty() {
                        live.push('\n');
                    }
                    live.push_str(&line);
                }
            }
            Err(_) => break,
        }
    }
    let _ = tx.send(collected);
}

fn remote_agent_ready_from_line(
    line: &str,
    acp_transport: RemoteAcpTransport,
) -> Option<RemoteAgentReady> {
    match acp_transport {
        RemoteAcpTransport::Tcp => {
            (line == REMOTE_AGENT_READY_MARKER).then(RemoteAgentReady::default)
        }
        RemoteAcpTransport::AcpStreamableHttp | RemoteAcpTransport::CodeBuddyServeHttp => {
            if line == REMOTE_AGENT_READY_MARKER {
                Some(RemoteAgentReady::default())
            } else {
                remote_streamable_http_endpoint_port(line).map(|endpoint_port| RemoteAgentReady {
                    endpoint_port: Some(endpoint_port),
                })
            }
        }
    }
}

#[cfg(test)]
fn is_remote_agent_ready_line(line: &str, acp_transport: RemoteAcpTransport) -> bool {
    remote_agent_ready_from_line(line, acp_transport).is_some()
}

fn remote_streamable_http_endpoint_port(line: &str) -> Option<u16> {
    let endpoint = line
        .split_once(REMOTE_STREAMABLE_HTTP_ENDPOINT_MARKER)?
        .1
        .trim();
    let without_scheme = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))?;
    let host_port = without_scheme.split('/').next()?;
    let port = host_port.rsplit_once(':')?.1;
    port.parse::<u16>().ok()
}

async fn wait_remote_agent_ready(
    ready_rx: tokio::sync::oneshot::Receiver<RemoteAgentReady>,
    live_stderr: Arc<Mutex<String>>,
    log_config: Option<&SessionConfig>,
) -> agent_client_protocol::Result<RemoteAgentReady> {
    match tokio::time::timeout(REMOTE_AGENT_READY_TIMEOUT, ready_rx).await {
        Ok(Ok(ready)) => {
            if let Some(config) = log_config {
                let _ = append_runtime_event_log(
                    config,
                    "agent/remote_ready",
                    &json!({
                        "marker": REMOTE_AGENT_READY_MARKER,
                        "endpoint_port": ready.endpoint_port
                    }),
                );
            }
            Ok(ready)
        }
        Ok(Err(_)) => Err(agent_client_protocol::util::internal_error(
            remote_agent_readiness_error(
                "ssh remote agent process ended before readiness was reported",
                &live_stderr,
            ),
        )),
        Err(_) => {
            let message = remote_agent_readiness_error(
                "timed out waiting for remote ACP agent readiness",
                &live_stderr,
            );
            Err(agent_client_protocol::util::internal_error(message))
        }
    }
}

fn remote_agent_readiness_error(prefix: &str, live_stderr: &Arc<Mutex<String>>) -> String {
    let stderr = live_stderr
        .lock()
        .map(|stderr| sanitize_remote_agent_diagnostic(&stderr))
        .unwrap_or_default();
    if stderr.trim().is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {stderr}")
    }
}

fn sanitize_remote_agent_diagnostic(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed
        .lines()
        .any(|line| contains_secret_material(line) || line.contains("-----BEGIN "))
    {
        return "Credential details redacted".into();
    }
    const MAX_DIAGNOSTIC_LEN: usize = 1200;
    if trimmed.len() <= MAX_DIAGNOSTIC_LEN {
        return trimmed.to_string();
    }
    format!(
        "{}...",
        trimmed.chars().take(MAX_DIAGNOSTIC_LEN).collect::<String>()
    )
}

fn contains_secret_material(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password=",
        "password:",
        "passphrase",
        "private key",
        "api_key",
        "apikey",
        "secret=",
        "token=",
        "auth_token",
        "authorization:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn write_agent_stdin(
    shared_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    rx: mpsc::Receiver<String>,
) {
    for line in rx {
        let Ok(mut guard) = shared_stdin.lock() else {
            break;
        };
        let Some(ref mut stdin) = *guard else {
            break;
        };
        if stdin.write_all(line.as_bytes()).is_err() {
            break;
        }
        if stdin.write_all(b"\n").is_err() {
            break;
        }
        if stdin.flush().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_remote_streamable_http_transport() {
        assert_eq!(
            detect_remote_acp_transport(&[
                "--acp".to_string(),
                "--acp-transport".to_string(),
                "streamable-http".to_string(),
            ]),
            RemoteAcpTransport::AcpStreamableHttp
        );
        assert_eq!(
            detect_remote_acp_transport(&["--acp-transport=streamable-http".to_string()]),
            RemoteAcpTransport::AcpStreamableHttp
        );
        assert_eq!(
            detect_remote_acp_transport(&["--serve".to_string()]),
            RemoteAcpTransport::CodeBuddyServeHttp
        );
        assert_eq!(
            detect_remote_acp_transport(&["--port".to_string(), "12345".to_string()]),
            RemoteAcpTransport::Tcp
        );
    }

    #[test]
    fn streamable_http_endpoint_line_does_not_bypass_port_probe() {
        assert!(!is_remote_agent_ready_line(
            "ACP streamable-http endpoint: http://127.0.0.1:35499/api/v1/acp",
            RemoteAcpTransport::Tcp,
        ));
        let ready = remote_agent_ready_from_line(
            "ACP streamable-http endpoint: http://127.0.0.1:35499/api/v1/acp",
            RemoteAcpTransport::AcpStreamableHttp,
        )
        .expect("endpoint should report streamable-http readiness");
        assert_eq!(ready.endpoint_port, Some(35499));
    }

    #[test]
    fn streamable_remote_command_does_not_wait_on_requested_port_probe() {
        let command = build_remote_streamable_agent_command(
            "/workspace/project",
            Path::new("/home/user/.kodex/remote-agents/codebuddy/current/bin/codebuddy"),
            &[
                "--acp".to_string(),
                "--acp-transport".to_string(),
                "streamable-http".to_string(),
            ],
            &[],
            4567,
        );
        assert!(command.contains("--port 4567"));
        assert!(!command.contains("/proc/net/tcp"));
        assert!(!command.contains(REMOTE_AGENT_READY_MARKER));
    }

    #[test]
    fn remote_ssh_forward_args_use_discovered_remote_port() {
        let args =
            build_remote_ssh_forward_args("root@example.com", Some(2222), 3456, 45913, false);
        assert!(args.contains(&"-N".to_string()));
        assert!(args.contains(&"127.0.0.1:3456:127.0.0.1:45913".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(!args.contains(&"ControlMaster=auto".to_string()));
        assert!(!args.contains(&"ControlPersist=300".to_string()));
        assert!(args.contains(&"ControlMaster=no".to_string()));
        assert!(args.contains(&"ControlPath=none".to_string()));
        assert!(args.contains(&"ControlPersist=no".to_string()));
    }

    #[test]
    fn readiness_error_includes_remote_agent_stderr() {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<RemoteAgentReady>();
        drop(ready_tx);
        let live_stderr = Arc::new(Mutex::new(
            "agent startup failed\nmissing remote provider configuration".to_string(),
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let error = runtime
            .block_on(wait_remote_agent_ready(ready_rx, live_stderr, None))
            .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("ended before readiness was reported"));
        assert!(message.contains("missing remote provider configuration"));
    }

    #[test]
    fn codebuddy_connect_payload_extracts_session_token() {
        let (connection_id, session_token) = codebuddy_connect_parts_from_payload(
            r#"{"data":{"connectionId":"conn-123","sessionToken":"token-456"}}"#,
        )
        .unwrap();

        assert_eq!(connection_id, "conn-123");
        assert_eq!(session_token.as_deref(), Some("token-456"));
    }

    #[test]
    fn codebuddy_connect_payload_accepts_root_level_fields() {
        let (connection_id, session_token) = codebuddy_connect_parts_from_payload(
            r#"{"connection_id":"conn-abc","session_token":"token-def"}"#,
        )
        .unwrap();

        assert_eq!(connection_id, "conn-abc");
        assert_eq!(session_token.as_deref(), Some("token-def"));
    }

    #[test]
    fn codebuddy_connection_not_found_response_is_detected() {
        assert!(codebuddy_connection_not_found_response(
            r#"{"jsonrpc":"2.0","error":{"message":"Connection not found. Please establish a connection first via POST /acp/connect before sending requests."}}"#,
        ));
        assert!(!codebuddy_connection_not_found_response(
            r#"{"jsonrpc":"2.0","error":{"message":"Another ACP client is already connected."}}"#,
        ));
    }

    #[test]
    fn codebuddy_reconnect_session_load_request_uses_current_session() {
        let request = codebuddy_session_load_request("session-123", "/workspace/project");

        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["method"], "session/load");
        assert_eq!(request["params"]["sessionId"], "session-123");
        assert_eq!(request["params"]["cwd"], "/workspace/project");
        assert_eq!(request["params"]["mcpServers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn message_has_method_matches_batched_requests() {
        let request = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "session/cancel" },
            { "jsonrpc": "2.0", "id": 2, "method": "initialize" }
        ]);

        assert!(message_has_method(&request, "initialize"));
        assert!(!message_has_method(&request, "session/load"));
    }

    #[test]
    fn streamable_http_message_response_posts_are_ack_only() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": "allow"
                }
            }
        });
        let request = json!({
            "jsonrpc": "2.0",
            "id": 43,
            "method": "session/prompt",
            "params": {
                "sessionId": "session-123"
            }
        });
        let resolve_interruption = json!({
            "jsonrpc": "2.0",
            "id": 44,
            "method": CODEBUDDY_RESOLVE_INTERRUPTION_METHOD,
            "params": {
                "sessionId": "session-123",
                "toolCallId": "call_123",
                "interruptionId": "ir-call_123",
                "decision": "allow"
            }
        });
        let mixed_batch = json!([response.clone(), request.clone()]);

        assert!(!streamable_http_message_expects_response_body(&response));
        assert!(streamable_http_message_expects_response_body(&request));
        assert!(!streamable_http_message_expects_response_body(
            &resolve_interruption
        ));
        assert!(streamable_http_message_expects_response_body(&mixed_batch));
    }

    #[test]
    fn streamable_http_session_id_falls_back_to_current_session_for_responses() {
        let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
        let state = StreamableHttpState {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: "http://127.0.0.1:1/api/v1/acp".into(),
            incoming_tx,
            connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
            session_token: Arc::new(Mutex::new(Some("token-1".into()))),
            get_task: Arc::new(Mutex::new(None)),
            acp_transport: RemoteAcpTransport::AcpStreamableHttp,
            last_initialize: Arc::new(Mutex::new(None)),
            current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
            current_session_cwd: Arc::new(Mutex::new(None)),
            log_config: None,
        };
        let response = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": "allow"
                }
            }
        });

        assert_eq!(
            streamable_http_session_id_for_message(&state, &response).as_deref(),
            Some("session-123")
        );
    }

    #[test]
    fn streamable_http_ack_only_posts_do_not_wait_for_sse_body() {
        let permission_response = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": "allow"
                }
            }
        });
        let resolve_interruption = json!({
            "jsonrpc": "2.0",
            "id": 44,
            "method": CODEBUDDY_RESOLVE_INTERRUPTION_METHOD,
            "params": {
                "sessionId": "session-123",
                "toolCallId": "call_123",
                "interruptionId": "ir-call_123",
                "decision": "allow"
            }
        });

        assert_ack_only_post_does_not_wait_for_sse_body(permission_response);
        assert_ack_only_post_does_not_wait_for_sse_body(resolve_interruption);
    }

    #[test]
    fn streamable_http_request_posts_do_not_wait_for_sse_body() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_mock_http_request(&mut stream);
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.header("acp-session-id").as_deref(),
                Some("session-123")
            );

            let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            assert!(streamable_http_message_expects_response_body(&value));

            use std::io::Write;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n: open\n\n",
                )
                .unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });

        let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
        let state = StreamableHttpState {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
            incoming_tx,
            connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
            session_token: Arc::new(Mutex::new(Some("token-1".into()))),
            get_task: Arc::new(Mutex::new(None)),
            acp_transport: RemoteAcpTransport::AcpStreamableHttp,
            last_initialize: Arc::new(Mutex::new(None)),
            current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
            current_session_cwd: Arc::new(Mutex::new(None)),
            log_config: None,
        };
        let prompt = json!({
            "jsonrpc": "2.0",
            "id": 43,
            "method": "session/prompt",
            "params": {
                "sessionId": "session-123",
                "prompt": [{ "type": "text", "text": "hello" }]
            }
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .unwrap();

        runtime
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_millis(200),
                    post_streamable_http_line(&state, prompt.to_string()),
                )
                .await
            })
            .expect("request POST should hand off the SSE body to a background task")
            .unwrap();

        if let Ok(mut guard) = state.get_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
        server_thread.join().unwrap();
    }

    fn assert_ack_only_post_does_not_wait_for_sse_body(payload: serde_json::Value) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_mock_http_request(&mut stream);
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.header("acp-session-id").as_deref(),
                Some("session-123")
            );

            let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            assert!(!streamable_http_message_expects_response_body(&value));

            use std::io::Write;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n: open\n\n",
                )
                .unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });

        let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
        let state = StreamableHttpState {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
            incoming_tx,
            connection_id: Arc::new(Mutex::new(Some("connection-1".into()))),
            session_token: Arc::new(Mutex::new(Some("token-1".into()))),
            get_task: Arc::new(Mutex::new(None)),
            acp_transport: RemoteAcpTransport::AcpStreamableHttp,
            last_initialize: Arc::new(Mutex::new(None)),
            current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
            current_session_cwd: Arc::new(Mutex::new(None)),
            log_config: None,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .unwrap();

        runtime
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_millis(200),
                    post_streamable_http_line(&state, payload.to_string()),
                )
                .await
            })
            .expect("response POST should not wait for the SSE body")
            .unwrap();

        server_thread.join().unwrap();
    }

    #[test]
    fn streamable_http_post_reconnects_and_retries_when_connection_is_missing() {
        let server = MockStreamableHttpServer::start();
        let (incoming_tx, _incoming_rx) = futures_mpsc::unbounded::<std::io::Result<String>>();
        let state = StreamableHttpState {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            endpoint: server.endpoint.clone(),
            incoming_tx,
            connection_id: Arc::new(Mutex::new(Some("stale-connection".into()))),
            session_token: Arc::new(Mutex::new(Some("stale-token".into()))),
            get_task: Arc::new(Mutex::new(None)),
            acp_transport: RemoteAcpTransport::AcpStreamableHttp,
            last_initialize: Arc::new(Mutex::new(Some(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": 1 }
            })))),
            current_session_id: Arc::new(Mutex::new(Some("session-123".into()))),
            current_session_cwd: Arc::new(Mutex::new(Some("/workspace/project".into()))),
            log_config: None,
        };
        let prompt = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": "session-123",
                "prompt": [{ "type": "text", "text": "hello" }]
            }
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .unwrap();

        let response = runtime
            .block_on(send_streamable_http_request_with_retry(&state, &prompt))
            .unwrap();
        assert!(response.status().is_success());
        let _ = runtime.block_on(response.text()).unwrap();
        if let Ok(mut guard) = state.get_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }

        server.wait_until_complete();
        assert_eq!(server.connect_count(), 1);
        assert_eq!(server.initialize_count(), 1);
        assert_eq!(server.session_load_count(), 1);
        assert_eq!(server.retried_prompt_count(), 1);
        assert_eq!(
            state
                .connection_id
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .as_deref(),
            Some("fresh-connection")
        );
        assert_eq!(
            state
                .session_token
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .as_deref(),
            Some("fresh-token")
        );
    }

    struct MockStreamableHttpServer {
        endpoint: String,
        state: Arc<MockStreamableHttpState>,
    }

    #[derive(Default)]
    struct MockStreamableHttpState {
        connect_count: std::sync::atomic::AtomicUsize,
        prompt_count: std::sync::atomic::AtomicUsize,
        initialize_count: std::sync::atomic::AtomicUsize,
        session_load_count: std::sync::atomic::AtomicUsize,
        retried_prompt_count: std::sync::atomic::AtomicUsize,
    }

    impl MockStreamableHttpServer {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let state = Arc::new(MockStreamableHttpState::default());
            let thread_state = state.clone();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while std::time::Instant::now() < deadline
                    && thread_state
                        .retried_prompt_count
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == 0
                {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            handle_mock_streamable_http_request(&mut stream, &thread_state)
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                endpoint: format!("http://{addr}{STREAMABLE_HTTP_PATH}"),
                state,
            }
        }

        fn wait_until_complete(&self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if self.retried_prompt_count() > 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("mock streamable-http server did not receive retried prompt");
        }

        fn connect_count(&self) -> usize {
            self.state
                .connect_count
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn initialize_count(&self) -> usize {
            self.state
                .initialize_count
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn session_load_count(&self) -> usize {
            self.state
                .session_load_count
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn retried_prompt_count(&self) -> usize {
            self.state
                .retried_prompt_count
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn handle_mock_streamable_http_request(
        stream: &mut std::net::TcpStream,
        state: &MockStreamableHttpState,
    ) {
        let request = read_mock_http_request(stream);
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", path) if path.ends_with("/connect") => {
                state
                    .connect_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                write_mock_json_response(
                    stream,
                    200,
                    r#"{"connectionId":"fresh-connection","sessionToken":"fresh-token"}"#,
                );
            }
            ("GET", _) => {
                write_mock_response(stream, 200, "text/event-stream", ":ok\n\n");
            }
            ("POST", _) => {
                let value: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                let method = value
                    .get("method")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                match method {
                    "session/prompt" => {
                        let count = state
                            .prompt_count
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if count == 0 {
                            write_mock_json_response(
                                stream,
                                409,
                                r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Connection not found. Please establish a connection first via POST /acp/connect before sending requests."},"id":null}"#,
                            );
                        } else {
                            assert_eq!(
                                request.header("acp-connection-id").as_deref(),
                                Some("fresh-connection")
                            );
                            assert_eq!(request.header_count("acp-connection-id"), 1);
                            assert_eq!(
                                request.header("acp-session-token").as_deref(),
                                Some("fresh-token")
                            );
                            state
                                .retried_prompt_count
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            write_mock_json_response(
                                stream,
                                200,
                                r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
                            );
                        }
                    }
                    "initialize" => {
                        state
                            .initialize_count
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        write_mock_json_response(
                            stream,
                            200,
                            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                        );
                    }
                    "session/load" => {
                        assert_eq!(value["params"]["sessionId"], "session-123");
                        assert_eq!(value["params"]["cwd"], "/workspace/project");
                        state
                            .session_load_count
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        write_mock_json_response(
                            stream,
                            200,
                            r#"{"jsonrpc":"2.0","id":"kodex-codebuddy-reconnect-session-load","result":{}}"#,
                        );
                    }
                    other => panic!("unexpected mock ACP method {other}"),
                }
            }
            other => panic!("unexpected mock HTTP request {other:?}"),
        }
    }

    struct MockHttpRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl MockHttpRequest {
        fn header(&self, name: &str) -> Option<String> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        }

        fn header_count(&self, name: &str) -> usize {
            self.headers
                .iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                .count()
        }
    }

    fn read_mock_http_request(stream: &mut std::net::TcpStream) -> MockHttpRequest {
        use std::io::Read;

        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "mock HTTP request ended before headers");
            buffer.extend_from_slice(&chunk[..count]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().unwrap();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap().to_string();
        let path = request_parts.next().unwrap().to_string();
        let headers = lines
            .filter_map(|line| {
                if line.is_empty() {
                    return None;
                }
                let (key, value) = line.split_once(':')?;
                Some((key.trim().to_string(), value.trim().to_string()))
            })
            .collect::<Vec<_>>();
        let content_length = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0, "mock HTTP request ended before body");
            buffer.extend_from_slice(&chunk[..count]);
        }
        let body =
            String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string();
        MockHttpRequest {
            method,
            path,
            headers,
            body,
        }
    }

    fn write_mock_json_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        write_mock_response(stream, status, "application/json", body);
    }

    fn write_mock_response(
        stream: &mut std::net::TcpStream,
        status: u16,
        content_type: &str,
        body: &str,
    ) {
        use std::io::Write;

        let status_text = match status {
            200 => "OK",
            409 => "Conflict",
            _ => "Status",
        };
        let response = format!(
            "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

struct AgentChildGuard {
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(windows)]
    _job: Option<WindowsKillOnDropJob>,
}

impl AgentChildGuard {
    fn new(child: Child, shutdown_signal: ShutdownSignal) -> Self {
        #[cfg(windows)]
        let job = WindowsKillOnDropJob::for_child(&child).ok();
        let guard = Self {
            child: Arc::new(Mutex::new(Some(child))),
            #[cfg(windows)]
            _job: job,
        };
        shutdown_signal.register_agent_child(&guard.child);
        if shutdown_signal.is_requested() {
            let _ = kill_child_handle(&guard.child);
        }
        guard
    }

    fn handle(&self) -> Arc<Mutex<Option<Child>>> {
        self.child.clone()
    }
}

#[cfg(windows)]
struct WindowsKillOnDropJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for WindowsKillOnDropJob {}

#[cfg(windows)]
impl WindowsKillOnDropJob {
    fn for_child(child: &Child) -> anyhow::Result<Self> {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                anyhow::bail!("CreateJobObjectW failed");
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("SetInformationJobObject failed");
            }

            let process = child.as_raw_handle() as HANDLE;
            let assign_ok = AssignProcessToJobObject(job, process);
            if assign_ok == 0 {
                CloseHandle(job);
                anyhow::bail!("AssignProcessToJobObject failed");
            }

            Ok(Self(job))
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsKillOnDropJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

impl Drop for AgentChildGuard {
    fn drop(&mut self) {
        let _ = kill_child_handle(&self.child);
    }
}

pub(super) fn kill_child_handle(child: &Arc<Mutex<Option<Child>>>) -> anyhow::Result<()> {
    let Ok(mut guard) = child.lock() else {
        return Ok(());
    };
    let Some(mut child) = guard.take() else {
        return Ok(());
    };

    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    Ok(())
}

async fn monitor_hidden_agent_child(
    child: Arc<Mutex<Option<Child>>>,
    stderr_rx: mpsc::Receiver<String>,
    log_config: Option<SessionConfig>,
    label: &'static str,
) -> agent_client_protocol::Result<()> {
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    thread::spawn(move || {
        loop {
            let result = {
                let mut guard = match child.lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        let _ =
                            exit_tx.send(Err(std::io::Error::other("agent child lock poisoned")));
                        return;
                    }
                };
                let Some(child) = guard.as_mut() else {
                    let _ = exit_tx.send(Err(std::io::Error::other("agent child handle closed")));
                    return;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        guard.take();
                        Some(Ok(status))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        guard.take();
                        Some(Err(error))
                    }
                }
            };

            if let Some(result) = result {
                let _ = exit_tx.send(result);
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let status = exit_rx
        .await
        .map_err(|_| agent_client_protocol::util::internal_error("agent wait thread closed"))?
        .map_err(agent_client_protocol::Error::into_internal_error)?;
    let success = status.success();
    let stderr = if success {
        String::new()
    } else {
        stderr_rx
            .recv_timeout(Duration::from_millis(200))
            .unwrap_or_default()
    };
    let payload = if stderr.is_empty() {
        json!({
            "label": label,
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code()
        })
    } else {
        json!({
            "label": label,
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code(),
            "stderr": stderr.clone()
        })
    };
    if let Some(config) = log_config.as_ref() {
        let _ = append_runtime_event_log(config, "agent/process_exit", &payload);
    }

    if success {
        return Ok(());
    }

    let message = if stderr.is_empty() {
        format!("agent process exited with {status}")
    } else {
        format!("agent process exited with {status}: {stderr}")
    };
    Err(agent_client_protocol::util::internal_error(message))
}
