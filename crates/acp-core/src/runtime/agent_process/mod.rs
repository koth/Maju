use super::process::{
    agent_spawn_command, apply_process_cwd_and_pwd, hide_console_window, parse_env_assignment,
};
use super::shutdown::{ShutdownCleanupHook, ShutdownSignal};
use crate::codex_api_proxy::{
    configure_codex_api_proxy_model_provider_map, ensure_codex_api_proxy,
};
use crate::events::{RemoteSshReverseForward, SessionConfig};
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::{Client, ConnectTo, Lines, Role};
use anyhow::anyhow;
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

mod process_lifecycle;
mod remote_ssh;
mod streamable_http;
#[cfg(test)]
mod tests;

pub(super) use process_lifecycle::kill_child_handle;
use process_lifecycle::{
    AgentChildGuard, configure_agent_process_group, monitor_hidden_agent_child, read_agent_stderr,
    read_agent_stdout, trim_line_ending, write_agent_stdin,
};
#[cfg(test)]
pub(super) use remote_ssh::REMOTE_AGENT_READY_MARKER;
use remote_ssh::{
    REMOTE_SSH_CONNECT_TIMEOUT_SECS, configure_ssh_askpass, read_remote_ssh_stderr,
    wait_remote_agent_ready,
};
pub(super) use remote_ssh::{
    build_remote_agent_cleanup_command, build_remote_agent_command,
    build_remote_ssh_agent_command_args, build_remote_ssh_args, build_remote_ssh_command_args,
    build_remote_ssh_forward_args, build_remote_ssh_reverse_forward_args,
    build_remote_streamable_agent_command,
};
use streamable_http::{STREAMABLE_HTTP_PATH, connect_streamable_http_endpoint};

fn managed_agent_spawn_command(command_path: &Path, args: &[String]) -> std::process::Command {
    #[cfg(windows)]
    {
        agent_spawn_command(command_path, args)
    }

    #[cfg(not(windows))]
    {
        parent_watchdog_agent_spawn_command(command_path, args, std::process::id())
    }
}

#[cfg(not(windows))]
fn parent_watchdog_agent_spawn_command(
    command_path: &Path,
    args: &[String],
    parent_pid: u32,
) -> std::process::Command {
    const SCRIPT: &str = r#"
parent_pid="$1"
shift
agent_pgrp="$$"
(
  trap '' TERM
  while kill -0 "$parent_pid" 2>/dev/null; do
    sleep 1
  done
  kill -TERM "-$agent_pgrp" 2>/dev/null || true
  sleep 1
  kill -KILL "-$agent_pgrp" 2>/dev/null || true
) &
watchdog_pid="$!"
"$@"
status="$?"
kill -KILL "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true
exit "$status"
"#;

    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(SCRIPT)
        .arg("kodex-agent-watchdog")
        .arg(parent_pid.to_string())
        .arg(command_path)
        .args(args);
    command
}

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
        let mut command = managed_agent_spawn_command(&self.command, &self.args);
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
        configure_agent_process_group(&mut command);

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
    remote_agent_pid_file: String,
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

struct RemoteAgentCleanupGuard {
    cleanup: Arc<RemoteAgentCleanup>,
    _shutdown_hook: Arc<ShutdownCleanupHook>,
}

#[derive(Debug)]
struct RemoteAgentCleanup {
    ssh_command: PathBuf,
    ssh_target: String,
    ssh_port: Option<u16>,
    ssh_password: Option<String>,
    remote_agent_pid_file: String,
    log_config: SessionConfig,
    did_run: AtomicBool,
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
            remote_agent_pid_file: format!(
                "/tmp/kodex-acp-agent-{}.pid",
                uuid::Uuid::new_v4().simple()
            ),
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
        let mut args = self.args.clone();
        args.extend(["--port".to_string(), self.port.to_string()]);
        let mut command = managed_agent_spawn_command(&self.command, &args);
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
        configure_agent_process_group(&mut command);

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
        let _remote_cleanup_guard = self.remote_cleanup_guard();

