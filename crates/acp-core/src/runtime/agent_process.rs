use super::ShutdownSignal;
use super::process::{
    agent_spawn_command, apply_process_cwd_and_pwd, hide_console_window, parse_env_assignment,
};
use crate::codex_api_proxy::{
    configure_codex_api_proxy_model_provider_map, ensure_codex_api_proxy,
};
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::{Client, ConnectTo, Lines, Role};
use anyhow::anyhow;
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const REMOTE_AGENT_READY_MARKER: &str = "__KODEX_ACP_REMOTE_AGENT_READY__";
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

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
        let child_monitor =
            monitor_hidden_agent_child(child_guard.handle(), stderr_rx, log_config.clone());
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
    log_config: SessionConfig,
    shutdown_signal: ShutdownSignal,
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
            agent_args: parsed.args,
            agent_env,
            local_port: remote.local_port,
            remote_port: remote.remote_port,
            log_config: config.clone(),
            shutdown_signal: ShutdownSignal::default(),
        })
    }

    pub(super) fn shutdown_signal(mut self, shutdown_signal: ShutdownSignal) -> Self {
        self.shutdown_signal = shutdown_signal;
        self
    }
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
        let child_monitor =
            monitor_hidden_agent_child(child_guard.handle(), stderr_rx, Some(log_config.clone()));

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

        let remote_command = build_remote_agent_command(
            &self.remote_workspace_root,
            &self.agent_command,
            &self.agent_args,
            &self.agent_env,
            self.remote_port,
        );
        let args = build_remote_ssh_args(
            &self.ssh_target,
            self.ssh_port,
            self.local_port,
            self.remote_port,
            remote_command,
        );
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
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let live_stderr = Arc::new(Mutex::new(String::new()));
        let reader_live_stderr = live_stderr.clone();
        thread::spawn(move || {
            read_remote_ssh_stderr(child_stderr, stderr_tx, reader_live_stderr, ready_tx)
        });

        let log_config = self.log_config.clone();
        let child_guard = AgentChildGuard::new(child, self.shutdown_signal.clone());
        let child_monitor =
            monitor_hidden_agent_child(child_guard.handle(), stderr_rx, Some(log_config.clone()));

        let _child_guard = child_guard;

        let ready = wait_remote_agent_ready(ready_rx, live_stderr, Some(&log_config));
        tokio::pin!(ready);
        tokio::pin!(child_monitor);
        tokio::select! {
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

        let stream =
            connect_loopback_tcp(self.local_port, "remote SSH ACP forward", Some(&log_config))
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
        "if [ \"$kodex_i\" -ge 50 ]; then echo {} >&2; kill \"$kodex_agent_pid\" 2>/dev/null; wait \"$kodex_agent_pid\"; exit 124; fi;",
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
) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
    ];
    if let Some(ssh_port) = ssh_port {
        args.push("-p".to_string());
        args.push(ssh_port.to_string());
    }
    args.extend([ssh_target.to_string(), remote_command]);
    args
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
    ready_tx: tokio::sync::oneshot::Sender<()>,
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
                if line == REMOTE_AGENT_READY_MARKER {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(());
                    }
                    continue;
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

async fn wait_remote_agent_ready(
    ready_rx: tokio::sync::oneshot::Receiver<()>,
    live_stderr: Arc<Mutex<String>>,
    log_config: Option<&SessionConfig>,
) -> agent_client_protocol::Result<()> {
    match tokio::time::timeout(Duration::from_secs(10), ready_rx).await {
        Ok(Ok(())) => {
            if let Some(config) = log_config {
                let _ = append_runtime_event_log(
                    config,
                    "agent/remote_ready",
                    &json!({ "marker": REMOTE_AGENT_READY_MARKER }),
                );
            }
            Ok(())
        }
        Ok(Err(_)) => Err(agent_client_protocol::util::internal_error(
            "ssh remote agent process ended before readiness was reported",
        )),
        Err(_) => {
            let stderr = live_stderr
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            let message = if stderr.trim().is_empty() {
                "timed out waiting for remote ACP agent readiness".to_string()
            } else {
                format!("timed out waiting for remote ACP agent readiness: {stderr}")
            };
            Err(agent_client_protocol::util::internal_error(message))
        }
    }
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
        stderr_rx.try_recv().unwrap_or_default()
    };
    let payload = if stderr.is_empty() {
        json!({
            "success": success,
            "status": status.to_string(),
            "exitCode": status.code()
        })
    } else {
        json!({
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
