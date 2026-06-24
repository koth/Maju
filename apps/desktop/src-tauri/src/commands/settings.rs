use crate::lsp::{LanguageServerRegistry, probe_command};
use crate::state::AppState;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};
use workspace_model::{
    AgentCliId, AgentInstallResult, AgentProviderFamily, AgentSettingsSnapshot, AppTheme,
    CustomProviderInput, LspProbeResult, LspServerConfigInput, LspServerSettingsEntry, LspSettingsSnapshot,
    RemoteMachineProfile, RemoteMachineProfileInput, RemoteMachineProfilesSnapshot,
    RemoteMachineValidationRequest,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CODEX_ACP_NPM_PACKAGE: &str = "@zed-industries/codex-acp@latest";
const CLAUDE_AGENT_ACP_NPM_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@latest";
const BUNDLED_CODEX_ACP_RESOURCE_DIR: &str = "bundled-codex-acp";
const BUNDLED_CLAUDE_AGENT_ACP_RESOURCE_DIR: &str = "bundled-claude-agent-acp";

#[tauri::command]
pub fn settings_get_agent_snapshot(
    state: State<'_, AppState>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_settings_snapshot(
            &scope.profile,
            scope.ssh_password.as_deref(),
        )
        .map_err(|e| e.to_string());
    }
    Ok(app_core::settings::settings_snapshot(&paths))
}

#[tauri::command]
pub fn settings_detect_agents(
    state: State<'_, AppState>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    settings_get_agent_snapshot(state, remote_profile_id)
}