        let is_streamable_http = self.acp_transport != RemoteAcpTransport::Tcp;
        let remote_command = if is_streamable_http {
            build_remote_streamable_agent_command(
                &self.remote_workspace_root,
                &self.agent_command,
                &self.agent_args,
                &self.agent_env,
                &self.remote_agent_pid_file,
                self.remote_port,
            )
        } else {
            build_remote_agent_command(
                &self.remote_workspace_root,
                &self.agent_command,
                &self.agent_args,
                &self.agent_env,
                &self.remote_agent_pid_file,
                self.remote_port,
            )
        };
        let has_password = self.ssh_password.is_some();
        let uses_multiplex = false;
        let args = if is_streamable_http {
            build_remote_ssh_agent_command_args(
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
        let env_keys = self
            .agent_env
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        let _ = append_runtime_event_log(
            &self.log_config,
            "agent/remote_ssh_agent_start",
            &json!({
                "target": self.ssh_target,
                "sshPort": self.ssh_port,
                "transport": format!("{:?}", self.acp_transport),
                "localPort": self.local_port,
                "remotePort": self.remote_port,
                "hasPassword": has_password,
                "reverseForwards": self.reverse_forwards.len(),
                "usesMultiplex": uses_multiplex,
                "connectTimeoutSecs": REMOTE_SSH_CONNECT_TIMEOUT_SECS,
                "envKeys": env_keys,
                "codexHomeConfigured": self.agent_env.iter().any(|(name, _)| name == "CODEX_HOME"),
            }),
        );
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
                let _ = append_runtime_event_log(
                    &log_config,
                    "agent/remote_ssh_forward_start",
                    &json!({
                        "target": self.ssh_target,
                        "sshPort": self.ssh_port,
                        "localPort": self.local_port,
                        "remotePort": remote_port,
                        "hasPassword": has_password,
                        "usesMultiplex": false,
                        "connectTimeoutSecs": REMOTE_SSH_CONNECT_TIMEOUT_SECS,
                    }),
                );
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
    fn remote_cleanup_guard(&self) -> RemoteAgentCleanupGuard {
        let cleanup = Arc::new(RemoteAgentCleanup {
            ssh_command: self.ssh_command.clone(),
            ssh_target: self.ssh_target.clone(),
            ssh_port: self.ssh_port,
            ssh_password: self.ssh_password.clone(),
            remote_agent_pid_file: self.remote_agent_pid_file.clone(),
            log_config: self.log_config.clone(),
            did_run: AtomicBool::new(false),
        });
        let hook_cleanup = cleanup.clone();
        let shutdown_hook: Arc<ShutdownCleanupHook> = Arc::new(move || hook_cleanup.run_once());
        self.shutdown_signal.register_cleanup_hook(&shutdown_hook);
        if self.shutdown_signal.is_requested() {
            cleanup.run_once();
        }

        RemoteAgentCleanupGuard {
            cleanup,
            _shutdown_hook: shutdown_hook,
        }
    }

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

        if let Some(config) = log_config {
            let _ = append_runtime_event_log(
                config,
                "agent/reverse_forward_start",
                &json!({
                    "target": self.ssh_target,
                    "sshPort": self.ssh_port,
                    "remotePort": forward.remote_port,
                    "localPort": forward.local_port,
                    "hasPassword": has_password,
                    "usesMultiplex": false,
                    "connectTimeoutSecs": REMOTE_SSH_CONNECT_TIMEOUT_SECS,
                }),
            );
        }
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

impl Drop for RemoteAgentCleanupGuard {
    fn drop(&mut self) {
        self.cleanup.run_once();
    }
}

impl RemoteAgentCleanup {
    fn run_once(&self) {
        if self.did_run.swap(true, Ordering::AcqRel) {
            return;
        }

        let has_password = self.ssh_password.is_some();
        let remote_command = build_remote_agent_cleanup_command(&self.remote_agent_pid_file);
        let args = build_remote_ssh_agent_command_args(
            &self.ssh_target,
            self.ssh_port,
            remote_command,
            has_password,
        );
        let _ = append_runtime_event_log(
            &self.log_config,
            "agent/remote_cleanup_start",
            &json!({
                "target": self.ssh_target,
                "sshPort": self.ssh_port,
                "pidFile": self.remote_agent_pid_file,
                "hasPassword": has_password,
            }),
        );

        let mut command = agent_spawn_command(&self.ssh_command, &args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(password) = self.ssh_password.as_deref() {
            if let Err(error) = configure_ssh_askpass(&mut command, password) {
                let _ = append_runtime_event_log(
                    &self.log_config,
                    "agent/remote_cleanup_error",
                    &json!({ "error": error.to_string() }),
                );
                return;
            }
        }
        hide_console_window(&mut command);

        let result = command.status();
        match result {
            Ok(status) => {
                let _ = append_runtime_event_log(
                    &self.log_config,
                    "agent/remote_cleanup_exit",
                    &json!({ "status": status.to_string(), "success": status.success() }),
                );
            }
            Err(error) => {
                let _ = append_runtime_event_log(
                    &self.log_config,
                    "agent/remote_cleanup_error",
                    &json!({ "error": error.to_string() }),
                );
            }
        }
    }
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
