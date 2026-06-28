use super::*;
use crate::remote_ssh::{RemoteSshCommand, RemoteSshCommandRunner};
use serde_json::json;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use workspace_model::{
    CustomProviderInput, CustomProviderProtocol, LspServerConfigInput, RemoteMachineProfile,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set_path(value: OsString) -> Self {
        let original = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", value);
        }
        Self {
            key: "PATH",
            original,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[derive(Clone)]
struct FakeRemoteSettingsRunner {
    outputs: Arc<Mutex<Vec<crate::remote_ssh::RemoteSshOutput>>>,
    commands: Arc<Mutex<Vec<RemoteSshCommand>>>,
}

impl FakeRemoteSettingsRunner {
    fn new(outputs: Vec<crate::remote_ssh::RemoteSshOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs)),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn commands(&self) -> Vec<RemoteSshCommand> {
        self.commands.lock().unwrap().clone()
    }
}

impl RemoteSshCommandRunner for FakeRemoteSettingsRunner {
    fn run_ssh_command(&self, command: &RemoteSshCommand) -> crate::remote_ssh::RemoteSshOutput {
        self.commands.lock().unwrap().push(command.clone());
        self.outputs.lock().unwrap().remove(0)
    }
}

fn remote_settings_output(settings: &AppSettings) -> crate::remote_ssh::RemoteSshOutput {
    crate::remote_ssh::RemoteSshOutput {
        success: true,
        stdout: serde_json::to_string(&json!({
            "home": "/root",
            "files": {
                "config/settings.json": serde_json::to_string(settings).unwrap(),
                "config/provider-secrets.json": null,
                "config/provider-models.json": null,
                "config.toml": null,
            },
            "agents": {
                "claude-agent-acp": "/usr/local/bin/claude-agent-acp",
                "codex-acp": null,
                "codebuddy": null,
            },
            "env_override": "remote-agent --acp",
            "token_status": {
                "exists": true,
                "malformed": false,
                "access_token": "acce...alue",
                "refresh_token": "refr...alue",
                "expires_at": "1700000000000",
                "valid_for_minutes": 10,
                "refresh_needed": false,
                "message": null,
            },
        }))
        .unwrap(),
        stderr: String::new(),
        timed_out: false,
        elapsed_ms: 1,
    }
}

fn remote_settings_ok_output() -> crate::remote_ssh::RemoteSshOutput {
    crate::remote_ssh::RemoteSshOutput {
        success: true,
        stdout: "{\"ok\":true}".into(),
        stderr: String::new(),
        timed_out: false,
        elapsed_ms: 1,
    }
}

fn remote_settings_profile() -> RemoteMachineProfile {
    RemoteMachineProfile {
        id: uuid::Uuid::new_v4(),
        display_name: "Devbox".into(),
        ssh_target: "root@devbox".into(),
        ssh_port: Some(36000),
        created_at_ms: 1,
        updated_at_ms: 1,
        last_validation: None,
    }
}

#[test]
fn remote_settings_snapshot_reads_remote_files_and_agents() {
    let mut settings = default_settings();
    settings.selected_agent = AgentCliId::ClaudeAgentAcp;
    let runner = FakeRemoteSettingsRunner::new(vec![remote_settings_output(&settings)]);

    let snapshot = remote_settings_snapshot_with_runner(
        &remote_settings_profile(),
        Some("ssh-secret"),
        &runner,
    )
    .unwrap();

    assert_eq!(snapshot.env_override.as_deref(), Some("remote-agent --acp"));
    let claude = snapshot
        .agents
        .iter()
        .find(|agent| agent.id == AgentCliId::ClaudeAgentAcp)
        .unwrap();
    assert!(claude.installed);
    assert_eq!(
        claude.detected_path.as_deref(),
        Some(Path::new("/usr/local/bin/claude-agent-acp"))
    );
    let codex = snapshot
        .agents
        .iter()
        .find(|agent| agent.id == AgentCliId::CodexAcp)
        .unwrap();
    assert!(!codex.installed);
    assert_eq!(
        runner.commands()[0].ssh_password.as_deref(),
        Some("ssh-secret")
    );
}

#[test]
fn remote_select_agent_rejects_missing_remote_binary() {
    let settings = default_settings();
    let runner = FakeRemoteSettingsRunner::new(vec![remote_settings_output(&settings)]);

    let error = remote_select_agent_with_runner(
        &remote_settings_profile(),
        Some("ssh-secret"),
        AgentCliId::CodexAcp,
        &runner,
    )
    .unwrap_err();

    assert!(error.to_string().contains("codex-acp is not installed"));
}

#[test]
fn remote_settings_update_pushes_mutated_kodex_files() {
    let settings = default_settings();
    let runner = FakeRemoteSettingsRunner::new(vec![
        remote_settings_output(&settings),
        remote_settings_ok_output(),
        remote_settings_output(&settings),
    ]);

    let _snapshot = remote_update_settings_with_runner(
        &remote_settings_profile(),
        Some("ssh-secret"),
        &runner,
        |paths| {
            save_agent_provider_secret(
                paths,
                AgentProviderFamily::Claude,
                TIMIAI_PROVIDER_ID,
                "remote-secret",
            )
        },
    )
    .unwrap();

    let calls = runner.commands();
    assert_eq!(calls.len(), 3);
    let import_stdin = calls[1].stdin.as_ref().expect("import stdin");
    let import_payload: serde_json::Value = serde_json::from_slice(import_stdin).unwrap();
    let provider_secrets = import_payload["files"]["config/provider-secrets.json"]
        .as_str()
        .unwrap();
    assert!(provider_secrets.contains("remote-secret"));
    assert!(!calls[1].remote_command.contains("remote-secret"));
}

#[test]
fn missing_settings_default_to_claude_byok() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let settings = load_app_settings(&paths);

    assert_eq!(settings.selected_agent, AgentCliId::CodexAcp);
    assert_eq!(settings.theme, AppTheme::Graphite);
    assert_eq!(
        settings.selected_claude_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
    assert!(settings.claude.available_models.is_empty());
    assert!(!settings.web_tools.enabled);
    assert_eq!(settings.web_tools.provider, WEB_TOOLS_PROVIDER_BRAVE);
}

#[test]
fn settings_round_trip() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    let settings = AppSettings {
        selected_agent: AgentCliId::Codebuddy,
        acp_port: 0,
        theme: AppTheme::Midnight,
        lsp_servers: BTreeMap::new(),
        codex_connection_mode: CodexConnectionMode::Managed,
        selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
        selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
        claude: ClaudeProviderSettings {
            available_models: default_claude_available_models(),
            ..ClaudeProviderSettings::default()
        },
        web_tools: WebToolsSettings {
            enabled: true,
            provider: WEB_TOOLS_PROVIDER_BRAVE.to_string(),
        },
        image: ImageSettings::default(),
    };

    save_app_settings(&paths, &settings).unwrap();
    let loaded = load_app_settings(&paths);

    assert_eq!(loaded, settings);
}

#[test]
fn legacy_settings_without_web_tools_default_to_disabled() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(
        settings_path(&paths),
        r#"{
  "selected_agent": "claude-agent-acp",
  "acp_port": 0,
  "theme": "graphite",
  "lsp_servers": {},
  "codex_connection_mode": "managed",
  "selected_codex_provider_profile_id": null,
  "selected_claude_provider_profile_id": "byok",
  "claude": {"available_models": [], "fast_model": null}
}"#,
    )
    .unwrap();

    let settings = load_app_settings(&paths);

    assert!(!settings.web_tools.enabled);
    assert_eq!(settings.web_tools.provider, WEB_TOOLS_PROVIDER_BRAVE);
}

#[test]
fn web_tools_settings_and_secret_update_snapshot() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_web_tools_settings(&paths, true, WEB_TOOLS_PROVIDER_BRAVE).unwrap();
    assert!(snapshot.settings.web_tools.enabled);
    assert_eq!(snapshot.web_tools.provider, WEB_TOOLS_PROVIDER_BRAVE);
    assert!(!snapshot.web_tools.configured);

    let snapshot =
        save_web_tools_provider_key(&paths, WEB_TOOLS_PROVIDER_BRAVE, "brave-secret").unwrap();
    assert!(snapshot.web_tools.enabled);
    assert!(snapshot.web_tools.configured);
    assert_eq!(
        web_tools_provider_secret(&paths, WEB_TOOLS_PROVIDER_BRAVE).as_deref(),
        Some("brave-secret")
    );

    let content = std::fs::read_to_string(settings_path(&paths)).unwrap();
    assert!(!content.contains("brave-secret"));
}

#[test]
fn web_tools_tavily_secret_is_stored_per_provider() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_web_tools_settings(&paths, true, WEB_TOOLS_PROVIDER_TAVILY).unwrap();
    assert!(snapshot.settings.web_tools.enabled);
    assert_eq!(snapshot.web_tools.provider, WEB_TOOLS_PROVIDER_TAVILY);
    assert!(!snapshot.web_tools.configured);

    let snapshot =
        save_web_tools_provider_key(&paths, WEB_TOOLS_PROVIDER_TAVILY, "tvly-secret").unwrap();
    assert_eq!(snapshot.web_tools.provider, WEB_TOOLS_PROVIDER_TAVILY);
    assert!(snapshot.web_tools.configured);
    assert_eq!(
        web_tools_provider_secret(&paths, WEB_TOOLS_PROVIDER_TAVILY).as_deref(),
        Some("tvly-secret")
    );
    assert_eq!(
        web_tools_provider_secret(&paths, WEB_TOOLS_PROVIDER_BRAVE).as_deref(),
        None
    );

    let snapshot = save_web_tools_settings(&paths, true, WEB_TOOLS_PROVIDER_BRAVE).unwrap();
    assert_eq!(snapshot.web_tools.provider, WEB_TOOLS_PROVIDER_BRAVE);
    assert!(!snapshot.web_tools.configured);
}

