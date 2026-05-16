use crate::lsp::{LanguageServerRegistry, probe_command};
use crate::state::AppState;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;
use workspace_model::{
    AgentCliId, AgentInstallResult, AgentSettingsSnapshot, AppTheme, LspProbeResult,
    LspServerConfigInput, LspServerSettingsEntry, LspSettingsSnapshot,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CODEX_ACP_NPM_PACKAGE: &str = "@zed-industries/codex-acp@latest";

#[tauri::command]
pub fn settings_get_agent_snapshot() -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    Ok(app_core::settings::settings_snapshot(&paths))
}

#[tauri::command]
pub fn settings_detect_agents() -> Result<AgentSettingsSnapshot, String> {
    settings_get_agent_snapshot()
}

#[tauri::command]
pub fn settings_select_agent(
    _state: State<'_, AppState>,
    agent: AgentCliId,
) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::select_agent(&paths, agent).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_theme(theme: AppTheme) -> Result<AgentSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::select_theme(&paths, theme).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_codex_acp_venus_key(
    state: State<'_, AppState>,
    venus_key: String,
) -> Result<AgentSettingsSnapshot, String> {
    ensure_no_running_codex_acp_session(&state)?;
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::save_codex_acp_venus_key(&paths, &venus_key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_save_codex_acp_provider_key(
    state: State<'_, AppState>,
    provider: String,
    api_key: String,
) -> Result<AgentSettingsSnapshot, String> {
    ensure_no_running_codex_acp_session(&state)?;
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::save_codex_acp_provider_key(&paths, &provider, &api_key)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_codex_acp_provider(
    state: State<'_, AppState>,
    provider: String,
) -> Result<AgentSettingsSnapshot, String> {
    ensure_no_running_codex_acp_session(&state)?;
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::select_codex_acp_provider(&paths, &provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_select_codex_default_mode(
    state: State<'_, AppState>,
) -> Result<AgentSettingsSnapshot, String> {
    ensure_no_running_codex_acp_session(&state)?;
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    app_core::settings::select_codex_default_mode(&paths).map_err(|e| e.to_string())
}

fn ensure_no_running_codex_acp_session(state: &State<'_, AppState>) -> Result<(), String> {
    if state.has_running_codex_acp_session()? {
        return Err(
            "已有当前配置codex启动，不能切换，请把对应codex会话删除先，确保没有对应的codex-acp启动了再写新配置。"
                .into(),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn settings_install_agent(agent: AgentCliId) -> Result<AgentInstallResult, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let install_paths = paths.clone();
    let result = tokio::task::spawn_blocking(move || install_agent(&install_paths, agent))
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
) -> Result<LspSettingsSnapshot, String> {
    lsp_snapshot(&state)
}

#[tauri::command]
pub fn settings_save_lsp_server(
    state: State<'_, AppState>,
    config: LspServerConfigInput,
) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    save_lsp_server_with_paths(state.inner(), &paths, config)
}

#[tauri::command]
pub fn settings_reset_lsp_server(
    state: State<'_, AppState>,
    language_id: String,
) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    reset_lsp_server_with_paths(state.inner(), &paths, &language_id)
}

#[tauri::command]
pub fn settings_probe_lsp_server(command: String) -> Result<LspProbeResult, String> {
    Ok(probe_command(&command))
}

fn lsp_snapshot(state: &State<'_, AppState>) -> Result<LspSettingsSnapshot, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    lsp_snapshot_with_paths(state.inner(), &paths)
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

fn install_agent(paths: &app_core::AppPaths, agent: AgentCliId) -> Result<String, String> {
    if agent == AgentCliId::CodexAcp {
        return install_codex_acp(paths);
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
        AgentCliId::CodexAcp => unreachable!("codex-acp installation is handled manually"),
    }
}

fn install_codex_acp(paths: &app_core::AppPaths) -> Result<String, String> {
    let bin_dir = app_core::settings::codex_acp_bin_dir(paths);
    fs::create_dir_all(&bin_dir).map_err(|e| {
        format!(
            "Failed to create Codex install directory {}: {e}",
            bin_dir.display()
        )
    })?;

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

    let target = app_core::settings::codex_acp_binary_path(paths);
    let binary_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid codex-acp binary path {}", target.display()))?;
    let source = find_named_file(&temp_dir, binary_name)
        .ok_or_else(|| format!("Downloaded package did not contain {binary_name}"))?;

    fs::copy(&source, &target).map_err(|e| {
        format!(
            "Failed to install codex-acp from {} to {}: {e}",
            source.display(),
            target.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&target)
            .map_err(|e| format!("Failed to read installed codex-acp permissions: {e}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&target, permissions)
            .map_err(|e| format!("Failed to mark codex-acp executable: {e}"))?;
    }

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(format!("Codex 已安装到 {}", target.display()))
}

fn unique_install_temp_dir() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "kodex-codex-acp-install-{}-{now}",
        std::process::id()
    ))
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
            "点击下载会把 Codex 下载到 `~/.kodex/bin`；Kodex 只检测并启动这个目录下的二进制。使用前请在此页面配置 Venus 或 DeepSeek API key。"
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
        let result = settings_probe_lsp_server("definitely-not-a-kodex-lsp".into()).unwrap();

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
}
