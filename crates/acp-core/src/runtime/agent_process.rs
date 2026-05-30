use super::ShutdownSignal;
use super::process::{
    agent_spawn_command, apply_process_cwd_and_pwd, hide_console_window, parse_env_assignment,
};
use crate::codex_api_proxy::ensure_codex_api_proxy;
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::{Client, ConnectTo, Lines, Role};
use anyhow::anyhow;
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

const CODEX_WOA_API_KEY_ENVS: &[&str] = &["AUTH_TOKEN", "CODEX_WOA_API_KEY"];
const CODEX_WOA_GIT_REPOS_ENVS: &[&str] = &["CODEX_INTERNAL_GIT_REPOS", "CODEX_WOA_GIT_REPOS"];
const CODEX_WOA_GIT_REPOS_ENV: &str = "CODEX_INTERNAL_GIT_REPOS";
const CODEX_WOA_GITHUB_REPOS_HEADER: &str = "TechPlatform/MachineLearning";

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
        append_codex_woa_workspace_env(&mut process.env, Path::new(&config.workspace_root));
        let agent_command = config.agent_command.to_ascii_lowercase();
        if agent_command.contains("codex-acp") || agent_command.contains("kodex-acp") {
            for (env_key, provider) in [
                ("DEEPSEEK_API_KEY", "deepseek"),
                ("KIMI_CODE_API_KEY", "kimi_code"),
                ("XIAOMI_MIMO_API_KEY", "xiaomi_mimo"),
                ("VENUS_API_KEY", "venus"),
            ] {
                if let Some((_, api_key)) = process.env.iter().find(|(name, _)| name == env_key) {
                    ensure_codex_api_proxy(provider, api_key);
                }
            }
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

fn append_codex_woa_workspace_env(env: &mut Vec<(String, String)>, workspace_root: &Path) {
    if !CODEX_WOA_API_KEY_ENVS
        .iter()
        .any(|name| env_has_non_empty(env, name))
        || CODEX_WOA_GIT_REPOS_ENVS
            .iter()
            .any(|name| env_has_non_empty(env, name))
    {
        return;
    }
    let header = codex_woa_git_repos_header(workspace_root)
        .unwrap_or_else(|| codex_woa_default_git_repos_header().to_string());
    env.push((CODEX_WOA_GIT_REPOS_ENV.to_string(), header));
}

fn env_has_non_empty(env: &[(String, String)], name: &str) -> bool {
    env.iter()
        .any(|(key, value)| key == name && !value.trim().is_empty())
}

pub(super) fn codex_woa_git_repos_header(workspace_root: &Path) -> Option<String> {
    let config_path = find_git_config_path(workspace_root)?;
    let content = std::fs::read_to_string(config_path).ok()?;
    parse_git_config_codex_woa_repos_header(&content)
}

pub(super) fn codex_woa_default_git_repos_header() -> &'static str {
    CODEX_WOA_GITHUB_REPOS_HEADER
}

pub(super) fn parse_git_config_codex_woa_repos_header(content: &str) -> Option<String> {
    let repos = parse_git_config_woa_repos(&content);
    if !repos.is_empty() {
        return Some(repos.join(","));
    }
    git_config_has_github_remote(content).then(|| CODEX_WOA_GITHUB_REPOS_HEADER.to_string())
}

fn find_git_config_path(workspace_root: &Path) -> Option<PathBuf> {
    let mut current = workspace_root;
    loop {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("config"));
        }
        if dot_git.is_file() {
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let git_dir = content.trim().strip_prefix("gitdir:")?.trim();
            let git_dir = Path::new(git_dir);
            let git_dir = if git_dir.is_absolute() {
                git_dir.to_path_buf()
            } else {
                current.join(git_dir)
            };
            return Some(git_dir.join("config"));
        }
        current = current.parent()?;
    }
}

pub(super) fn parse_git_config_woa_repos(content: &str) -> Vec<String> {
    let mut repos = BTreeSet::new();

    for line in content.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "url" {
            continue;
        }
        if let Some(repo) = normalize_woa_remote_path(value) {
            repos.insert(repo);
        }
    }

    repos.into_iter().collect()
}

pub(super) fn normalize_woa_remote_path(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let remote = remote.strip_prefix("git+").unwrap_or(remote);
    let path = if let Some(rest) = remote.strip_prefix("git@git.woa.com:") {
        rest
    } else if let Some(rest) = remote.strip_prefix("ssh://git@git.woa.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("https://git.woa.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("http://git.woa.com/") {
        rest
    } else {
        return None;
    };
    let path = path.trim_start_matches('/').trim_end_matches(".git").trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn git_config_has_github_remote(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == "url" && remote_is_github(value)
    })
}

fn remote_is_github(remote: &str) -> bool {
    let remote = remote.trim();
    let remote = remote.strip_prefix("git+").unwrap_or(remote);
    remote.starts_with("git@github.com:")
        || remote.starts_with("ssh://git@github.com/")
        || remote.starts_with("https://github.com/")
        || remote.starts_with("http://github.com/")
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
        append_codex_woa_workspace_env(&mut env, Path::new(&config.workspace_root));
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

/// Wrapper that dispatches to either stdio or TCP transport.
pub(super) enum AgentTransport {
    Stdio(HiddenAgentProcess),
    Tcp(TcpAgentProcess),
}

impl ConnectTo<Client> for AgentTransport {
    async fn connect_to(
        self,
        client: impl ConnectTo<<Client as Role>::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        match self {
            AgentTransport::Stdio(agent) => agent.connect_to(client).await,
            AgentTransport::Tcp(agent) => agent.connect_to(client).await,
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

        // Wait for the agent to start listening on the port.
        let addr: std::net::SocketAddr =
            format!("127.0.0.1:{}", self.port).parse().map_err(|e| {
                agent_client_protocol::util::internal_error(format!("invalid address: {e}"))
            })?;

        let stream = {
            let mut last_err = None;
            let mut connected = None;
            for attempt in 0..50 {
                match std::net::TcpStream::connect_timeout(
                    &addr,
                    std::time::Duration::from_millis(200),
                ) {
                    Ok(s) => {
                        connected = Some(s);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if attempt < 49 {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                }
            }
            connected.ok_or_else(|| {
                agent_client_protocol::util::internal_error(format!(
                    "failed to connect to agent at 127.0.0.1:{}: {}",
                    self.port,
                    last_err.map(|e| e.to_string()).unwrap_or_default()
                ))
            })?
        };

        stream.set_nonblocking(true).map_err(|e| {
            agent_client_protocol::util::internal_error(format!("set_nonblocking: {e}"))
        })?;

        let tcp_stream = tokio::net::TcpStream::from_std(stream).map_err(|e| {
            agent_client_protocol::util::internal_error(format!("TcpStream::from_std: {e}"))
        })?;

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
        let outgoing_sink =
            futures::sink::unfold(writer, |mut writer, line: String| async move {
                use tokio::io::AsyncWriteExt;
                writer.write_all(line.as_bytes()).await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                writer.write_all(b"\n").await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                writer.flush().await.map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                })?;
                Ok::<_, std::io::Error>(writer)
            });

        let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
            Lines::new(outgoing_sink, incoming_lines),
            client,
        );

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