#[test]
fn legacy_goose_selection_migrates_to_codebuddy_when_codex_is_missing() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    let settings = AppSettings {
        selected_agent: AgentCliId::Goose,
        acp_port: 0,
        theme: AppTheme::Midnight,
        lsp_servers: BTreeMap::new(),
        codex_connection_mode: CodexConnectionMode::Managed,
        selected_codex_provider_profile_id: None,
        selected_claude_provider_profile_id: None,
        claude: ClaudeProviderSettings::default(),
        web_tools: WebToolsSettings::default(),
        image: ImageSettings::default(),
    };

    save_app_settings(&paths, &settings).unwrap();
    let loaded = load_app_settings(&paths);

    assert_eq!(loaded.selected_agent, AgentCliId::Codebuddy);
    assert_eq!(
        loaded.selected_codex_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
    assert_eq!(
        loaded.selected_claude_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
}

#[test]
fn invalid_saved_provider_profiles_migrate_to_supported_values() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    paths.ensure_root().unwrap();
    std::fs::write(
        codex_config_path(&paths),
        r#"
model = "gpt-5.5"
model_provider = "timiai"
"#,
    )
    .unwrap();
    let settings = AppSettings {
        selected_agent: AgentCliId::ClaudeAgentAcp,
        acp_port: 0,
        theme: AppTheme::Midnight,
        lsp_servers: BTreeMap::new(),
        codex_connection_mode: CodexConnectionMode::Managed,
        selected_codex_provider_profile_id: Some("woa".to_string()),
        selected_claude_provider_profile_id: Some("legacy-claude".to_string()),
        claude: ClaudeProviderSettings::default(),
        web_tools: WebToolsSettings::default(),
        image: ImageSettings::default(),
    };

    save_app_settings(&paths, &settings).unwrap();
    let loaded = load_app_settings(&paths);

    assert_eq!(
        loaded.selected_codex_provider_profile_id.as_deref(),
        Some(TIMIAI_PROVIDER_ID)
    );
    assert_eq!(
        loaded.selected_claude_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
}

#[test]
fn settings_snapshot_omits_goose_and_lists_provider_profiles() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = settings_snapshot(&paths);

    assert!(
        !snapshot
            .agents
            .iter()
            .any(|agent| agent.id == AgentCliId::Goose)
    );
    assert_eq!(
        snapshot.agents.first().map(|agent| agent.id),
        Some(AgentCliId::CodexAcp)
    );
    assert!(
        snapshot
            .agents
            .first()
            .map(|agent| agent.selected)
            .unwrap_or(false)
    );
    assert_eq!(snapshot.claude.selected_profile_id, BYOK_PROVIDER_ID);
    assert!(snapshot.codex_acp.profiles.len() >= 7);
    assert!(snapshot.claude.profiles.len() >= 5);
    assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
        profile.id == TIMIAI_PROVIDER_ID
            && profile.label == TIMIAI_PROVIDER_NAME
            && profile.proxy_kind == AgentProviderProxyKind::Responses
            && profile.base_url.as_deref() == Some(TIMIAI_BASE_URL)
    }));
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == TIMIAI_PROVIDER_ID
            && profile.label == TIMIAI_PROVIDER_NAME
            && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
            && profile.base_url.as_deref() == Some(TIMIAI_BASE_URL)
    }));
    assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
        profile.id == COMMANDCODE_PROVIDER_ID
            && profile.label == COMMANDCODE_PROVIDER_NAME
            && profile.proxy_kind == AgentProviderProxyKind::CompletionToResponses
            && profile.base_url.as_deref() == Some(COMMANDCODE_BASE_URL)
            && profile.default_model.as_deref() == Some(COMMANDCODE_MODEL)
    }));
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == COMMANDCODE_PROVIDER_ID
            && profile.label == COMMANDCODE_PROVIDER_NAME
            && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
            && profile.base_url.as_deref() == Some(COMMANDCODE_BASE_URL)
    }));
    assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
        profile.id == KIMI_PROVIDER_ID
            && profile.proxy_kind == AgentProviderProxyKind::CompletionToResponses
            && profile.default_model.as_deref() == Some(KIMI_MODEL)
    }));
    assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
        profile.id == MIMO_PROVIDER_ID
            && profile.label == MIMO_PROVIDER_NAME
            && profile.proxy_kind == AgentProviderProxyKind::CompletionToResponses
            && profile.base_url.as_deref() == Some(MIMO_OPENAI_BASE_URL)
            && profile
                .models
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                == MIMO_CATALOG_MODELS
    }));
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == KIMI_PROVIDER_ID
            && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
            && profile.base_url.as_deref() == Some(KIMI_CODE_ANTHROPIC_BASE_URL)
    }));
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == MIMO_PROVIDER_ID
            && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
            && profile.base_url.as_deref() == Some(MIMO_ANTHROPIC_BASE_URL)
    }));
}

#[test]
fn unsupported_provider_profile_is_rejected() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let error =
        select_agent_provider_profile(&paths, AgentProviderFamily::Codex, "missing").unwrap_err();

    assert!(error.to_string().contains("Unsupported provider profile"));
}

#[test]
fn invalid_settings_default_to_claude_byok() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(settings_path(&paths), "not json").unwrap();

    let settings = load_app_settings(&paths);

    assert_eq!(settings.selected_agent, AgentCliId::CodexAcp);
    assert_eq!(
        settings.selected_claude_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
    assert_eq!(settings.theme, AppTheme::Graphite);
}

#[test]
fn command_for_agent_uses_selected_binary_name() {
    let codebuddy = command_for_agent(AgentCliId::Codebuddy).unwrap();
    let codex_acp = command_for_agent(AgentCliId::CodexAcp).unwrap();
    let claude_agent_acp = command_for_agent(AgentCliId::ClaudeAgentAcp).unwrap();

    assert!(codebuddy.to_lowercase().contains("codebuddy"));
    assert!(codex_acp.to_lowercase().contains("codex-acp"));
    assert!(claude_agent_acp.to_lowercase().contains("claude-agent-acp"));
    assert!(codebuddy.ends_with(" --acp"));
    assert!(!codex_acp.ends_with(' '));
    assert!(command_for_agent(AgentCliId::Goose).is_none());
}

#[test]
fn remote_linux_command_for_agent_uses_remote_binary_names() {
    assert_eq!(
        remote_linux_command_for_agent(AgentCliId::Codebuddy).unwrap(),
        "codebuddy --acp"
    );
    assert_eq!(
        remote_linux_command_for_agent(AgentCliId::CodexAcp).unwrap(),
        "codex-acp"
    );
    assert_eq!(
        remote_linux_command_for_agent(AgentCliId::ClaudeAgentAcp).unwrap(),
        "claude-agent-acp"
    );
    assert!(remote_linux_command_for_agent(AgentCliId::Goose).is_none());
}

#[test]
fn remote_codex_home_derives_managed_home_from_remote_home() {
    assert_eq!(
        remote_codex_home(" /home/koth/ "),
        Some("/home/koth/.kodex".to_string())
    );
    assert_eq!(remote_codex_home("/"), Some("/.kodex".to_string()));
    assert_eq!(remote_codex_home("   "), None);
}

#[test]
fn command_for_agent_label_resolves_persisted_labels() {
    let codebuddy = command_for_agent_label("CodeBuddy").unwrap();
    let codex_acp = command_for_agent_label("codex-acp").unwrap();
    let claude_agent_acp = command_for_agent_label("Claude").unwrap();

    assert!(codebuddy.to_lowercase().contains("codebuddy"));
    assert!(codex_acp.to_lowercase().contains("codex-acp"));
    assert!(claude_agent_acp.to_lowercase().contains("claude-agent-acp"));
    assert!(command_for_agent_label("goose").is_none());
}

#[test]
fn default_agent_for_new_work_uses_codebuddy_when_claude_byok_is_unconfigured() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(bin_dir.join(binary_name("codebuddy")), "fake").unwrap();
    let _path_guard = EnvVarGuard::set_path(std::env::join_paths([bin_dir.as_os_str()]).unwrap());
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    // Default agent is now CodexAcp when neither agent is configured.
    // With only a codebuddy binary present and no keys saved, CodexAcp
    // is the default and its command resolves.
    assert_eq!(
        default_agent_for_new_work(&paths),
        AgentCliId::CodexAcp
    );
}

