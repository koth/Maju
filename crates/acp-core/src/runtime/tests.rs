use super::process::{apply_process_cwd_and_pwd, process_cwd};
use super::session_titles::{
    advertised_session_list_capability, command_implies_codex_session_list,
    select_session_title_for_sync, supports_session_list_title_sync,
};
use super::*;
use agent_client_protocol::schema::{
    AgentCapabilities, SessionCapabilities, SessionId, SessionInfo, SessionListCapabilities,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

    let delivered = broker.resolve("call-1", Some("allow".into())).unwrap();

    assert!(delivered);
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        Some("allow".into())
    );
}

#[test]
fn permission_broker_replays_early_resolution() {
    let broker = PermissionBroker::default();

    let delivered = broker.resolve("call-1", Some("allowAll".into())).unwrap();
    let rx = broker.register("call-1".into()).unwrap();

    assert!(!delivered);
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        Some("allowAll".into())
    );
}

#[test]
fn permission_broker_cancel_clears_early_resolutions() {
    let broker = PermissionBroker::default();

    let delivered = broker.resolve("call-1", Some("allow".into())).unwrap();
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
    }
}

#[test]
fn codex_agent_command_implies_session_list_support() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);

    assert!(command_implies_codex_session_list(&config));
}

#[test]
fn non_codex_agent_command_does_not_imply_session_list_support() {
    let config = test_session_config("codebuddy.exe --acp");

    assert!(!command_implies_codex_session_list(&config));
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
