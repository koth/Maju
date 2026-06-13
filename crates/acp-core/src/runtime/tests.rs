use super::agent_process::{
    RemoteSshAgentProcess, build_remote_agent_command, build_remote_ssh_args,
    connect_loopback_tcp_with_retry, connect_tcp_stream, kill_child_handle,
};
use super::process::{apply_process_cwd_and_pwd, process_cwd};
use super::prompt_content::prompt_title_text;
use super::session_titles::{
    advertised_session_list_capability, command_implies_codex_session_list,
    select_session_title_for_sync, supports_session_list_title_sync,
};
use super::*;
use crate::events::{AgentEditPolicy, RemoteSshSessionConfig, agent_edit_policy_for_command};
use agent_client_protocol::schema::{
    AgentCapabilities, SessionCapabilities, SessionId, SessionInfo, SessionListCapabilities,
};
use agent_client_protocol::{Channel, ConnectTo};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use workspace_model::UserPromptContent;

#[test]
fn hidden_agent_process_uses_workspace_as_current_dir() {
    let process =
        HiddenAgentProcess::from_command("CODEBUDDY_TEST=1 codebuddy.exe --acp", "D:/work/kodex")
            .unwrap();

    assert_eq!(process.command, PathBuf::from("codebuddy.exe"));
    assert_eq!(process.args, vec!["--acp"]);
    assert_eq!(process.env, vec![("CODEBUDDY_TEST".into(), "1".into())]);
    assert_eq!(process.current_dir, PathBuf::from("D:/work/kodex"));
}

#[test]
fn remote_agent_command_quotes_workspace_args_and_env() {
    let command = build_remote_agent_command(
        "/home/alice/project with space",
        Path::new("codex-acp"),
        &["--config".into(), "profile='dev'".into()],
        &[("TOKEN".into(), "a'b".into())],
        4567,
    );

    assert!(command.contains("cd '/home/alice/project with space' || exit $?;"));
    assert!(
        command.contains(
            "TOKEN='a'\\''b' 'codex-acp' '--config' 'profile='\\''dev'\\''' --port 4567 &"
        )
    );
    assert!(command.contains("KODEX_REMOTE_ACP_PORT_HEX='11D7';"));
    assert!(command.contains("if [ \"$kodex_i\" -ge 150 ];"));
    assert!(command.contains("__KODEX_ACP_REMOTE_AGENT_READY__"));
    assert!(command.contains("wait \"$kodex_agent_pid\""));
}

#[test]
fn remote_agent_command_accepts_bootstrapped_absolute_agent_path() {
    let command = build_remote_agent_command(
        "/srv/project",
        Path::new("/root/.kodex/remote-agents/codebuddy/current/bin/codebuddy"),
        &["--acp".into()],
        &[],
        4567,
    );

    assert!(command.contains("cd '/srv/project' || exit $?;"));
    assert!(command.contains(
        "'/root/.kodex/remote-agents/codebuddy/current/bin/codebuddy' '--acp' --port 4567 &"
    ));
    assert!(command.contains("trap 'kill \"$kodex_agent_pid\" 2>/dev/null'"));
    assert!(command.contains("__KODEX_ACP_REMOTE_AGENT_READY__"));
}

#[test]
fn remote_ssh_args_use_loopback_forwarding_and_exit_on_forward_failure() {
    let args = build_remote_ssh_args(
        "alice@devbox",
        None,
        3456,
        4567,
        "cd '/srv/project' && exec 'codex-acp' --port 4567".into(),
    );

    assert_eq!(
        args,
        vec![
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            "127.0.0.1:3456:127.0.0.1:4567",
            "alice@devbox",
            "cd '/srv/project' && exec 'codex-acp' --port 4567",
        ]
    );
}