#[test]
fn default_agent_for_new_work_keeps_configured_claude_byok() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(bin_dir.join(binary_name("codebuddy")), "fake").unwrap();
    let _path_guard = EnvVarGuard::set_path(std::env::join_paths([bin_dir.as_os_str()]).unwrap());
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        MIMO_PROVIDER_ID,
        "mimo-secret",
    )
    .unwrap();

    // The default agent is now CodexAcp; saving a Claude secret should
    // not change the selected agent.
    assert_eq!(
        default_agent_for_new_work(&paths),
        AgentCliId::CodexAcp
    );
}

#[test]
fn codex_acp_command_includes_codex_home_env_when_paths_are_known() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let binary_path = codex_acp_binary_path(&paths);

    assert!(command.starts_with("CODEX_HOME="));
    assert!(command.contains(&paths.root().to_string_lossy().to_string()));
    assert!(command.contains(&binary_path.to_string_lossy().to_string()));
    assert!(command.contains(&paths.root().join("bin").to_string_lossy().to_string()));
}

#[test]
fn codex_default_mode_uses_user_codex_home_and_no_agent_env() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    select_codex_default_mode(&paths).unwrap();
    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);
    let status = codex_acp_settings_status(&paths);

    assert!(!command.starts_with("CODEX_HOME="));
    assert!(command.contains("codex-acp"));
    assert!(env.is_empty());
    assert_eq!(status.provider, "default");
    assert_eq!(status.connection_mode, CodexConnectionMode::Default);
}

#[test]
fn saving_codex_provider_key_switches_back_to_managed_mode() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    select_codex_default_mode(&paths).unwrap();
    save_codex_acp_provider_key(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let status = codex_acp_settings_status(&paths);

    assert!(command.starts_with("CODEX_HOME="));
    assert_eq!(status.provider, BYOK_PROVIDER_ID);
    assert_eq!(status.connection_mode, CodexConnectionMode::Managed);
}

#[test]
fn codex_acp_detection_only_uses_kodex_bin() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let missing = detect_agent_with_paths(&paths, AgentCliId::CodexAcp);
    assert!(!missing.installed);
    assert_eq!(missing.detected_path, None);

    let binary_path = codex_acp_binary_path(&paths);
    std::fs::create_dir_all(binary_path.parent().unwrap()).unwrap();
    std::fs::write(&binary_path, "fake").unwrap();

    let installed = detect_agent_with_paths(&paths, AgentCliId::CodexAcp);
    assert!(installed.installed);
    assert_eq!(
        installed.detected_path.as_deref(),
        Some(binary_path.as_path())
    );
}

#[test]
fn selected_codex_acp_resolves_with_codex_home_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let original = std::env::var("ACP_AGENT_COMMAND").ok();
    unsafe {
        std::env::remove_var("ACP_AGENT_COMMAND");
    }
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();

    let command = resolve_agent_command_with_settings(&paths);

    match original {
        Some(value) => unsafe { std::env::set_var("ACP_AGENT_COMMAND", value) },
        None => unsafe { std::env::remove_var("ACP_AGENT_COMMAND") },
    }
    assert!(command.starts_with("CODEX_HOME="));
    assert!(command.contains(&paths.root().to_string_lossy().to_string()));
    assert!(command.contains("codex-acp"));
}

#[test]
fn codex_acp_timiai_source_creates_byok_config_file() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret").unwrap();

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    assert!(content.contains("[model_providers.timiai]"));
    assert!(!content.contains("model_providers = {"));
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(doc["model"].as_str(), Some(byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str()));
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["preferred_auth_method"].as_str(),
        Some(CODEX_AUTH_METHOD_API_KEY)
    );
    // `model_context_window` is intentionally omitted so Codex resolves the
    // window per-model from `model_catalog_json` instead of clamping every
    // model to the launch model's window.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens(TIMIAI_CODEX_MODEL))
    );
    assert_eq!(
        doc["model_reasoning_effort"].as_str(),
        Some(CODEX_REASONING_EFFORT_NONE)
    );
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["name"].as_str(),
        Some(TIMIAI_PROVIDER_NAME)
    );
    assert!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["base_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("http://127.0.0.1:")
    );
    assert_eq!(
        doc["model_catalog_json"].as_str(),
        Some(codex_model_catalog_path(&paths).to_string_lossy().as_ref())
    );
    assert!(codex_model_catalog_path(&paths).is_file());
    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    assert!(catalog.contains("You are Codex, a coding agent"));
    assert!(catalog.contains("you MUST use the apply_patch tool"));
    assert!(catalog.contains("Never use shell redirection, rm, mv, cp, sed -i"));
    assert!(!catalog.contains("{{ base_instructions }}"));
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    for model in catalog["models"].as_array().unwrap() {
        let display_name = model["display_name"].as_str().unwrap();
        assert_eq!(
            model["max_output_tokens"].as_i64(),
            Some(model_max_output_tokens(display_name))
        );
    }
    assert!(catalog["models"].as_array().unwrap().iter().any(|model| {
        model["display_name"].as_str() == Some(TIMIAI_CODEX_MODEL)
            && model["_meta"]["source_provider"].as_str() == Some(TIMIAI_PROVIDER_ID)
    }));
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["wire_api"].as_str(),
        Some(CODEX_PROXY_WIRE_API)
    );
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["env_key"].as_str(),
        Some(TIMIAI_API_KEY_ENV)
    );
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["api_key"].as_str(),
        Some("timiai-secret")
    );
    let status = codex_acp_settings_status(&paths);
    assert_eq!(status.provider, BYOK_PROVIDER_ID);
    assert!(!status.deepseek_key_configured);
}

#[test]
fn remote_codex_proxy_config_strips_local_only_paths() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(TIMIAI_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();
    write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret").unwrap();

    let config_path = codex_config_path(&paths);
    let mut doc = std::fs::read_to_string(&config_path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    doc["model_catalog_json"] = value("/Users/kothchen/.kodex/model_catalog.json");
    doc["model_instructions_file"] = value("/Users/kothchen/.kodex/instructions.md");
    doc["experimental_instructions_file"] = value("/Users/kothchen/.kodex/legacy.md");
    doc["profiles"]["default"]["model_instructions_file"] =
        value("/Users/kothchen/.kodex/profile-instructions.md");
    std::fs::write(&config_path, doc.to_string()).unwrap();

    let remote_config = remote_codex_proxy_config(&paths, Some("/home/koth/.kodex"))
        .unwrap()
        .unwrap();
    let remote_doc = remote_config.parse::<DocumentMut>().unwrap();

    assert_eq!(
        remote_doc["model_catalog_json"].as_str(),
        Some("/home/koth/.kodex/model_catalog.json")
    );
    assert!(remote_doc.get("model_instructions_file").is_none());
    assert!(remote_doc.get("experimental_instructions_file").is_none());
    assert_eq!(
        remote_doc["model_providers"][TIMIAI_PROVIDER_ID]["api_key"].as_str(),
        Some("kodex-proxy")
    );
    assert!(
        remote_doc["profiles"]["default"]
            .as_table_like()
            .is_none_or(|profile| profile.get("model_instructions_file").is_none())
    );
}

#[test]
fn remote_codex_model_catalog_content_includes_byok_provider_models() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        KIMI_PROVIDER_ID,
        "kimi-secret",
    )
    .unwrap();
    write_codex_acp_provider_config(&paths, BYOK_PROVIDER_ID, "byok").unwrap();

    let catalog = remote_codex_model_catalog_content(&paths).unwrap().unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();

    assert!(catalog["models"].as_array().unwrap().iter().any(|model| {
        model["slug"].as_str()
            == Some(byok_encoded_model_slug(KIMI_MODEL, KIMI_PROVIDER_ID).as_str())
            && model["_meta"]["source_provider"].as_str() == Some(KIMI_PROVIDER_ID)
    }));
}

#[test]
fn remote_codex_byok_env_starts_local_proxy_before_scrubbing_keys() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        KIMI_PROVIDER_ID,
        "kimi-secret",
    )
    .unwrap();
    write_codex_acp_provider_config(&paths, BYOK_PROVIDER_ID, "byok").unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = remote_agent_env_for_command(&command, &paths, Some("/home/koth"));

    assert!(env.contains(&(KIMI_API_KEY_ENV.to_string(), "kodex-proxy".to_string())));
    assert!(!env.iter().any(|(_, value)| value == "kimi-secret"));
    let base_url = acp_core::codex_api_proxy_base_url();
    let port = base_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.split('/').next())
        .and_then(|port| port.parse::<u16>().ok())
        .expect("proxy base URL should include a local port");
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(500),
    )
    .expect("remote env preparation should start the local proxy");
}

