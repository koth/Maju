use crate::state::AppState;
use std::process::{Command, Stdio};
use tauri::State;
use workspace_model::{AgentCliId, AgentInstallResult, AgentSettingsSnapshot, AppTheme};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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
pub fn settings_install_agent(agent: AgentCliId) -> Result<AgentInstallResult, String> {
    let paths = app_core::AppPaths::resolve().map_err(|e| e.to_string())?;
    let result = install_agent(agent);
    let snapshot = app_core::settings::settings_snapshot(&paths);
    Ok(AgentInstallResult {
        agent,
        success: result.is_ok(),
        message: result.unwrap_or_else(|e| e),
        manual_instruction: manual_instruction(agent),
        snapshot,
    })
}

fn install_agent(agent: AgentCliId) -> Result<String, String> {
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
    }
}