#[test]
fn remote_ssh_args_include_custom_ssh_port_when_configured() {
    let args = build_remote_ssh_args(
        "alice@devbox",
        Some(2222),
        3456,
        4567,
        "cd '/srv/project' && exec 'codex-acp' --port 4567".into(),
    );

    assert_eq!(
        args,
        vec![
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            "127.0.0.1:3456:127.0.0.1:4567",
            "-p",
            "2222",
            "alice@devbox",
            "cd '/srv/project' && exec 'codex-acp' --port 4567",
        ]
    );
}

#[test]
fn remote_ssh_agent_process_waits_for_forward_and_finishes_after_tcp_close() {
    let dir = tempfile::tempdir().unwrap();
    let fake_ssh = create_fake_ssh_command(dir.path());
    let local_port = unused_loopback_port();
    let remote_port = unused_loopback_port();
    let config = SessionConfig {
        workspace_root: "/srv/project".into(),
        app_data_root: dir.path().display().to_string(),
        model: String::new(),
        agent_command: "mock-acp-agent".into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: String::new(),
        acp_port: local_port,
        remote_ssh: Some(RemoteSshSessionConfig {
            ssh_target: "fake-host".into(),
            ssh_port: None,
            remote_workspace_root: "/srv/project".into(),
            local_port,
            remote_port,
            ssh_command: Some(fake_ssh.display().to_string()),
            ssh_password: None,
        }),
    };
    let agent = RemoteSshAgentProcess::from_config(&config).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = runtime.block_on(async move {
        let (agent_side, client_side) = Channel::duplex();
        drop(agent_side);
        tokio::time::timeout(Duration::from_secs(5), agent.connect_to(client_side)).await
    });

    match result {
        Ok(protocol_result) => {
            assert!(
                protocol_result.is_ok(),
                "protocol result: {protocol_result:?}"
            );
        }
        Err(_) => panic!("remote SSH agent process did not finish"),
    }
}

#[test]
fn connect_loopback_tcp_reports_unavailable_endpoint_with_phase() {
    let port = unused_loopback_port();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let error = runtime
        .block_on(connect_loopback_tcp_with_retry(
            port,
            "test ACP endpoint",
            None,
            2,
            Duration::from_millis(5),
            Duration::from_millis(5),
        ))
        .unwrap_err();
    let message = error.to_string();

    assert!(message.contains("failed to connect to test ACP endpoint"));
    assert!(message.contains(&format!("127.0.0.1:{port}")));
}