#[test]
fn codex_acp_deepseek_config_creates_provider_config() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    assert!(content.contains("[model_providers.deepseek]"));
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(doc["model"].as_str(), Some(byok_encoded_model_slug(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID).as_str()));
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens(DEEPSEEK_MODEL))
    );
    assert_eq!(
        doc["model_providers"][DEEPSEEK_PROVIDER_ID]["name"].as_str(),
        Some(DEEPSEEK_PROVIDER_NAME)
    );
    assert!(
        doc["model_providers"][DEEPSEEK_PROVIDER_ID]["base_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("http://127.0.0.1:")
    );
    assert_eq!(
        doc["model_providers"][DEEPSEEK_PROVIDER_ID]["wire_api"].as_str(),
        Some(CODEX_PROXY_WIRE_API)
    );
    assert_eq!(
        doc["model_providers"][DEEPSEEK_PROVIDER_ID]["env_key"].as_str(),
        Some(DEEPSEEK_API_KEY_ENV)
    );
    assert_eq!(
        doc["model_providers"][DEEPSEEK_PROVIDER_ID]["api_key"].as_str(),
        Some("deepseek-secret")
    );
    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let slugs = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| { model["apply_patch_tool_type"].as_str() == Some("freeform") })
    );
    assert_eq!(
        catalog["models"][0]["input_modalities"].as_array().unwrap(),
        &vec![serde_json::Value::String("text".to_string())]
    );
    assert_eq!(
        catalog["models"][0]["supports_search_tool"].as_bool(),
        Some(false)
    );
    assert_eq!(
        catalog["models"][0]["supports_parallel_tool_calls"].as_bool(),
        Some(false)
    );
    assert!(catalog["models"].as_array().unwrap().iter().all(|model| {
        model["experimental_supported_tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool == "request_user_input"))
    }));
    for model in catalog["models"].as_array().unwrap() {
        let display_name = model["display_name"].as_str().unwrap();
        assert_eq!(
            model["max_output_tokens"].as_i64(),
            Some(model_max_output_tokens(display_name))
        );
    }
    let status = codex_acp_settings_status(&paths);
    assert_eq!(status.provider, BYOK_PROVIDER_ID);
    assert!(status.deepseek_key_configured);
}

#[test]
fn codex_acp_commandcode_catalog_uses_provider_specific_model_limits() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    write_codex_acp_provider_config(&paths, COMMANDCODE_PROVIDER_ID, "commandcode-secret").unwrap();

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(doc["model"].as_str(), Some(byok_encoded_model_slug(COMMANDCODE_MODEL, COMMANDCODE_PROVIDER_ID).as_str()));
    assert_eq!(
        doc["model_provider"].as_str(),
        Some(BYOK_PROVIDER_ID)
    );
    // `model_context_window` is intentionally omitted; see byok channel test.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens_for_provider(
            COMMANDCODE_MODEL,
            COMMANDCODE_PROVIDER_ID
        ))
    );

    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let models = catalog["models"].as_array().unwrap();
    let find_model = |display_name: &str| {
        models
            .iter()
            .find(|model| model["display_name"].as_str() == Some(display_name))
            .unwrap_or_else(|| panic!("missing {display_name} in commandcode catalog"))
    };

    let glm52 = find_model("zai-org/GLM-5.2");
    assert_eq!(glm52["context_window"].as_i64(), Some(1_000_000));
    assert_eq!(
        glm52["max_output_tokens"].as_i64(),
        Some(DEFAULT_MODEL_MAX_OUTPUT_TOKENS)
    );

    let qwen37 = find_model("Qwen/Qwen3.7-Max");
    assert_eq!(qwen37["context_window"].as_i64(), Some(1_000_000));
    assert_eq!(qwen37["max_output_tokens"].as_i64(), Some(65_536));

    let qwen36_plus = find_model("Qwen/Qwen3.6-Plus");
    assert_eq!(qwen36_plus["context_window"].as_i64(), Some(1_000_000));
    assert_eq!(qwen36_plus["max_output_tokens"].as_i64(), Some(65_536));

    let qwen36_max = find_model("Qwen/Qwen3.6-Max-Preview");
    assert_eq!(qwen36_max["context_window"].as_i64(), Some(256_000));
    assert_eq!(qwen36_max["max_output_tokens"].as_i64(), Some(65_536));

    let minimax_m3 = find_model("MiniMaxAI/MiniMax-M3");
    assert_eq!(minimax_m3["context_window"].as_i64(), Some(1_000_000));
    assert_eq!(minimax_m3["max_output_tokens"].as_i64(), Some(128_000));

    let minimax_m27 = find_model("MiniMaxAI/MiniMax-M2.7");
    assert_eq!(minimax_m27["context_window"].as_i64(), Some(204_800));
    assert_eq!(minimax_m27["max_output_tokens"].as_i64(), Some(128_000));
}

#[test]
fn codex_acp_env_repair_refreshes_known_provider_model_limits() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    paths.ensure_root().unwrap();
    let config_path = codex_config_path(&paths);
    std::fs::write(
        &config_path,
        r#"
model = "Qwen/Qwen3.7-Max"
model_provider = "commandcode"
preferred_auth_method = "apikey"
model_context_window = 200000
model_max_output_tokens = 128000
model_reasoning_effort = "none"

[model_providers.commandcode]
name = "CommandCode"
base_url = "https://api.commandcode.ai/provider/v1"
wire_api = "chat"
env_key = "COMMANDCODE_API_KEY"
api_key = "commandcode-secret"
"#,
    )
    .unwrap();

    ensure_codex_acp_env_key(&config_path).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    // `model_context_window` override is stripped so Codex resolves the window
    // per-model from `model_catalog_json`.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens_for_provider(
            "Qwen/Qwen3.7-Max",
            COMMANDCODE_PROVIDER_ID
        ))
    );
}

#[test]
fn codex_acp_env_repair_resolves_byok_commandcode_model_limits() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    paths.ensure_root().unwrap();
    let config_path = codex_config_path(&paths);
    std::fs::write(
        &config_path,
        r#"
model = "Qwen/Qwen3.7-Max"
model_provider = "byok"
preferred_auth_method = "apikey"
model_context_window = 200000
model_max_output_tokens = 128000
model_reasoning_effort = "none"
"#,
    )
    .unwrap();

    ensure_codex_acp_env_key(&config_path).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    // `model_context_window` override is stripped; resolved per-model.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens_for_provider(
            "Qwen/Qwen3.7-Max",
            COMMANDCODE_PROVIDER_ID
        ))
    );
}

#[test]
fn codex_provider_profiles_generate_expected_config() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    select_agent_provider_profile(
        &paths,
        AgentProviderFamily::Codex,
        CODEX_DEFAULT_PROVIDER_ID,
    )
    .unwrap();
    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    assert!(!command.starts_with("CODEX_HOME="));

    for (provider, env_key, _model) in [
        (
            TIMIAI_PROVIDER_ID,
            TIMIAI_API_KEY_ENV,
            model_slug_for_provider(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID),
        ),
        (
            COMMANDCODE_PROVIDER_ID,
            COMMANDCODE_API_KEY_ENV,
            model_slug_for_provider(COMMANDCODE_MODEL, COMMANDCODE_PROVIDER_ID),
        ),
        (
            DEEPSEEK_PROVIDER_ID,
            DEEPSEEK_API_KEY_ENV,
            model_slug_for_provider(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID),
        ),
        (
            KIMI_PROVIDER_ID,
            KIMI_API_KEY_ENV,
            model_slug_for_provider(KIMI_MODEL, KIMI_PROVIDER_ID),
        ),
        (
            MIMO_PROVIDER_ID,
            MIMO_API_KEY_ENV,
            model_slug_for_provider(MIMO_MODEL, MIMO_PROVIDER_ID),
        ),
    ] {
        let secret = format!("{provider}-secret");
        let snapshot =
            save_agent_provider_secret(&paths, AgentProviderFamily::Codex, provider, &secret)
                .unwrap();
        assert!(
            snapshot
                .codex_acp
                .profiles
                .iter()
                .any(|profile| profile.id == provider && profile.configured)
        );
        assert_ne!(snapshot.codex_acp.selected_profile_id, provider);
        let snapshot = select_codex_acp_provider(&paths, provider).unwrap();
        assert_eq!(snapshot.codex_acp.selected_profile_id, BYOK_PROVIDER_ID);

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        let configured_models = configured_codex_byok_models(&paths);
        let expected_model = default_model_for_provider_with_paths(&paths, provider);
        assert!(
            doc["model"]
                .as_str()
                .unwrap_or_default()
                .starts_with(PROVIDER_MODEL_ID_PREFIX)
        );
        assert!(!configured_models.is_empty());
        assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
        assert_eq!(
            doc["model_providers"][BYOK_PROVIDER_ID]["base_url"].as_str(),
            Some(codex_proxy_base_url().as_str())
        );
        assert_eq!(
            doc["model_providers"][provider]["env_key"].as_str(),
            Some(env_key)
        );
        assert_eq!(
            doc["model_providers"][provider]["wire_api"].as_str(),
            Some(CODEX_PROXY_WIRE_API)
        );
        assert_eq!(
            doc["model_providers"][provider]["api_key"].as_str(),
            Some(secret.as_str())
        );
        let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);
        assert!(env.contains(&(BYOK_API_KEY_ENV.to_string(), BYOK_PROVIDER_ID.to_string())));
        assert!(env.contains(&(env_key.to_string(), secret)));
    }
}