#[tauri::command]
pub fn settings_select_agent(
    state: State<'_, AppState>,
    agent: AgentCliId,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_agent(
            &scope.profile,
            scope.ssh_password.as_deref(),
            agent,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_agent(&paths, agent).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_theme(
    state: State<'_, AppState>,
    theme: AppTheme,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_theme(
            &scope.profile,
            scope.ssh_password.as_deref(),
            theme,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_theme(&paths, theme).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_web_tools_settings(
    enabled: bool,
    provider: String,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::save_web_tools_settings(&paths, enabled, &provider)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_web_tools_provider_key(
    provider: String,
    api_key: String,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::save_web_tools_provider_key(&paths, &provider, &api_key)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_get_remote_profiles() -> Result<RemoteMachineProfilesSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(app_core::remote_profiles::load_remote_machine_profiles(
        &paths,
    ))
}

#[tauri::command]
pub fn settings_save_remote_profile(
    input: RemoteMachineProfileInput,
) -> Result<RemoteMachineProfilesSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::remote_profiles::save_remote_machine_profile(&paths, input).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_delete_remote_profile(
    profile_id: uuid::Uuid,
) -> Result<RemoteMachineProfilesSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::remote_profiles::delete_remote_machine_profile(&paths, profile_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_validate_remote_profile(
    request: RemoteMachineValidationRequest,
) -> Result<RemoteMachineProfilesSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::remote_profiles::validate_remote_machine_profile(&paths, request)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_codex_acp_provider_key(
    state: State<'_, AppState>,
    provider: String,
    api_key: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_save_agent_provider_secret(
            &scope.profile,
            scope.ssh_password.as_deref(),
            AgentProviderFamily::Codex,
            &provider,
            &api_key,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::save_codex_acp_provider_key(&paths, &provider, &api_key)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_codex_acp_provider(
    state: State<'_, AppState>,
    provider: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_agent_provider_profile(
            &scope.profile,
            scope.ssh_password.as_deref(),
            AgentProviderFamily::Codex,
            &provider,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_codex_acp_provider(&paths, &provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_codex_default_mode(
    state: State<'_, AppState>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_agent_provider_profile(
            &scope.profile,
            scope.ssh_password.as_deref(),
            AgentProviderFamily::Codex,
            "default",
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_codex_default_mode(&paths).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_agent_provider_profile(
    state: State<'_, AppState>,
    family: AgentProviderFamily,
    profile_id: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_agent_provider_profile(
            &scope.profile,
            scope.ssh_password.as_deref(),
            family,
            &profile_id,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_agent_provider_profile(&paths, family, &profile_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_agent_provider_secret(
    state: State<'_, AppState>,
    family: AgentProviderFamily,
    profile_id: String,
    secret: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_save_agent_provider_secret(
            &scope.profile,
            scope.ssh_password.as_deref(),
            family,
            &profile_id,
            &secret,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::save_agent_provider_secret(&paths, family, &profile_id, &secret)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_custom_provider(
    state: State<'_, AppState>,
    input: CustomProviderInput,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if remote_settings_scope(state.inner(), &paths, remote_profile_id)?.is_some() {
        return Err("远程机器暂不支持保存自定义 BYOK provider".to_string());
    }
    app_core::settings::save_custom_provider(&paths, input).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn settings_save_provider_models(
    state: State<'_, AppState>,
    provider: String,
    models: Vec<String>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_save_provider_models(
            &scope.profile,
            scope.ssh_password.as_deref(),
            &provider,
            models,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::save_provider_models(&paths, &provider, models).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn settings_sync_provider_models_from_url(
    state: State<'_, AppState>,
    provider: String,
    model_list_url: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let models =
        app_core::settings::fetch_provider_models_from_url(&paths, &provider, &model_list_url)
            .await
            .map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_save_provider_models_with_model_list_url(
            &scope.profile,
            scope.ssh_password.as_deref(),
            &provider,
            models,
            model_list_url,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::save_provider_models_with_model_list_url(
        &paths,
        &provider,
        models,
        Some(model_list_url),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_reset_provider_models(
    state: State<'_, AppState>,
    provider: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_reset_provider_models(
            &scope.profile,
            scope.ssh_password.as_deref(),
            &provider,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::reset_provider_models(&paths, &provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_claude_fast_model(
    state: State<'_, AppState>,
    model_id: Option<String>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_select_claude_fast_model(
            &scope.profile,
            scope.ssh_password.as_deref(),
            model_id,
        )
        .map_err(|e| e.to_string());
    }
    app_core::settings::select_claude_fast_model(&paths, model_id).map_err(|e| e.to_string())
}

struct RemoteSettingsScope {
    profile: RemoteMachineProfile,
    ssh_password: Option<String>,
}

fn remote_settings_scope(
    state: &AppState,
    paths: &app_core::AppPaths,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<Option<RemoteSettingsScope>, String> {
    let Some(profile_id) = remote_profile_id else {
        return Ok(None);
    };
    let profile = app_core::remote_profiles::get_remote_machine_profile(paths, profile_id)
        .map_err(|e| e.to_string())?;
    let active_remote = state.active_remote_linux_workspace()?;
    let ssh_password = active_remote
        .filter(|remote| remote.profile_id == Some(profile_id))
        .and_then(|remote| remote.ssh_password);
    Ok(Some(RemoteSettingsScope {
        profile,
        ssh_password,
    }))
}

#[tauri::command]
pub async fn settings_install_agent(
    app: AppHandle,
    agent: AgentCliId,
) -> Result<AgentInstallResult, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let install_paths = paths.clone();
    let bundled_codex_acp = if agent == AgentCliId::CodexAcp {
        bundled_codex_acp_binary(&app)
    } else {
        None
    };
    let bundled_claude_agent_acp = if agent == AgentCliId::ClaudeAgentAcp {
        bundled_claude_agent_acp_resource(&app)
    } else {
        None
    };
    let result = tokio::task::spawn_blocking(move || {
        install_agent(
            &install_paths,
            agent,
            bundled_codex_acp.as_deref(),
            bundled_claude_agent_acp.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("Installer task failed: {e}"))?;
    let snapshot =
        tokio::task::spawn_blocking(move || app_core::settings::settings_snapshot(&paths))
            .await
            .map_err(|e| format!("Settings refresh failed: {e}"))?;
    Ok(AgentInstallResult {
        agent,
        success: result.is_ok(),
        message: result.unwrap_or_else(|e| e),
        manual_instruction: manual_instruction(agent),
        snapshot,
    })
}

#[tauri::command]
pub fn settings_get_lsp_snapshot(
    state: State<'_, AppState>,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_lsp_settings_snapshot(
            &scope.profile,
            scope.ssh_password.as_deref(),
        )
        .map_err(|e| e.to_string());
    }
    lsp_snapshot_with_paths(state.inner(), &paths)
}

#[tauri::command]
pub fn settings_save_lsp_server(
    state: State<'_, AppState>,
    config: LspServerConfigInput,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_save_lsp_server_config(
            &scope.profile,
            scope.ssh_password.as_deref(),
            config,
        )
        .map_err(|e| e.to_string());
    }
    save_lsp_server_with_paths(state.inner(), &paths, config)
}

#[tauri::command]
pub fn settings_reset_lsp_server(
    state: State<'_, AppState>,
    language_id: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_reset_lsp_server_config(
            &scope.profile,
            scope.ssh_password.as_deref(),
            &language_id,
        )
        .map_err(|e| e.to_string());
    }
    reset_lsp_server_with_paths(state.inner(), &paths, &language_id)
}

#[tauri::command]
pub fn settings_probe_lsp_server(
    state: State<'_, AppState>,
    command: String,
    remote_profile_id: Option<uuid::Uuid>,
) -> Result<LspProbeResult, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(scope) = remote_settings_scope(state.inner(), &paths, remote_profile_id)? {
        return app_core::settings::remote_probe_lsp_server(
            &scope.profile,
            scope.ssh_password.as_deref(),
            &command,
        )
        .map_err(|e| e.to_string());
    }
    Ok(probe_command(&command))
}

fn save_lsp_server_with_paths(
    state: &AppState,
    paths: &app_core::AppPaths,
    config: LspServerConfigInput,
) -> Result<LspSettingsSnapshot, String> {
    let language_id = config.language_id.clone();
    let settings =
        app_core::settings::save_lsp_server_config(paths, config).map_err(|e| e.to_string())?;
    apply_lsp_settings_to_state(state, &settings, Some(&language_id));
    lsp_snapshot_with_paths(state, paths)
}

fn reset_lsp_server_with_paths(
    state: &AppState,
    paths: &app_core::AppPaths,
    language_id: &str,
) -> Result<LspSettingsSnapshot, String> {
    let settings = app_core::settings::reset_lsp_server_config(paths, language_id)
        .map_err(|e| e.to_string())?;
    apply_lsp_settings_to_state(state, &settings, Some(language_id));
    lsp_snapshot_with_paths(state, paths)
}

fn apply_lsp_settings_to_state(
    state: &AppState,
    settings: &workspace_model::AppSettings,
    restart_language_id: Option<&str>,
) {
    state
        .lsp_service()
        .configure_registry(LanguageServerRegistry::from_settings(settings));
    if let Some(language_id) = restart_language_id {
        state.lsp_service().shutdown_language(language_id);
    }
}

fn lsp_snapshot_with_paths(
    state: &AppState,
    paths: &app_core::AppPaths,
) -> Result<LspSettingsSnapshot, String> {
    let settings = app_core::settings::load_app_settings(&paths);
    apply_lsp_settings_to_state(state, &settings, None);
    let service = state.lsp_service();
    let servers = app_core::settings::all_effective_lsp_servers(&settings)
        .into_iter()
        .map(|server| {
            let probe = if server.enabled {
                probe_command(&server.command)
            } else {
                LspProbeResult {
                    available: false,
                    resolved_path: None,
                    message: Some("Language server disabled".into()),
                }
            };
            LspServerSettingsEntry {
                language_id: server.language_id.clone(),
                display_name: server.display_name,
                enabled: server.enabled,
                command: server.command,
                args: server.args,
                default_command: server.default_command,
                default_args: server.default_args,
                available: probe.available,
                resolved_path: probe.resolved_path,
                running: service.is_language_running(&server.language_id),
                message: probe.message,
                customized: server.customized,
            }
        })
        .collect();
    Ok(LspSettingsSnapshot { servers })
}

fn install_agent(
    paths: &app_core::AppPaths,
    agent: AgentCliId,
    bundled_codex_acp: Option<&Path>,
    bundled_claude_agent_acp: Option<&Path>,
) -> Result<String, String> {
    if agent == AgentCliId::CodexAcp {
        return install_codex_acp(paths, bundled_codex_acp);
    }
    if agent == AgentCliId::ClaudeAgentAcp {
        return install_claude_agent_acp(paths, bundled_claude_agent_acp);
    }

    let (program, args) = install_command(agent);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = command.output().map_err(|e| {
        format!("Failed to start installer. Make sure npm is installed and on PATH: {e}")
    })?;

    if output.status.success() {
        Ok("Installation completed. Re-detecting CLI availability.".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() { stderr } else { stdout };
        Err(if details.is_empty() {
            "Installer failed without output".to_string()
        } else {
            details
        })
    }
}

fn install_command(agent: AgentCliId) -> (&'static str, Vec<&'static str>) {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    match agent {
        AgentCliId::Codebuddy => (npm, vec!["install", "-g", "@tencent-ai/codebuddy-code"]),
        AgentCliId::Goose => {
            if cfg!(windows) {
                (
                    "powershell.exe",
                    vec![
                        "-NoProfile",
                        "-Command",
                        "Invoke-WebRequest -Uri \"https://raw.githubusercontent.com/aaif-goose/goose/main/download_cli.ps1\" -OutFile \"download_cli.ps1\"; .\\download_cli.ps1",
                    ],
                )
            } else {
                (
                    "sh",
                    vec![
                        "-c",
                        "curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | bash",
                    ],
                )
            }
        }
        AgentCliId::CodexAcp | AgentCliId::ClaudeAgentAcp => {
            unreachable!("managed agents are handled separately")
        }
    }
}

fn install_codex_acp(
    paths: &app_core::AppPaths,
    bundled_binary: Option<&Path>,
) -> Result<String, String> {
    if let Some(source) = bundled_binary.filter(|path| path.is_file()) {
        return install_codex_acp_from_bundled_binary(paths, source);
    }

    install_codex_acp_from_npm(paths)
}

fn install_claude_agent_acp(
    paths: &app_core::AppPaths,
    bundled_resource: Option<&Path>,
) -> Result<String, String> {
    if let Some(resource) = bundled_resource {
        if resource.is_file() {
            let target = install_managed_binary(
                paths,
                resource,
                &app_core::settings::claude_agent_acp_binary_path(paths),
            )?;
            return Ok(format!(
                "Claude Agent ACP 已从安装包安装到 {}",
                target.display()
            ));
        }
        if resource.join("package").is_dir() {
            install_claude_agent_acp_package(paths, &resource.join("package"))?;
            return Ok(format!(
                "Claude Agent ACP 已从安装包安装到 {}",
                app_core::settings::codex_acp_bin_dir(paths).display()
            ));
        }
    }
    install_claude_agent_acp_from_npm(paths)
}

pub(crate) fn install_bundled_claude_agent_acp_if_missing(app: &AppHandle) -> Result<(), String> {
    let Some(resource) = bundled_claude_agent_acp_resource(app) else {
        return Ok(());
    };
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    install_bundled_claude_agent_acp_if_missing_with_paths(&paths, &resource)
}

pub(crate) fn install_bundled_codex_acp_if_missing(app: &AppHandle) -> Result<(), String> {
    let Some(resource) = bundled_codex_acp_binary(app) else {
        return Ok(());
    };
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    install_bundled_codex_acp_if_missing_with_paths(&paths, &resource)
}

fn install_bundled_codex_acp_if_missing_with_paths(
    paths: &app_core::AppPaths,
    resource: &Path,
) -> Result<(), String> {
    if managed_binary_matches_resource(&app_core::settings::codex_acp_binary_path(paths), resource)
    {
        return Ok(());
    }
    install_codex_acp(paths, Some(resource)).map(|_| ())
}

fn install_bundled_claude_agent_acp_if_missing_with_paths(
    paths: &app_core::AppPaths,
    resource: &Path,
) -> Result<(), String> {
    if claude_agent_acp_managed_install_matches_resource(paths, resource) {
        return Ok(());
    }
    install_claude_agent_acp(paths, Some(resource)).map(|_| ())
}

fn claude_agent_acp_managed_install_matches_resource(
    paths: &app_core::AppPaths,
    resource: &Path,
) -> bool {
    if resource.is_file() {
        return managed_binary_matches_resource(
            &app_core::settings::claude_agent_acp_binary_path(paths),
            resource,
        );
    }
    claude_agent_acp_managed_install_exists(paths)
}

fn managed_binary_matches_resource(target: &Path, source: &Path) -> bool {
    let Ok(target_meta) = fs::metadata(target) else {
        return false;
    };
    let Ok(source_meta) = fs::metadata(source) else {
        return false;
    };
    if !target_meta.is_file() || !source_meta.is_file() || target_meta.len() != source_meta.len() {
        return false;
    }
    let Ok(target_modified) = target_meta.modified() else {
        return false;
    };
    let Ok(source_modified) = source_meta.modified() else {
        return false;
    };
    target_modified >= source_modified
}

fn claude_agent_acp_managed_install_exists(paths: &app_core::AppPaths) -> bool {
    let package_dir = app_core::settings::claude_agent_acp_package_dir(paths);
    let launcher = app_core::settings::codex_acp_bin_dir(paths).join(if cfg!(windows) {
        "claude-agent-acp.cmd"
    } else {
        "claude-agent-acp"
    });
    if package_dir.exists() {
        return launcher.is_file() && claude_agent_acp_package_is_runnable(&package_dir);
    }
    app_core::settings::claude_agent_acp_binary_path(paths).is_file()
}

fn claude_agent_acp_package_is_runnable(package_dir: &Path) -> bool {
    package_dir.join("dist").join("index.js").is_file()
        && package_dir.join("package.json").is_file()
        && package_dir
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("sdk")
            .join("package.json")
            .is_file()
        && package_dir
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-agent-sdk")
            .join("package.json")
            .is_file()
        && package_dir
            .join("node_modules")
            .join("zod")
            .join("package.json")
            .is_file()
}

fn install_claude_agent_acp_from_npm(paths: &app_core::AppPaths) -> Result<String, String> {
    let temp_dir = unique_install_temp_dir_named("kodex-claude-agent-acp-install");
    fs::create_dir_all(&temp_dir).map_err(|e| {
        format!(
            "Failed to create temporary installer directory {}: {e}",
            temp_dir.display()
        )
    })?;

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let temp_prefix = temp_dir.to_string_lossy().to_string();
    let output = Command::new(npm)
        .args([
            "install",
            "--prefix",
            &temp_prefix,
            "--no-save",
            "--omit=dev",
            CLAUDE_AGENT_ACP_NPM_PACKAGE,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .no_window()
        .output()
        .map_err(|e| format!("Failed to start npm installer for claude-agent-acp: {e}"))?;

    if !output.status.success() {
        let _ = fs::remove_dir_all(&temp_dir);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() { stderr } else { stdout };
        return Err(if details.is_empty() {
            "claude-agent-acp installer failed without output".to_string()
        } else {
            details
        });
    }

    let package = temp_dir
        .join("node_modules")
        .join("@agentclientprotocol")
        .join("claude-agent-acp");
    if !package.is_dir() {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(
            "Downloaded package did not contain @agentclientprotocol/claude-agent-acp".into(),
        );
    }
    let result = install_claude_agent_acp_package(paths, &package);
    let _ = fs::remove_dir_all(&temp_dir);
    result.map(|_| {
        format!(
            "Claude Agent ACP 已安装到 {}",
            app_core::settings::codex_acp_bin_dir(paths).display()
        )
    })
}

fn install_claude_agent_acp_package(
    paths: &app_core::AppPaths,
    package_source: &Path,
) -> Result<(), String> {
    let bin_dir = app_core::settings::codex_acp_bin_dir(paths);
    fs::create_dir_all(&bin_dir).map_err(|e| {
        format!(
            "Failed to create Claude Agent ACP install directory {}: {e}",
            bin_dir.display()
        )
    })?;
    let target_package = app_core::settings::claude_agent_acp_package_dir(paths);
    if target_package.exists() {
        fs::remove_dir_all(&target_package).map_err(|e| {
            format!(
                "Failed to remove previous Claude Agent ACP package {}: {e}",
                target_package.display()
            )
        })?;
    }
    copy_dir(package_source, &target_package).map_err(|e| {
        format!(
            "Failed to install Claude Agent ACP package from {} to {}: {e}",
            package_source.display(),
            target_package.display()
        )
    })?;
    if !claude_agent_acp_package_is_runnable(&target_package) {
        return Err(format!(
            "Claude Agent ACP package at {} is missing runtime dependencies; rebuild the bundle with node_modules included.",
            target_package.display()
        ));
    }
    write_claude_agent_acp_launcher(paths)
}

fn write_claude_agent_acp_launcher(paths: &app_core::AppPaths) -> Result<(), String> {
    let bin_dir = app_core::settings::codex_acp_bin_dir(paths);
    let package_dir = app_core::settings::claude_agent_acp_package_dir(paths);
    let entry = package_dir.join("dist").join("index.js");
    if cfg!(windows) {
        let launcher = bin_dir.join("claude-agent-acp.cmd");
        fs::write(
            &launcher,
            format!(
                "@echo off\r\nwhere node >nul 2>nul\r\nif errorlevel 1 (\r\n  echo Claude Agent ACP requires Node.js to run the bundled package launcher. Please install Node.js or install a native claude-agent-acp binary.\r\n  exit /b 1\r\n)\r\nif not exist \"{}\" (\r\n  echo Claude Agent ACP launcher is missing bundled entrypoint: {}\r\n  exit /b 1\r\n)\r\nnode \"{}\" %*\r\n",
                entry.to_string_lossy(),
                entry.to_string_lossy(),
                entry.to_string_lossy()
            ),
        )
        .map_err(|e| format!("Failed to write Claude Agent ACP launcher {}: {e}", launcher.display()))?;
    } else {
        let launcher = bin_dir.join("claude-agent-acp");
        fs::write(
            &launcher,
            format!(
                "#!/usr/bin/env sh\nif ! command -v node >/dev/null 2>&1; then\n  echo \"Claude Agent ACP requires Node.js to run the bundled package launcher. Please install Node.js or install a native claude-agent-acp binary.\" >&2\n  exit 1\nfi\nif [ ! -f \"{}\" ]; then\n  echo \"Claude Agent ACP launcher is missing bundled entrypoint: {}\" >&2\n  exit 1\nfi\nexec node \"{}\" \"$@\"\n",
                entry.to_string_lossy(),
                entry.to_string_lossy(),
                entry.to_string_lossy()
            ),
        )
        .map_err(|e| format!("Failed to write Claude Agent ACP launcher {}: {e}", launcher.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&launcher)
                .map_err(|e| format!("Failed to read launcher permissions: {e}"))?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&launcher, permissions)
                .map_err(|e| format!("Failed to mark launcher executable: {e}"))?;
        }
    }
    Ok(())
}

fn install_codex_acp_from_bundled_binary(
    paths: &app_core::AppPaths,
    source: &Path,
) -> Result<String, String> {
    let target = install_codex_acp_binary(paths, source)?;
    Ok(format!("Codex 已从安装包安装到 {}", target.display()))
}

fn install_codex_acp_from_npm(paths: &app_core::AppPaths) -> Result<String, String> {
    let temp_dir = unique_install_temp_dir();
    fs::create_dir_all(&temp_dir).map_err(|e| {
        format!(
            "Failed to create temporary installer directory {}: {e}",
            temp_dir.display()
        )
    })?;

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let temp_prefix = temp_dir.to_string_lossy().to_string();
    let output = Command::new(npm)
        .args([
            "install",
            "--prefix",
            &temp_prefix,
            "--no-save",
            "--omit=dev",
            CODEX_ACP_NPM_PACKAGE,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .no_window()
        .output()
        .map_err(|e| format!("Failed to start npm installer for codex-acp: {e}"))?;

    if !output.status.success() {
        let _ = fs::remove_dir_all(&temp_dir);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() { stderr } else { stdout };
        return Err(if details.is_empty() {
            "codex-acp installer failed without output".to_string()
        } else {
            details
        });
    }

    let binary_name = codex_acp_binary_name();
    let source = match find_named_file(&temp_dir, binary_name) {
        Some(source) => source,
        None => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(format!("Downloaded package did not contain {binary_name}"));
        }
    };
    let target = match install_codex_acp_binary(paths, &source) {
        Ok(target) => target,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(format!("Codex 已安装到 {}", target.display()))
}

fn install_codex_acp_binary(paths: &app_core::AppPaths, source: &Path) -> Result<PathBuf, String> {
    let target = app_core::settings::codex_acp_binary_path(paths);
    install_managed_binary(paths, source, &target)
}

fn install_managed_binary(
    paths: &app_core::AppPaths,
    source: &Path,
    target: &Path,
) -> Result<PathBuf, String> {
    let bin_dir = app_core::settings::codex_acp_bin_dir(paths);
    fs::create_dir_all(&bin_dir).map_err(|e| {
        format!(
            "Failed to create managed agent install directory {}: {e}",
            bin_dir.display()
        )
    })?;
    if let Err(first_error) = fs::copy(source, target) {
        terminate_processes_using_executable(target).map_err(|terminate_error| {
            format!(
                "Failed to install managed agent from {} to {}: {first_error}; also failed to stop the running target process: {terminate_error}",
                source.display(),
                target.display(),
            )
        })?;
        copy_with_retry(source, target).map_err(|retry_error| {
            format!(
                "Failed to install managed agent from {} to {} after stopping the running target process: {retry_error}",
                source.display(),
                target.display()
            )
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(target)
            .map_err(|e| format!("Failed to read managed agent permissions: {e}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(target, permissions)
            .map_err(|e| format!("Failed to mark managed agent executable: {e}"))?;
    }
    ad_hoc_sign_macos_executable(target, "managed agent")?;
    Ok(target.to_path_buf())
}

#[cfg(all(target_os = "macos", not(test)))]
fn ad_hoc_sign_macos_executable(target: &Path, label: &str) -> Result<(), String> {
    let target_arg = target.to_string_lossy().to_string();
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-", target_arg.as_str()])
        .output()
        .map_err(|e| format!("Failed to ad-hoc sign {label} {}: {e}", target.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        output.status.to_string()
    } else {
        stderr
    };
    Err(format!(
        "Failed to ad-hoc sign {label} {}: {detail}",
        target.display()
    ))
}

#[cfg(any(not(target_os = "macos"), test))]
fn ad_hoc_sign_macos_executable(_target: &Path, _label: &str) -> Result<(), String> {
    Ok(())
}

fn copy_with_retry(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    let mut last_error = None;
    for _ in 0..20 {
        match fs::copy(source, target) {
            Ok(_) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("copy retry failed")))
}

#[cfg(windows)]
fn terminate_processes_using_executable(target: &Path) -> Result<(), String> {
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let target = target.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$target = '{}'; Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -eq $target }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
        target
    );
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .no_window()
        .output()
        .map_err(|e| format!("Failed to start PowerShell to stop running agent: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() { stderr } else { stdout };
        Err(if details.is_empty() {
            "PowerShell failed without output".into()
        } else {
            details
        })
    }
}

#[cfg(not(windows))]
fn terminate_processes_using_executable(_target: &Path) -> Result<(), String> {
    Ok(())
}

fn bundled_codex_acp_binary(app: &AppHandle) -> Option<PathBuf> {
    let candidate = app
        .path()
        .resource_dir()
        .ok()?
        .join(BUNDLED_CODEX_ACP_RESOURCE_DIR)
        .join(codex_acp_binary_name());
    candidate.is_file().then_some(candidate)
}

fn bundled_claude_agent_acp_resource(app: &AppHandle) -> Option<PathBuf> {
    let root = app
        .path()
        .resource_dir()
        .ok()?
        .join(BUNDLED_CLAUDE_AGENT_ACP_RESOURCE_DIR);
    let binary = root.join(claude_agent_acp_binary_name());
    if binary.is_file() {
        return Some(binary);
    }
    root.is_dir().then_some(root)
}

fn codex_acp_binary_name() -> &'static str {
    if cfg!(windows) {
        "codex-acp.exe"
    } else {
        "codex-acp"
    }
}

fn claude_agent_acp_binary_name() -> &'static str {
    if cfg!(windows) {
        "claude-agent-acp.exe"
    } else {
        "claude-agent-acp"
    }
}

fn unique_install_temp_dir() -> PathBuf {
    unique_install_temp_dir_named("kodex-codex-acp-install")
}

fn unique_install_temp_dir_named(prefix: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("{prefix}-{}-{now}", std::process::id()))
}

fn copy_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn find_named_file(root: &Path, file_name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
            {
                return Some(path);
            }
        }
    }
    None
}

trait CommandNoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl CommandNoWindow for Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(0x08000000);
        self
    }
}

fn manual_instruction(agent: AgentCliId) -> Option<String> {
    match agent {
        AgentCliId::Codebuddy => Some(
            "Run `npm install -g @tencent-ai/codebuddy-code`, then ensure `codebuddy` is on PATH."
                .to_string(),
        ),
        AgentCliId::Goose => Some(
            "Run `curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | bash`, then ensure `goose` is on PATH. On Windows PowerShell, download and run `download_cli.ps1` from https://raw.githubusercontent.com/aaif-goose/goose/main/download_cli.ps1."
                .to_string(),
        ),
        AgentCliId::CodexAcp => Some(
            "点击下载会优先把安装包内置的 Codex 安装到 `~/.kodex/bin`，未内置时再在线下载；Kodex 只检测并启动这个目录下的二进制。使用前请在此页面配置 BYOK API key。"
                .to_string(),
        ),
        AgentCliId::ClaudeAgentAcp => Some(
            "点击下载会优先把安装包内置的 Claude Agent ACP 安装到 `~/.kodex/bin`，未内置时再在线下载；默认使用 BYOK 通道，请先保存至少一个 BYOK 模型 API key。"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_paths() -> app_core::AppPaths {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kodex-lsp-settings-{unique}"));
        fs::create_dir_all(&root).unwrap();
        app_core::AppPaths::from_root(root.join(".kodex"))
    }

    fn server<'a>(
        snapshot: &'a LspSettingsSnapshot,
        language_id: &str,
    ) -> &'a LspServerSettingsEntry {
        snapshot
            .servers
            .iter()
            .find(|entry| entry.language_id == language_id)
            .expect("server entry should exist")
    }

    #[test]
    fn lsp_snapshot_reports_default_servers() {
        let state = AppState::new();
        let paths = temp_paths();

        let snapshot = lsp_snapshot_with_paths(&state, &paths).unwrap();

        let rust = server(&snapshot, "rust");
        assert_eq!(rust.display_name, "Rust");
        assert_eq!(rust.command, "rust-analyzer");
        assert!(rust.enabled);
        assert!(!rust.customized);
    }

    #[test]
    fn lsp_save_updates_snapshot_and_disables_runtime_server() {
        let state = AppState::new();
        let paths = temp_paths();

        let snapshot = save_lsp_server_with_paths(
            &state,
            &paths,
            LspServerConfigInput {
                language_id: "typescript".into(),
                enabled: false,
                command: "custom-typescript-language-server".into(),
                args: vec!["--stdio".into(), "--log-level".into(), "4".into()],
            },
        )
        .unwrap();

        let typescript = server(&snapshot, "typescript");
        assert!(!typescript.enabled);
        assert_eq!(typescript.command, "custom-typescript-language-server");
        assert_eq!(typescript.args, vec!["--stdio", "--log-level", "4"]);
        assert!(typescript.customized);
        assert_eq!(
            typescript.message.as_deref(),
            Some("Language server disabled")
        );
    }

    #[test]
    fn lsp_reset_restores_default_snapshot() {
        let state = AppState::new();
        let paths = temp_paths();
        save_lsp_server_with_paths(
            &state,
            &paths,
            LspServerConfigInput {
                language_id: "python".into(),
                enabled: false,
                command: "custom-pyright".into(),
                args: vec![],
            },
        )
        .unwrap();

        let snapshot = reset_lsp_server_with_paths(&state, &paths, "python").unwrap();

        let python = server(&snapshot, "python");
        assert!(python.enabled);
        assert_eq!(python.command, "pyright-langserver");
        assert_eq!(python.args, vec!["--stdio"]);
        assert!(!python.customized);
    }

    #[test]
    fn lsp_probe_command_reports_missing_command() {
        let result = probe_command("definitely-not-a-kodex-lsp");

        assert!(!result.available);
        assert!(result.resolved_path.is_none());
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("not found")
        );
    }

    #[test]
    fn codex_acp_install_can_use_bundled_binary() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-bundled-codex-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(codex_acp_binary_name());
        fs::write(&source, b"bundled-codex-acp").unwrap();

        let message = install_codex_acp_from_bundled_binary(&paths, &source).unwrap();

        let target = app_core::settings::codex_acp_binary_path(&paths);
        assert_eq!(fs::read(&target).unwrap(), b"bundled-codex-acp");
        assert!(message.contains("安装包"));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn codex_acp_startup_seed_installs_bundled_binary_when_missing() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-startup-bundled-codex-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(codex_acp_binary_name());
        fs::write(&source, b"bundled-codex-acp").unwrap();

        install_bundled_codex_acp_if_missing_with_paths(&paths, &source).unwrap();

        let target = app_core::settings::codex_acp_binary_path(&paths);
        assert_eq!(fs::read(&target).unwrap(), b"bundled-codex-acp");
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn codex_acp_startup_seed_replaces_stale_bundled_binary() {
        let paths = temp_paths();
        let target = app_core::settings::codex_acp_binary_path(&paths);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"old").unwrap();

        let source_dir = std::env::temp_dir().join(format!(
            "kodex-startup-refresh-codex-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(codex_acp_binary_name());
        fs::write(&source, b"new bundled codex-acp").unwrap();

        install_bundled_codex_acp_if_missing_with_paths(&paths, &source).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new bundled codex-acp");
        assert!(managed_binary_matches_resource(&target, &source));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_install_can_use_bundled_package() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-bundled-claude-agent-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package = source_dir.join("package");
        write_test_claude_agent_acp_package(&package);

        let message = install_claude_agent_acp(&paths, Some(&source_dir)).unwrap();

        assert!(message.contains("安装包"));
        assert!(app_core::settings::claude_agent_acp_package_dir(&paths).is_dir());
        let launcher = app_core::settings::codex_acp_bin_dir(&paths).join(if cfg!(windows) {
            "claude-agent-acp.cmd"
        } else {
            "claude-agent-acp"
        });
        assert!(launcher.is_file());
        let launcher_text = fs::read_to_string(&launcher).unwrap();
        assert!(launcher_text.contains("requires Node.js"));
        assert!(launcher_text.contains("dist"));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_install_can_use_bundled_binary() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-bundled-claude-agent-acp-binary-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(claude_agent_acp_binary_name());
        fs::write(&source, b"bundled-claude-agent-acp").unwrap();

        let message = install_claude_agent_acp(&paths, Some(&source)).unwrap();

        let target = app_core::settings::claude_agent_acp_binary_path(&paths);
        assert_eq!(fs::read(&target).unwrap(), b"bundled-claude-agent-acp");
        assert!(message.contains("安装包"));
        assert!(!app_core::settings::claude_agent_acp_package_dir(&paths).exists());
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_startup_seed_installs_bundled_package_when_missing() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-startup-bundled-claude-agent-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package = source_dir.join("package");
        write_test_claude_agent_acp_package(&package);

        install_bundled_claude_agent_acp_if_missing_with_paths(&paths, &source_dir).unwrap();

        let launcher = app_core::settings::codex_acp_bin_dir(&paths).join(if cfg!(windows) {
            "claude-agent-acp.cmd"
        } else {
            "claude-agent-acp"
        });
        assert!(launcher.is_file());
        assert!(
            app_core::settings::claude_agent_acp_package_dir(&paths)
                .join("dist")
                .join("index.js")
                .is_file()
        );
        assert!(claude_agent_acp_managed_install_exists(&paths));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_startup_seed_installs_bundled_binary_when_missing() {
        let paths = temp_paths();
        let source_dir = std::env::temp_dir().join(format!(
            "kodex-startup-bundled-claude-agent-acp-binary-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(claude_agent_acp_binary_name());
        fs::write(&source, b"bundled-claude-agent-acp").unwrap();

        install_bundled_claude_agent_acp_if_missing_with_paths(&paths, &source).unwrap();

        let target = app_core::settings::claude_agent_acp_binary_path(&paths);
        assert_eq!(fs::read(&target).unwrap(), b"bundled-claude-agent-acp");
        assert!(claude_agent_acp_managed_install_exists(&paths));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_startup_seed_replaces_stale_bundled_binary() {
        let paths = temp_paths();
        let target = app_core::settings::claude_agent_acp_binary_path(&paths);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"old").unwrap();

        let source_dir = std::env::temp_dir().join(format!(
            "kodex-startup-refresh-claude-agent-acp-binary-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join(claude_agent_acp_binary_name());
        fs::write(&source, b"new bundled binary").unwrap();

        install_bundled_claude_agent_acp_if_missing_with_paths(&paths, &source).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new bundled binary");
        assert!(claude_agent_acp_managed_install_matches_resource(
            &paths, &source
        ));
        let _ = fs::remove_dir_all(source_dir);
    }

    #[test]
    fn claude_agent_acp_startup_seed_reinstalls_incomplete_package() {
        let paths = temp_paths();
        let bin_dir = app_core::settings::codex_acp_bin_dir(&paths);
        let target_package = app_core::settings::claude_agent_acp_package_dir(&paths);
        fs::create_dir_all(target_package.join("dist")).unwrap();
        fs::write(
            target_package.join("dist").join("index.js"),
            "console.log('old')",
        )
        .unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(
            bin_dir.join(if cfg!(windows) {
                "claude-agent-acp.cmd"
            } else {
                "claude-agent-acp"
            }),
            "node old",
        )
        .unwrap();
        assert!(!claude_agent_acp_managed_install_exists(&paths));

        let source_dir = std::env::temp_dir().join(format!(
            "kodex-repair-bundled-claude-agent-acp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_test_claude_agent_acp_package(&source_dir.join("package"));

        install_bundled_claude_agent_acp_if_missing_with_paths(&paths, &source_dir).unwrap();

        assert!(claude_agent_acp_managed_install_exists(&paths));
        assert!(
            target_package
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-agent-sdk")
                .join("package.json")
                .is_file()
        );
        let _ = fs::remove_dir_all(source_dir);
    }

    fn write_test_claude_agent_acp_package(package: &Path) {
        fs::create_dir_all(package.join("dist")).unwrap();
        fs::write(package.join("dist").join("index.js"), "console.log('ok')").unwrap();
        fs::write(package.join("package.json"), "{\"type\":\"module\"}").unwrap();
        for package_dir in [
            package
                .join("node_modules")
                .join("@agentclientprotocol")
                .join("sdk"),
            package
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-agent-sdk"),
            package.join("node_modules").join("zod"),
        ] {
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(package_dir.join("package.json"), "{}").unwrap();
        }
    }
}