#[test]
fn connect_loopback_tcp_waits_until_endpoint_becomes_reachable() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accept_thread = std::thread::spawn(move || {
        let _ = listener.accept();
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let stream = runtime
        .block_on(connect_loopback_tcp_with_retry(
            port,
            "ready ACP endpoint",
            None,
            2,
            Duration::from_millis(50),
            Duration::from_millis(5),
        ))
        .unwrap();

    drop(stream);
    accept_thread.join().unwrap();
}

#[test]
fn connect_tcp_stream_finishes_when_peer_drops_connection() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accept_thread = std::thread::spawn(move || {
        let _ = listener.accept();
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = runtime.block_on(async move {
        let stream = connect_loopback_tcp_with_retry(
            port,
            "dropping ACP endpoint",
            None,
            2,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .await?;
        let (agent_side, client_side) = Channel::duplex();
        drop(agent_side);
        let protocol = connect_tcp_stream(stream, client_side)?;
        tokio::time::timeout(Duration::from_secs(1), protocol)
            .await
            .map_err(|_| agent_client_protocol::util::internal_error("protocol did not finish"))?
    });

    accept_thread.join().unwrap();
    assert!(result.is_ok(), "protocol result: {result:?}");
}

#[test]
fn kill_child_handle_terminates_running_child() {
    let child = spawn_sleep_child();
    let child = Arc::new(Mutex::new(Some(child)));

    kill_child_handle(&child).unwrap();

    assert!(child.lock().unwrap().is_none());
}

fn unused_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_sleep_child() -> Child {
    #[cfg(windows)]
    {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .spawn()
            .unwrap()
    }

    #[cfg(not(windows))]
    {
        Command::new("sleep").arg("30").spawn().unwrap()
    }
}

fn create_fake_ssh_command(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let ps1 = dir.join("fake-ssh.ps1");
        std::fs::write(
            &ps1,
            r#"
$ErrorActionPreference = "Stop"
$forward = $null
for ($i = 0; $i -lt $args.Count; $i++) {
    if ($args[$i] -eq "-L") {
        $forward = $args[$i + 1]
        break
    }
}
if (-not $forward) {
    Write-Error "missing -L"
    exit 1
}
$parts = $forward -split ":"
$localPort = [int]$parts[1]
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse("127.0.0.1"), $localPort)
$listener.Start()
[Console]::Error.WriteLine("__KODEX_ACP_REMOTE_AGENT_READY__")
try {
    $client = $listener.AcceptTcpClient()
    $client.Close()
    Start-Sleep -Milliseconds 100
} finally {
    $listener.Stop()
}
"#,
        )
        .unwrap();
        let cmd = dir.join("fake-ssh.cmd");
        std::fs::write(
            &cmd,
            "@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0fake-ssh.ps1\" %*\r\n",
        )
        .unwrap();
        cmd
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-ssh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
forward=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-L" ]; then
    forward="$2"
    shift 2
  else
    shift
  fi
done
if [ -z "$forward" ]; then
  echo "missing -L" >&2
  exit 1
fi
local_port=$(printf "%s" "$forward" | cut -d: -f2)
python3 - "$local_port" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen(1)
print("__KODEX_ACP_REMOTE_AGENT_READY__", file=sys.stderr, flush=True)
conn, _ = server.accept()
conn.close()
server.close()
time.sleep(0.1)
PY
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        script
    }
}

#[test]
fn process_cwd_defaults_to_workspace_root() {
    assert_eq!(
        process_cwd("workspace-root", None),
        PathBuf::from("workspace-root")
    );
}

#[test]
fn process_cwd_resolves_relative_request_from_workspace_root() {
    assert_eq!(
        process_cwd("workspace-root", Some(Path::new("backend"))),
        PathBuf::from("workspace-root/backend")
    );
}

#[test]
fn process_context_sets_current_dir_and_pwd() {
    let cwd = PathBuf::from("workspace-root");
    let mut command = Command::new("codebuddy.exe");

    apply_process_cwd_and_pwd(&mut command, &cwd);

    assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
    let pwd = command
        .get_envs()
        .find(|(name, _)| name.to_string_lossy() == "PWD")
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().to_string());
    assert_eq!(pwd.as_deref(), Some("workspace-root"));
}

#[test]
fn permission_broker_delivers_pending_resolution() {
    let broker = PermissionBroker::default();
    let rx = broker.register("call-1".into()).unwrap();

    let delivered = broker
        .resolve("call-1", Some("allow".into()), None, None)
        .unwrap();

    assert!(delivered);
    let resolution = rx.recv_timeout(Duration::from_millis(50)).unwrap();
    assert_eq!(resolution.option_id.as_deref(), Some("allow"));
    assert_eq!(resolution.guidance, None);
}

#[test]
fn permission_broker_replays_early_resolution() {
    let broker = PermissionBroker::default();

    let delivered = broker
        .resolve(
            "call-1",
            Some("allowAll".into()),
            Some("  try a read-only command instead  ".into()),
            None,
        )
        .unwrap();
    let rx = broker.register("call-1".into()).unwrap();

    assert!(!delivered);
    let resolution = rx.recv_timeout(Duration::from_millis(50)).unwrap();
    assert_eq!(resolution.option_id.as_deref(), Some("allowAll"));
    assert_eq!(
        resolution.guidance.as_deref(),
        Some("try a read-only command instead")
    );
}

#[test]
fn permission_broker_cancel_clears_early_resolutions() {
    let broker = PermissionBroker::default();

    let delivered = broker
        .resolve("call-1", Some("allow".into()), None, None)
        .unwrap();
    broker.cancel_all().unwrap();
    let rx = broker.register("call-1".into()).unwrap();

    assert!(!delivered);
    assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
}