#[test]
fn timiai_key_is_shared_between_codex_and_claude_channels() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();

    assert!(
        snapshot
            .codex_acp
            .profiles
            .iter()
            .any(|profile| profile.id == TIMIAI_PROVIDER_ID && profile.configured)
    );
    assert!(
        snapshot
            .claude
            .profiles
            .iter()
            .any(|profile| profile.id == TIMIAI_PROVIDER_ID && profile.configured)
    );

    let snapshot = select_codex_acp_provider(&paths, TIMIAI_PROVIDER_ID).unwrap();
    assert_eq!(snapshot.codex_acp.selected_profile_id, BYOK_PROVIDER_ID);

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str())
    );
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["model_providers"][BYOK_PROVIDER_ID]["base_url"].as_str(),
        Some(codex_proxy_base_url().as_str())
    );
    assert!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["base_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("http://127.0.0.1:")
    );

    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let models = catalog["models"].as_array().unwrap();
    let slugs = models
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    let display_names = models
        .iter()
        .map(|model| model["display_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        slugs.contains(&byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str())
    );
    assert!(display_names.contains(&TIMIAI_CLAUDE_MODEL));
    assert!(!slugs.contains(&DEEPSEEK_MODEL));
    assert!(!slugs.contains(&model_slug(DEEPSEEK_MODEL)));

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);
    assert!(env.contains(&(BYOK_API_KEY_ENV.to_string(), BYOK_PROVIDER_ID.to_string())));
    assert!(env.contains(&(TIMIAI_API_KEY_ENV.to_string(), "timiai-secret".to_string())));
}

#[test]
fn provider_model_catalog_can_override_and_reset_models() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();

    let snapshot = save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![
            "gpt-5.6".to_string(),
            "claude-opus-4.9".to_string(),
            "gpt-5.6".to_string(),
            " ".to_string(),
        ],
    )
    .unwrap();
    let profile = snapshot
        .codex_acp
        .profiles
        .iter()
        .find(|profile| profile.id == TIMIAI_PROVIDER_ID)
        .unwrap();
    assert_eq!(profile.models, vec!["gpt-5.6", "claude-opus-4.9"]);
    assert_eq!(profile.model_list_url, None);

    let snapshot = save_provider_models_with_model_list_url(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec!["gpt-5.7".to_string(), "claude-opus-4.10".to_string()],
        Some("https://models.example.test/v1/models".to_string()),
    )
    .unwrap();
    let profile = snapshot
        .codex_acp
        .profiles
        .iter()
        .find(|profile| profile.id == TIMIAI_PROVIDER_ID)
        .unwrap();
    assert_eq!(profile.models, vec!["gpt-5.7", "claude-opus-4.10"]);
    assert_eq!(
        profile.model_list_url.as_deref(),
        Some("https://models.example.test/v1/models")
    );

    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let display_names = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["display_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(display_names.contains(&"gpt-5.7"));
    assert!(display_names.contains(&"claude-opus-4.10"));
    assert!(!display_names.contains(&TIMIAI_CODEX_MODEL));

    let snapshot = reset_provider_models(&paths, TIMIAI_PROVIDER_ID).unwrap();
    let profile = snapshot
        .codex_acp
        .profiles
        .iter()
        .find(|profile| profile.id == TIMIAI_PROVIDER_ID)
        .unwrap();
    assert!(profile.models.contains(&TIMIAI_CODEX_MODEL.to_string()));
    assert_eq!(profile.model_list_url, None);
    assert!(!profile.models.contains(&"gpt-5.7".to_string()));
}

#[test]
fn custom_provider_saves_config_and_exports_model_provider_map() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_custom_provider(
        &paths,
        CustomProviderInput {
            provider_id: None,
            label: "Lab Provider".to_string(),
            endpoint: "https://api.lab.test/v1/responses".to_string(),
            protocol: CustomProviderProtocol::Responses,
            api_key: "lab-secret".to_string(),
            model_list_url: Some("https://api.lab.test/v1/models".to_string()),
        },
    )
    .unwrap();
    let profile = snapshot
        .codex_acp
        .profiles
        .iter()
        .find(|profile| profile.custom && profile.label == "Lab Provider")
        .unwrap();
    assert!(profile.custom);
    assert!(profile.configured);
    let custom_id = profile.id.clone();
    assert_eq!(profile.label, "Lab Provider");
    assert_eq!(
        profile.base_url.as_deref(),
        Some("https://api.lab.test/v1/responses")
    );
    assert_eq!(profile.protocol, Some(CustomProviderProtocol::Responses));
    assert_eq!(
        profile.model_list_url.as_deref(),
        Some("https://api.lab.test/v1/models")
    );

    save_provider_models(&paths, &custom_id, vec!["lab-model".to_string()]).unwrap();
    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);
    let expected_env_key = format!("CUSTOM_PROVIDER_{}_API_KEY", custom_id.strip_prefix("custom_").unwrap_or(&custom_id).to_ascii_uppercase());
     assert!(env.contains(&(expected_env_key.clone(), "lab-secret".to_string())));
    let (_, provider_map) = env
        .iter()
        .find(|(name, _)| name == KODEX_MODEL_PROVIDER_MAP_ENV)
        .unwrap();
    let provider_map: serde_json::Value = serde_json::from_str(provider_map).unwrap();
    let custom_entry = provider_map
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["display_name"].as_str() == Some("lab-model"))
        .unwrap();
     assert_eq!(custom_entry["provider"].as_str(), Some(custom_id.as_str()));
    assert_eq!(
        custom_entry["base_url"].as_str(),
        Some("https://api.lab.test/v1/responses")
    );
    assert_eq!(custom_entry["protocol"].as_str(), Some("responses"));
    assert_eq!(
        custom_entry["env_key"].as_str(),
         Some(expected_env_key.as_str())
    );

     let snapshot = reset_provider_models(&paths, &custom_id).unwrap();
    let profile = snapshot
        .codex_acp
        .profiles
        .iter()
         .find(|profile| profile.id == custom_id)
        .unwrap();
    assert!(profile.custom);
    assert!(profile.configured);
    assert_eq!(
        profile.base_url.as_deref(),
        Some("https://api.lab.test/v1/responses")
    );
    assert!(profile.model_list_url.is_none());
}
#[test]
fn provider_model_catalog_parser_accepts_common_model_list_shapes() {
    let openai_models = parse_provider_models_response(
        r#"{"object":"list","data":[{"id":"gpt-5.5"},{"id":"Qwen/Qwen3.7-Max"},{"id":"gpt-5.5"}]}"#,
    )
    .unwrap();
    assert_eq!(openai_models, vec!["gpt-5.5", "Qwen/Qwen3.7-Max"]);

    let string_models =
        parse_provider_models_response(r#"["claude-opus-4-8","MiniMaxAI/MiniMax-M3"]"#).unwrap();
    assert_eq!(
        string_models,
        vec!["claude-opus-4-8", "MiniMaxAI/MiniMax-M3"]
    );

    let text_models =
        parse_provider_models_response("deepseek/deepseek-v4-pro\n\n- google/gemini-3.5-flash\n")
            .unwrap();
    assert_eq!(
        text_models,
        vec!["deepseek/deepseek-v4-pro", "google/gemini-3.5-flash"]
    );
}

#[test]
fn claude_byok_channel_uses_shared_timiai_key_and_local_proxy() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();
    let snapshot =
        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, TIMIAI_PROVIDER_ID)
            .unwrap();

    assert_eq!(snapshot.claude.selected_profile_id, BYOK_PROVIDER_ID);
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == BYOK_PROVIDER_ID
            && profile.configured
            && profile.models.contains(&TIMIAI_CODEX_MODEL.to_string())
            && profile.models.contains(&TIMIAI_CLAUDE_MODEL.to_string())
    }));

    let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
    ensure_agent_ready_for_command(&command, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&("ANTHROPIC_API_KEY".to_string(), "byok".to_string())));
    assert!(env.iter().any(|(name, value)| {
        name == "ANTHROPIC_BASE_URL" && value.starts_with("http://127.0.0.1:")
    }));
    assert!(env.contains(&(
        "ANTHROPIC_MODEL".to_string(),
        TIMIAI_CLAUDE_MODEL.to_string()
    )));
    let (_, model_config) = env
        .iter()
        .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
        .unwrap();
    let model_config: serde_json::Value = serde_json::from_str(model_config).unwrap();
    let available_models = model_config["availableModels"].as_array().unwrap();
    assert!(available_models.contains(&serde_json::Value::String(TIMIAI_CODEX_MODEL.to_string())));
    assert!(available_models.contains(&serde_json::Value::String(TIMIAI_CLAUDE_MODEL.to_string())));
    assert_eq!(
        model_config["modelOverrides"][TIMIAI_CLAUDE_MODEL].as_str(),
        Some(byok_encoded_model_slug(TIMIAI_CLAUDE_MODEL, TIMIAI_PROVIDER_ID).as_str())
    );
}

