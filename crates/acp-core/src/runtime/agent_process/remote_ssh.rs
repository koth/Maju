use super::process_lifecycle::trim_line_ending;
use super::{RemoteAcpTransport, RemoteAgentReady};
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use serde_json::json;
#[cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub(in crate::runtime) const REMOTE_AGENT_READY_MARKER: &str = "__KODEX_ACP_REMOTE_AGENT_READY__";
const REMOTE_STREAMABLE_HTTP_ENDPOINT_MARKER: &str = "ACP streamable-http endpoint:";
const REMOTE_AGENT_LISTEN_ATTEMPTS: u16 = 150;
const REMOTE_AGENT_READY_TIMEOUT: Duration = Duration::from_secs(35);
const REMOTE_SSH_CONNECT_TIMEOUT_SECS: u64 = 5;
const KODEX_SSH_ASKPASS_ENV: &str = "KODEX_SSH_ASKPASS";
const KODEX_SSH_ASKPASS_PASSWORD_ENV: &str = "KODEX_SSH_ASKPASS_PASSWORD";

pub(in crate::runtime) fn build_remote_agent_command(
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

pub(in crate::runtime) fn build_remote_streamable_agent_command(
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

pub(in crate::runtime) fn build_remote_ssh_args(
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

pub(in crate::runtime) fn build_remote_ssh_command_args(
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

pub(in crate::runtime) fn build_remote_ssh_forward_args(
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

pub(in crate::runtime) fn build_remote_ssh_reverse_forward_args(
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

pub(super) fn configure_ssh_askpass(
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

pub(super) fn read_remote_ssh_stderr(
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

pub(super) fn remote_agent_ready_from_line(
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
pub(super) fn is_remote_agent_ready_line(line: &str, acp_transport: RemoteAcpTransport) -> bool {
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

pub(super) async fn wait_remote_agent_ready(
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
