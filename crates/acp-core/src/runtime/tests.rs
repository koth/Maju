use super::agent_process::{
    normalize_woa_remote_path, parse_git_config_codex_woa_repos_header, parse_git_config_woa_repos,
};
use super::process::{apply_process_cwd_and_pwd, process_cwd};
use super::prompt_content::prompt_title_text;
use super::session_titles::{
    advertised_session_list_capability, codex_woa_title_conversation_id, codex_woa_title_git_repos,
    codex_woa_title_payload, command_implies_codex_session_list,
    command_uses_codex_woa_side_query_titles, command_uses_codex_woa_titles,
    extract_codex_woa_title, extract_codex_woa_title_from_body, select_session_title_for_sync,
    supports_session_list_title_sync,
};
use super::*;
use agent_client_protocol::schema::{
    AgentCapabilities, SessionCapabilities, SessionId, SessionInfo, SessionListCapabilities,
};
use std::path::{Path, PathBuf};
use std::process::Command;
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
fn codex_woa_remote_path_normalizes_woa_git_urls() {
    assert_eq!(
        normalize_woa_remote_path(
            "https://git.woa.com/TechPlatform/MachineLearning/ArashiIconAIGenTool.git"
        ),
        Some("TechPlatform/MachineLearning/ArashiIconAIGenTool".into())
    );
    assert_eq!(
        normalize_woa_remote_path("git@git.woa.com:foo/bar.git"),
        Some("foo/bar".into())
    );
    assert_eq!(
        normalize_woa_remote_path("git@github.com:koth/Kodex.git"),
        None
    );
}

#[test]
fn codex_woa_git_config_parser_collects_woa_urls() {
    let content = r#"
[remote "origin"]
    url = https://git.woa.com/TechPlatform/MachineLearning/ArashiIconAIGenTool.git
[remote "github"]
    url = git@github.com:koth/Kodex.git
[submodule "reference/ArtWebBackend"]
    url = https://git.woa.com/TechPlatform/MachineLearning/ArtWebBackend
"#;

    assert_eq!(
        parse_git_config_woa_repos(content),
        vec![
            "TechPlatform/MachineLearning/ArashiIconAIGenTool".to_string(),
            "TechPlatform/MachineLearning/ArtWebBackend".to_string(),
        ]
    );
}

#[test]
fn codex_woa_git_repos_header_uses_fixed_header_for_github_repos() {
    let content = r#"
[remote "origin"]
    url = git@github.com:koth/Kodex.git
[submodule "codex-acp"]
    url = https://github.com/koth/kodex-acp.git
"#;

    assert_eq!(
        parse_git_config_codex_woa_repos_header(content),
        Some("TechPlatform/MachineLearning".to_string())
    );
}

#[test]
fn codex_woa_git_repos_header_prefers_real_woa_repos_over_github_fallback() {
    let content = r#"
[remote "origin"]
    url = git@github.com:koth/Kodex.git
[submodule "reference/ArtWebBackend"]
    url = https://git.woa.com/TechPlatform/MachineLearning/ArtWebBackend
"#;

    assert_eq!(
        parse_git_config_codex_woa_repos_header(content),
        Some("TechPlatform/MachineLearning/ArtWebBackend".to_string())
    );
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
fn codex_woa_agent_command_uses_native_title_and_session_list_fallback_by_default() {
    let mut config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);
    config
        .agent_env
        .push(("CODEX_WOA_API_KEY".into(), "access-token".into()));

    assert!(command_uses_codex_woa_titles(&config));
    assert!(!command_uses_codex_woa_side_query_titles(&config));
    assert!(supports_session_list_title_sync(&config, true));
    assert!(supports_session_list_title_sync(&config, false));
}

#[test]
fn codex_woa_title_side_query_requires_explicit_escape_hatch() {
    let mut config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\codex-acp.exe"#);
    config
        .agent_env
        .push(("CODEX_WOA_API_KEY".into(), "access-token".into()));
    config
        .agent_env
        .push(("KODEX_ENABLE_CODEX_WOA_TITLE_SIDE_QUERY".into(), "1".into()));

    assert!(command_uses_codex_woa_titles(&config));
    assert!(command_uses_codex_woa_side_query_titles(&config));
    assert!(supports_session_list_title_sync(&config, true));
    assert!(supports_session_list_title_sync(&config, false));
}

#[test]
fn codex_woa_title_query_reuses_configured_conversation_id() {
    let mut config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#);
    config.agent_env.push((
        "CODEX_INTERNAL_CONVERSATION_ID".into(),
        "conversation-from-agent-env".into(),
    ));

    assert_eq!(
        codex_woa_title_conversation_id(&config),
        "conversation-from-agent-env"
    );
}

#[test]
fn codex_woa_title_query_prefers_configured_git_repos_header() {
    let mut config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#);
    config.agent_env.push((
        "CODEX_INTERNAL_GIT_REPOS".into(),
        "TechPlatform/MachineLearning/ArashiIconAIGenTool".into(),
    ));

    assert_eq!(
        codex_woa_title_git_repos(&config),
        "TechPlatform/MachineLearning/ArashiIconAIGenTool"
    );
}

#[test]
fn codex_woa_title_query_uses_fixed_git_repos_fallback() {
    let config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#);

    assert_eq!(
        codex_woa_title_git_repos(&config),
        "TechPlatform/MachineLearning"
    );
}

#[test]
fn codex_woa_title_query_uses_codex_responses_payload_shape() {
    let mut config = test_session_config(r#"C:\Users\yvonchen\.kodex\bin\kodex-acp.exe"#);
    config.model = "gpt-5.4".into();
    let payload = codex_woa_title_payload(
        &config,
        "修复 WOA 标题生成",
        "session-123",
        "installation-456",
    );

    assert_eq!(payload["model"], "gpt-5.4");
    assert_eq!(payload["tool_choice"], "auto");
    assert_eq!(payload["stream"], true);
    assert_eq!(payload["prompt_cache_key"], "session-123");
    assert_eq!(
        payload["client_metadata"]["x-codex-installation-id"],
        "installation-456"
    );
    assert_eq!(payload["input"][0]["type"], "message");
    assert_eq!(payload["input"][0]["role"], "user");
    assert_eq!(payload["input"][0]["content"][0]["type"], "input_text");
    assert!(
        payload["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<user_request>")
    );
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
fn codex_woa_title_extraction_handles_responses_output() {
    let value = serde_json::json!({
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "\"检查 WOA 标题更新\""
            }]
        }]
    });

    assert_eq!(
        extract_codex_woa_title(&value),
        Some("检查 WOA 标题更新".into())
    );
}

#[test]
fn codex_woa_title_extraction_handles_streaming_response_body() {
    let body = [
        r#"event: response.output_text.delta"#,
        r#"data: {"type":"response.output_text.delta","delta":"修复"}"#,
        "",
        r#"event: response.output_text.delta"#,
        r#"data: {"type":"response.output_text.delta","delta":"标题生成"}"#,
        "",
        r#"data: [DONE]"#,
    ]
    .join("\n");

    assert_eq!(
        extract_codex_woa_title_from_body(&body),
        Some("修复标题生成".into())
    );
}

#[test]
fn codex_woa_title_extraction_handles_streaming_completed_event() {
    let body = r#"data: {"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"\"稳定标题请求\""}]}]}}"#;

    assert_eq!(
        extract_codex_woa_title_from_body(body),
        Some("稳定标题请求".into())
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