#[test]
fn claude_commandcode_source_key_contributes_to_byok_proxy_model_map() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        COMMANDCODE_PROVIDER_ID,
        "commandcode-secret",
    )
    .unwrap();
    let snapshot =
        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, COMMANDCODE_PROVIDER_ID)
            .unwrap();
    assert_eq!(snapshot.claude.selected_profile_id, BYOK_PROVIDER_ID);

    let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
    ensure_agent_ready_for_command(&command, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&(
        "ANTHROPIC_API_KEY".to_string(),
        BYOK_PROVIDER_ID.to_string()
    )));
    assert!(env.iter().any(|(name, value)| {
        name == "ANTHROPIC_BASE_URL" && value.starts_with("http://127.0.0.1:")
    }));
    assert!(!env.iter().any(|(_, value)| value == "commandcode-secret"));
    let (_, model_config) = env
        .iter()
        .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
        .unwrap();
    let model_config: serde_json::Value = serde_json::from_str(model_config).unwrap();
    let available_models = model_config["availableModels"].as_array().unwrap();
    assert!(available_models.contains(&serde_json::Value::String(COMMANDCODE_MODEL.to_string())));
    assert!(available_models.contains(&serde_json::Value::String(
        "claude-haiku-4-5-20251001".to_string()
    )));
    assert!(available_models.contains(&serde_json::Value::String("Qwen/Qwen3.7-Max".to_string())));
    assert_eq!(
        model_config["modelOverrides"][COMMANDCODE_MODEL].as_str(),
        Some(byok_encoded_model_slug(COMMANDCODE_MODEL, COMMANDCODE_PROVIDER_ID).as_str())
    );
    assert_eq!(
        model_config["modelOverrides"]["claude-haiku-4-5-20251001"].as_str(),
        Some(
            byok_encoded_model_slug("claude-haiku-4-5-20251001", COMMANDCODE_PROVIDER_ID).as_str()
        )
    );
    let (_, provider_map) = env
        .iter()
        .find(|(name, _)| name == KODEX_MODEL_PROVIDER_MAP_ENV)
        .unwrap();
    let provider_map: serde_json::Value = serde_json::from_str(provider_map).unwrap();
    assert!(provider_map.as_array().unwrap().iter().any(|entry| {
        entry["display_name"].as_str() == Some("Qwen/Qwen3.7-Max")
            && entry["provider"].as_str() == Some(COMMANDCODE_PROVIDER_ID)
    }));
}

#[test]
fn claude_fast_model_routes_internal_haiku_to_selected_byok_model() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        COMMANDCODE_PROVIDER_ID,
        "commandcode-secret",
    )
    .unwrap();
    let fast_model = byok_encoded_model_slug("Qwen/Qwen3.7-Max", COMMANDCODE_PROVIDER_ID);
    let snapshot = select_claude_fast_model(&paths, Some(fast_model.clone())).unwrap();

    assert_eq!(
        snapshot.claude.fast_model.as_deref(),
        Some(fast_model.as_str())
    );
    assert!(snapshot.claude.fast_model_options.iter().any(|option| {
        option.id == fast_model
            && option.label == "Qwen/Qwen3.7-Max"
            && option.provider_id == COMMANDCODE_PROVIDER_ID
    }));

    let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
    ensure_agent_ready_for_command(&command, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&(
        "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
        fast_model.clone()
    )));
    let (_, model_config) = env
        .iter()
        .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
        .unwrap();
    let model_config: serde_json::Value = serde_json::from_str(model_config).unwrap();
    assert_eq!(
        model_config["modelOverrides"]["claude-haiku-4-5-20251001"].as_str(),
        Some(fast_model.as_str())
    );
    assert_eq!(
        model_config["modelOverrides"]["haiku"].as_str(),
        Some(fast_model.as_str())
    );
}

#[test]
fn claude_byok_model_provider_map_keeps_earlier_provider_for_duplicate_models() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        COMMANDCODE_PROVIDER_ID,
        "commandcode-secret",
    )
    .unwrap();
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec!["deepseek-v4-pro-r1".to_string()],
    )
    .unwrap();
    save_provider_models(
        &paths,
        COMMANDCODE_PROVIDER_ID,
        vec!["deepseek-v4-pro-r1".to_string()],
    )
    .unwrap();

    let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
    ensure_agent_ready_for_command(&command, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);
    let (_, provider_map) = env
        .iter()
        .find(|(name, _)| name == KODEX_MODEL_PROVIDER_MAP_ENV)
        .unwrap();
    let provider_map: serde_json::Value = serde_json::from_str(provider_map).unwrap();
    let first_entry = provider_map
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["display_name"].as_str() == Some("deepseek-v4-pro-r1"))
        .unwrap();

    assert_eq!(first_entry["provider"].as_str(), Some(TIMIAI_PROVIDER_ID));
}

#[test]
fn saving_codex_byok_source_key_preserves_selected_byok_channel() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        DEEPSEEK_PROVIDER_ID,
        "deepseek-secret",
    )
    .unwrap();
    select_agent_provider_profile(&paths, AgentProviderFamily::Codex, DEEPSEEK_PROVIDER_ID)
        .unwrap();

    let snapshot = save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        MIMO_PROVIDER_ID,
        "mimo-secret",
    )
    .unwrap();

    assert_eq!(snapshot.codex_acp.selected_profile_id, BYOK_PROVIDER_ID);
    assert!(
        snapshot
            .codex_acp
            .profiles
            .iter()
            .any(|profile| profile.id == MIMO_PROVIDER_ID && profile.configured)
    );
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_model_slug(DEEPSEEK_MODEL).as_str())
    );
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["model_providers"][MIMO_PROVIDER_ID]["api_key"].as_str(),
        Some("mimo-secret")
    );

    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let slugs = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(slugs.contains(&byok_model_slug(DEEPSEEK_MODEL).as_str()));
    assert!(slugs.contains(&byok_model_slug(MIMO_MODEL).as_str()));
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| { model["apply_patch_tool_type"].as_str() == Some("freeform") })
    );
}

#[test]
fn codex_byok_session_launch_repairs_legacy_source_provider_catalog() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(KIMI_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();
    std::fs::write(
        codex_config_path(&paths),
        format!(
            r#"
model = "{kimi_model}"
model_provider = "{kimi_provider}"
preferred_auth_method = "apikey"
model_catalog_json = "{catalog_path}"

[model_providers.{deepseek_provider}]
name = "DeepSeek"
base_url = "http://127.0.0.1:17851/v1"
wire_api = "responses"
env_key = "DEEPSEEK_API_KEY"
api_key = "deepseek-secret"

[model_providers.{kimi_provider}]
name = "Kimi Code"
base_url = "http://127.0.0.1:17851/v1"
wire_api = "responses"
env_key = "KIMI_CODE_API_KEY"
api_key = "kimi-secret"

[model_providers.{mimo_provider}]
name = "Xiaomi Token Plan"
base_url = "http://127.0.0.1:17851/v1"
wire_api = "responses"
env_key = "XIAOMI_MIMO_API_KEY"
api_key = "mimo-secret"
"#,
            kimi_model = KIMI_MODEL,
            kimi_provider = KIMI_PROVIDER_ID,
            catalog_path = codex_model_catalog_path(&paths)
                .to_string_lossy()
                .replace('\\', "\\\\"),
            deepseek_provider = DEEPSEEK_PROVIDER_ID,
            mimo_provider = MIMO_PROVIDER_ID,
        ),
    )
    .unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&(BYOK_API_KEY_ENV.to_string(), BYOK_PROVIDER_ID.to_string())));
    assert!(env.contains(&(
        DEEPSEEK_API_KEY_ENV.to_string(),
        "deepseek-secret".to_string()
    )));
    assert!(env.contains(&(KIMI_API_KEY_ENV.to_string(), "kimi-secret".to_string())));
    assert!(env.contains(&(MIMO_API_KEY_ENV.to_string(), "mimo-secret".to_string())));
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_encoded_model_slug(KIMI_MODEL, KIMI_PROVIDER_ID).as_str())
    );
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));

    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert!(models.iter().any(|model| {
        model["display_name"].as_str() == Some(KIMI_MODEL)
    }));
}

#[test]
fn codex_byok_session_launch_repairs_misencoded_kimi_model_provider() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::CodexAcp,
            acp_port: 0,
            theme: AppTheme::KodexDark,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(
        codex_config_path(&paths),
        format!(
            r#"
model = "kodex-provider/{commandcode_provider}/{kimi_model}"
model_provider = "{byok_provider}"
preferred_auth_method = "apikey"
model_catalog_json = "{catalog_path}"

[model_providers.{commandcode_provider}]
name = "CommandCode"
base_url = "http://127.0.0.1:17851/v1"
wire_api = "responses"
env_key = "COMMANDCODE_API_KEY"
api_key = "commandcode-secret"

[model_providers.{kimi_provider}]
name = "Kimi Code"
base_url = "http://127.0.0.1:17851/v1"
wire_api = "responses"
env_key = "KIMI_CODE_API_KEY"
api_key = "kimi-secret"
"#,
            byok_provider = BYOK_PROVIDER_ID,
            commandcode_provider = COMMANDCODE_PROVIDER_ID,
            kimi_provider = KIMI_PROVIDER_ID,
            kimi_model = KIMI_MODEL,
            catalog_path = codex_model_catalog_path(&paths)
                .to_string_lossy()
                .replace('\\', "\\\\"),
        ),
    )
    .unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let _env = agent_env_for_command(&command, &paths);

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_encoded_model_slug(KIMI_MODEL, KIMI_PROVIDER_ID).as_str())
    );
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    // `model_context_window` override is stripped; resolved per-model.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(KIMI_MODEL_MAX_OUTPUT_TOKENS)
    );
}