#[test]
fn advertised_session_list_capability_detects_initialize_capability() {
    let capabilities = AgentCapabilities::new()
        .session_capabilities(SessionCapabilities::new().list(SessionListCapabilities::new()));

    assert!(advertised_session_list_capability(&capabilities));
}

fn test_session_config(agent_command: &str) -> SessionConfig {
    SessionConfig {
        workspace_root: String::new(),
        app_data_root: String::new(),
        model: String::new(),
        agent_command: agent_command.into(),
        agent_env: Vec::new(),
        resume_session_id: None,
        log_id: String::new(),
        acp_port: 0,
        remote_ssh: None,
    }
}

#[test]
fn codex_agent_command_implies_session_list_support() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);

    assert!(command_implies_codex_session_list(&config));
}

#[test]
fn kodex_agent_command_implies_session_list_support() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#);

    assert!(command_implies_codex_session_list(&config));
}

#[test]
fn non_codex_agent_command_does_not_imply_session_list_support() {
    let config = test_session_config("codebuddy.exe --acp");

    assert!(!command_implies_codex_session_list(&config));
}

#[test]
fn codex_and_claude_acp_commands_prefer_apply_patch_edits() {
    for command in [
        r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#,
        r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#,
        r#"C:\Users\yvonchen\.kodex\bin\claude-agent-acp.exe"#,
        r#"C:\Users\yvonchen\.kodex\bin\claude-acp.exe"#,
    ] {
        assert_eq!(
            agent_edit_policy_for_command(command),
            AgentEditPolicy::PreferApplyPatch,
            "{command}",
        );
    }
}

#[test]
fn non_acp_commands_keep_default_edit_policy() {
    for command in ["codebuddy.exe --acp", "claude", "codex"] {
        assert_eq!(
            agent_edit_policy_for_command(command),
            AgentEditPolicy::None,
            "{command}",
        );
    }
}

#[test]
fn claude_agent_command_disables_session_list_title_sync() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\claude-agent-acp.exe"#);

    assert!(!supports_session_list_title_sync(&config, true));
}

#[test]
fn claude_acp_alias_command_disables_session_list_title_sync() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\claude-acp.exe"#);

    assert!(!supports_session_list_title_sync(&config, true));
}

#[test]
fn codex_agent_command_can_use_session_list_title_sync_without_advertising_it() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);

    assert!(supports_session_list_title_sync(&config, false));
}

#[test]
fn advertised_non_claude_agent_can_use_session_list_title_sync() {
    let config = test_session_config("codebuddy.exe --acp");

    assert!(supports_session_list_title_sync(&config, true));
}

#[test]
fn session_title_sync_prefers_exact_session_id() {
    let sessions = vec![
        SessionInfo::new("other", "D:/work/kodex").title("Other title"),
        SessionInfo::new("current", "D:/work/kodex").title(" Current title "),
    ];

    assert_eq!(
        select_session_title_for_sync(&sessions, &SessionId::from("current")),
        Some(("Current title".into(), "sessionId"))
    );
}

#[test]
fn session_title_sync_ignores_titles_from_other_sessions() {
    let sessions = vec![
        SessionInfo::new("sdk-session", "D:/work/kodex").title(" Claude summary "),
        SessionInfo::new("older", "D:/work/kodex"),
    ];

    assert_eq!(
        select_session_title_for_sync(&sessions, &SessionId::from("acp-session")),
        None
    );
}

#[test]
fn prompt_title_text_uses_text_blocks_only() {
    let prompt = vec![
        UserPromptContent::text("  修复登录标题  "),
        UserPromptContent::image("aW1hZ2U=", "image/png", Some("shot.png".into())),
        UserPromptContent::text("并更新测试"),
    ];

    assert_eq!(
        prompt_title_text(&prompt),
        Some("修复登录标题\n\n并更新测试".into())
    );
}