#[test]
fn codex_acp_model_metadata_tracks_individual_model_limits() {
    assert_eq!(model_context_window("glm-5.2"), 1_000_000);
    assert_eq!(model_max_output_tokens("glm-5.2"), 128_000);
    assert_eq!(model_context_window("glm-5.1"), 200_000);
    assert_eq!(model_max_output_tokens("glm-5.1"), 128_000);
    assert_eq!(model_context_window("gpt-5.2"), 400_000);
    assert_eq!(model_context_window(KIMI_MODEL), KIMI_MODEL_CONTEXT_WINDOW);
    assert_eq!(
        model_max_output_tokens(KIMI_MODEL),
        KIMI_MODEL_MAX_OUTPUT_TOKENS
    );
    assert_eq!(
        model_slug_for_provider(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID),
        "deepseek-v4-pro"
    );
    assert_eq!(byok_model_slug("deepseek-v4-pro-external"), DEEPSEEK_MODEL);
    assert_eq!(
        model_slug_for_provider("MiMo-V2.5-Pro", MIMO_PROVIDER_ID),
        "mimo-v2.5-pro"
    );
    assert_eq!(
        model_slug_for_provider("MiMo-V2.5", MIMO_PROVIDER_ID),
        "mimo-v2.5"
    );
    assert_eq!(
        model_slug_for_provider("MiMo-V2.5-Pro", TIMIAI_PROVIDER_ID),
        "MiMo-V2.5-Pro"
    );
    assert_eq!(model_context_window("MiMo-V2.5-Pro"), 1_000_000);
    assert_eq!(model_max_output_tokens("MiMo-V2.5-Pro"), 128_000);
    assert_eq!(model_context_window("mimo-v2.5-pro"), 1_000_000);
    assert_eq!(model_max_output_tokens("mimo-v2.5-pro"), 128_000);
    assert_eq!(model_context_window("MiMo-V2.5"), 1_000_000);
    assert_eq!(model_max_output_tokens("MiMo-V2.5"), 128_000);
    assert_eq!(model_context_window("mimo-v2.5"), 1_000_000);
    assert_eq!(model_max_output_tokens("mimo-v2.5"), 128_000);
    assert_eq!(
        model_context_window_for_provider("zai-org/GLM-5.2", COMMANDCODE_PROVIDER_ID,),
        1_000_000
    );
    assert_eq!(model_max_output_tokens("gpt-5.2"), 128_000);
    assert_eq!(model_context_window("gpt-5.3"), 128_000);
    assert_eq!(model_max_output_tokens("gpt-5.3"), 16_384);
    assert_eq!(model_context_window("gpt-5.4"), 1_050_000);
    assert_eq!(model_max_output_tokens("gpt-5.4"), 128_000);
    assert_eq!(model_context_window("gpt-5.5"), 1_050_000);
    assert_eq!(model_max_output_tokens("gpt-5.5"), 128_000);
    assert_eq!(model_context_window("claude-opus-4.5"), 200_000);
    assert_eq!(model_max_output_tokens("claude-opus-4.5"), 64_000);
    assert_eq!(model_context_window("claude-sonnet-4.5"), 200_000);
    assert_eq!(model_max_output_tokens("claude-sonnet-4.5"), 64_000);
    assert_eq!(model_context_window("claude-opus-4.6"), 1_000_000);
    assert_eq!(model_max_output_tokens("claude-opus-4.6"), 128_000);
    assert_eq!(model_context_window("claude-sonnet-4.6"), 1_000_000);
    assert_eq!(model_max_output_tokens("claude-sonnet-4.6"), 64_000);
    assert_eq!(model_context_window("claude-opus-4.7"), 1_000_000);
    assert_eq!(model_max_output_tokens("claude-opus-4.7"), 128_000);
    assert_eq!(model_context_window("claude-opus-4.8"), 1_000_000);
    assert_eq!(model_max_output_tokens("claude-opus-4.8"), 128_000);
    assert_eq!(model_context_window("gemini-3.5-flash"), 1_000_000);
    assert_eq!(model_max_output_tokens("gemini-3.5-flash"), 128_000);
    assert_eq!(model_context_window("deepseek-v4-pro"), 1_000_000);
    assert_eq!(model_max_output_tokens("deepseek-v4-pro"), 384_000);
    assert_eq!(model_context_window("deepseek-v4-flash"), 1_000_000);
    assert_eq!(model_max_output_tokens("deepseek-v4-flash"), 384_000);
    assert_eq!(
        model_context_window_for_provider("Qwen/Qwen3.7-Max", COMMANDCODE_PROVIDER_ID),
        1_000_000
    );
    assert_eq!(
        model_max_output_tokens_for_provider("Qwen/Qwen3.7-Max", COMMANDCODE_PROVIDER_ID),
        65_536
    );
    assert_eq!(
        model_context_window_for_provider("Qwen/Qwen3.6-Plus", COMMANDCODE_PROVIDER_ID),
        1_000_000
    );
    assert_eq!(
        model_context_window_for_provider("Qwen/Qwen3.6-Max-Preview", COMMANDCODE_PROVIDER_ID),
        256_000
    );
    assert_eq!(
        model_context_window_for_provider("MiniMaxAI/MiniMax-M3", COMMANDCODE_PROVIDER_ID),
        1_000_000
    );
    assert_eq!(
        model_max_output_tokens_for_provider("MiniMaxAI/MiniMax-M3", COMMANDCODE_PROVIDER_ID),
        128_000
    );
    assert_eq!(
        byok_source_provider_for_model("Qwen/Qwen3.7-Max"),
        COMMANDCODE_PROVIDER_ID
    );
    assert_eq!(
        byok_source_provider_for_model("MiniMaxAI/MiniMax-M3"),
        COMMANDCODE_PROVIDER_ID
    );
    // Custom providers expose upstream model ids verbatim, with vendor
    // namespaces and casing that differ from the bare catalog slugs. The
    // metadata lookup must still resolve the correct window instead of
    // falling back to the 200k default.
    assert_eq!(model_context_window("z-ai/glm-5.2"), 1_000_000);
    assert_eq!(model_context_window("zai-org/glm-5.2"), 1_000_000);
    assert_eq!(model_context_window("GLM-5.2"), 1_000_000);
    assert_eq!(model_max_output_tokens("z-ai/glm-5.2"), 128_000);
    assert_eq!(model_context_window("vendor/gpt-5.4"), 1_050_000);
    assert_eq!(model_context_window("deepseek/deepseek-v4-pro"), 1_000_000);
}

#[test]
fn selecting_codex_provider_reuses_saved_key_and_rewrites_catalog() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret").unwrap();
    write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

    let snapshot = select_codex_acp_provider(&paths, TIMIAI_PROVIDER_ID).unwrap();

    assert_eq!(snapshot.codex_acp.provider, BYOK_PROVIDER_ID);
    assert!(snapshot.codex_acp.deepseek_key_configured);
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_encoded_model_slug(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID).as_str())
    );
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["api_key"].as_str(),
        Some("timiai-secret")
    );
    let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let slugs = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        slugs.contains(&byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str())
    );
    assert!(
        slugs.contains(&byok_encoded_model_slug(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID).as_str())
    );
}

#[test]
fn selecting_codex_provider_requires_existing_key() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let error = select_codex_acp_provider(&paths, DEEPSEEK_PROVIDER_ID).unwrap_err();

    assert!(error.to_string().contains("DeepSeek API key"));
}

#[test]
fn codex_acp_timiai_config_updates_existing_config_and_preserves_unrelated_entries() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(
        codex_config_path(&paths),
        r#"
approval_policy = "on-request"

[profiles.dev]
model = "old"

[model_providers.other]
name = "Other"
"#,
    )
    .unwrap();

    write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "new-secret").unwrap();

    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(doc["approval_policy"].as_str(), Some("on-request"));
    assert_eq!(doc["profiles"]["dev"]["model"].as_str(), Some("old"));
    assert_eq!(
        doc["model_providers"]["other"]["name"].as_str(),
        Some("Other")
    );
    assert_eq!(
        doc["model_providers"][TIMIAI_PROVIDER_ID]["api_key"].as_str(),
        Some("new-secret")
    );
}

#[test]
fn codex_acp_timiai_config_rejects_empty_key() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let error = write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "   ").unwrap_err();

    assert!(error.to_string().contains("api_key"));
    assert!(!codex_config_path(&paths).exists());
}

#[test]
fn codex_acp_timiai_config_rejects_malformed_existing_config_without_overwriting() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(codex_config_path(&paths), "[broken").unwrap();

    let error =
        write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret").unwrap_err();

    assert!(error.to_string().contains("failed to parse"));
    assert_eq!(
        std::fs::read_to_string(codex_config_path(&paths)).unwrap(),
        "[broken"
    );
}

#[test]
fn codex_acp_snapshot_reports_configured_without_secret() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();
    let serialized = serde_json::to_string(&snapshot).unwrap();

    assert_eq!(snapshot.codex_acp.selected_profile_id, BYOK_PROVIDER_ID);
    assert!(
        snapshot
            .codex_acp
            .profiles
            .iter()
            .any(|profile| profile.id == TIMIAI_PROVIDER_ID && profile.configured)
    );
    assert!(!serialized.contains("timiai-secret"));
}

#[test]
fn generic_provider_secret_snapshot_is_redacted() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let snapshot = save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Claude,
        DEEPSEEK_PROVIDER_ID,
        "deepseek-secret",
    )
    .unwrap();
    let serialized = serde_json::to_string(&snapshot).unwrap();

    assert!(
        snapshot
            .claude
            .profiles
            .iter()
            .any(|profile| profile.id == DEEPSEEK_PROVIDER_ID && profile.configured)
    );
    assert!(!serialized.contains("deepseek-secret"));
}

#[test]
fn claude_byok_channel_uses_shared_model_pool_and_local_proxy() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    for provider in [
        COMMANDCODE_PROVIDER_ID,
        DEEPSEEK_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        MIMO_PROVIDER_ID,
    ] {
        save_agent_provider_secret(
            &paths,
            AgentProviderFamily::Codex,
            provider,
            &format!("{provider}-secret"),
        )
        .unwrap();
    }
    let snapshot =
        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, BYOK_PROVIDER_ID)
            .unwrap();
    assert_eq!(snapshot.claude.selected_profile_id, BYOK_PROVIDER_ID);
    assert!(snapshot.claude.profiles.iter().any(|profile| {
        profile.id == BYOK_PROVIDER_ID
            && profile.configured
            && profile.models.contains(&DEEPSEEK_MODEL.to_string())
            && profile.models.contains(&KIMI_MODEL.to_string())
            && profile.models.contains(&MIMO_MODEL.to_string())
    }));

    let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
    ensure_agent_ready_for_command(&command, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&("ANTHROPIC_API_KEY".to_string(), "byok".to_string())));
    assert!(env.iter().any(
        |(name, value)| name == "ANTHROPIC_BASE_URL" && value.starts_with("http://127.0.0.1:")
    ));
    assert!(
        env.iter()
            .any(|(name, value)| name == "ANTHROPIC_MODEL" && value == COMMANDCODE_MODEL)
    );
    let (_, model_config) = env
        .iter()
        .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
        .unwrap();
    let model_config: serde_json::Value = serde_json::from_str(model_config).unwrap();
    let available_models = model_config["availableModels"].as_array().unwrap();
    assert!(available_models.contains(&serde_json::Value::String(DEEPSEEK_MODEL.to_string())));
    assert!(available_models.contains(&serde_json::Value::String("Qwen/Qwen3.7-Max".to_string())));
    assert!(available_models.contains(&serde_json::Value::String(KIMI_MODEL.to_string())));
    assert!(available_models.contains(&serde_json::Value::String(MIMO_MODEL.to_string())));
    assert_eq!(
        model_config["modelOverrides"][MIMO_MODEL].as_str(),
        Some(byok_encoded_model_slug(MIMO_MODEL, MIMO_PROVIDER_ID).as_str())
    );
    assert_eq!(model_config["preserveDefaultModel"].as_bool(), Some(false));
    let (_, provider_map) = env
        .iter()
        .find(|(name, _)| name == KODEX_MODEL_PROVIDER_MAP_ENV)
        .unwrap();
    let provider_map: serde_json::Value = serde_json::from_str(provider_map).unwrap();
    assert!(provider_map.as_array().unwrap().iter().any(|entry| {
        entry["display_name"].as_str() == Some("Qwen/Qwen3.7-Max")
            && entry["provider"].as_str() == Some(COMMANDCODE_PROVIDER_ID)
    }));
}

#[test]
fn legacy_claude_byok_source_selection_migrates_to_byok_channel() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    let mut settings = default_settings();
    settings.selected_claude_provider_profile_id = Some(MIMO_PROVIDER_ID.to_string());
    save_app_settings(&paths, &settings).unwrap();

    let loaded = load_app_settings(&paths);

    assert_eq!(
        loaded.selected_claude_provider_profile_id.as_deref(),
        Some(BYOK_PROVIDER_ID)
    );
    let content = std::fs::read_to_string(settings_path(&paths)).unwrap();
    assert!(content.contains("\"selected_claude_provider_profile_id\": \"byok\""));
}

#[test]
fn codex_acp_agent_env_reads_saved_timiai_key() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret").unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&(TIMIAI_API_KEY_ENV.to_string(), "timiai-secret".to_string())));
    assert!(env.iter().any(|(name, value)| {
        name == KODEX_MODEL_PROVIDER_MAP_ENV
            && value.contains(TIMIAI_PROVIDER_ID)
            && value.contains(TIMIAI_CODEX_MODEL)
    }));
    assert!(!command.contains("timiai-secret"));
}

#[test]
fn codex_acp_agent_env_reads_saved_deepseek_key() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);

    assert!(env.contains(&(
        DEEPSEEK_API_KEY_ENV.to_string(),
        "deepseek-secret".to_string()
    )));
    assert!(env.iter().any(|(name, value)| {
        name == KODEX_MODEL_PROVIDER_MAP_ENV
            && value.contains(DEEPSEEK_PROVIDER_ID)
            && value.contains("deepseek-v4-pro")
    }));
    assert!(!command.contains("deepseek-secret"));
}

#[test]
fn codex_acp_agent_env_migrates_legacy_venus_config_to_byok() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(
        codex_config_path(&paths),
        r#"
model = "glm-5.1"
model_provider = "venus"

[model_providers.venus]
name = "Venus LLM"
base_url = "https://v2.open.venus.woa.com/llmproxy/v1"
wire_api = "responses"
api_key = "old-secret"
"#,
    )
    .unwrap();

    let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
    let env = agent_env_for_command(&command, &paths);
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();

    assert!(env.contains(&(BYOK_API_KEY_ENV.to_string(), "byok".to_string())));
    assert!(env.iter().any(|(name, value)| {
        name == KODEX_MODEL_PROVIDER_MAP_ENV
            && value.contains(TIMIAI_PROVIDER_ID)
            && value.contains(TIMIAI_CODEX_MODEL)
    }));
    assert!(!env.iter().any(|(_, value)| value == "old-secret"));
    assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));
    assert_eq!(
        doc["model_providers"][BYOK_PROVIDER_ID]["env_key"].as_str(),
        Some(BYOK_API_KEY_ENV)
    );
    assert_eq!(
        doc["preferred_auth_method"].as_str(),
        Some(CODEX_AUTH_METHOD_API_KEY)
    );
    // `model_context_window` override is stripped; resolved per-model.
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(DEFAULT_MODEL_MAX_OUTPUT_TOKENS)
    );
    assert_eq!(
        doc["model_reasoning_effort"].as_str(),
        Some(CODEX_REASONING_EFFORT_NONE)
    );
    assert_eq!(
        doc["model"].as_str(),
        Some(byok_encoded_model_slug("glm-5.1", TIMIAI_PROVIDER_ID).as_str())
    );
}

#[test]
fn env_override_wins_over_persisted_selection() {
    let _guard = ENV_LOCK.lock().unwrap();
    let original = std::env::var("ACP_AGENT_COMMAND").ok();
    unsafe {
        std::env::set_var("ACP_AGENT_COMMAND", "custom-agent --acp");
    }
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_app_settings(
        &paths,
        &AppSettings {
            selected_agent: AgentCliId::Goose,
            acp_port: 0,
            theme: AppTheme::Graphite,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
            selected_codex_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
            claude: ClaudeProviderSettings::default(),
            web_tools: WebToolsSettings::default(),
            image: ImageSettings::default(),
        },
    )
    .unwrap();

    let command = resolve_agent_command_with_settings(&paths);

    match original {
        Some(value) => unsafe { std::env::set_var("ACP_AGENT_COMMAND", value) },
        None => unsafe { std::env::remove_var("ACP_AGENT_COMMAND") },
    }
    assert_eq!(command, "custom-agent --acp");
}

#[test]
fn lsp_settings_default_to_known_servers() {
    let settings = default_settings();

    let servers = all_effective_lsp_servers(&settings);

    assert!(servers.iter().any(|server| server.language_id == "rust"));
    let ts = effective_lsp_server(&settings, "typescript").unwrap();
    assert_eq!(ts.command, "typescript-language-server");
    assert_eq!(ts.args, vec!["--stdio"]);
    assert!(ts.enabled);
    assert!(!ts.customized);
}

#[test]
fn lsp_settings_apply_overrides_and_disabled_servers() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let saved = save_lsp_server_config(
        &paths,
        LspServerConfigInput {
            language_id: "rust".into(),
            enabled: false,
            command: "custom-rust-analyzer".into(),
            args: vec!["--log-file".into(), "ra.log".into()],
        },
    )
    .unwrap();

    let rust = effective_lsp_server(&saved, "rust").unwrap();
    assert!(!rust.enabled);
    assert_eq!(rust.command, "custom-rust-analyzer");
    assert_eq!(rust.args, vec!["--log-file", "ra.log"]);
    assert!(rust.customized);
}

#[test]
fn lsp_settings_reset_to_defaults() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_lsp_server_config(
        &paths,
        LspServerConfigInput {
            language_id: "python".into(),
            enabled: false,
            command: "custom-pyright".into(),
            args: vec![],
        },
    )
    .unwrap();

    let reset = reset_lsp_server_config(&paths, "python").unwrap();
    let python = effective_lsp_server(&reset, "python").unwrap();

    assert!(python.enabled);
    assert_eq!(python.command, "pyright-langserver");
    assert_eq!(python.args, vec!["--stdio"]);
    assert!(!python.customized);
}

#[test]
fn lsp_settings_reject_unknown_languages() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    let error = save_lsp_server_config(
        &paths,
        LspServerConfigInput {
            language_id: "madeup".into(),
            enabled: true,
            command: "server".into(),
            args: vec![],
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("Unsupported language server"));
}
