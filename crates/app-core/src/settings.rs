use crate::AppPaths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use workspace_model::{AgentCliId, AgentCliStatus, AgentSettingsSnapshot, AppSettings, AppTheme};

const SETTINGS_FILE: &str = "settings.json";

fn default_settings() -> AppSettings {
    AppSettings {
        selected_agent: AgentCliId::Codebuddy,
        acp_port: 0,
        theme: AppTheme::KodexDark,
    }
}

#[derive(Debug, Clone, Copy)]
struct AgentCliDefinition {
    id: AgentCliId,
    label: &'static str,
    binary: &'static str,
    acp_arg: &'static str,
}

const AGENTS: &[AgentCliDefinition] = &[
    AgentCliDefinition {
        id: AgentCliId::Codebuddy,
        label: "CodeBuddy",
        binary: "codebuddy",
        acp_arg: "--acp",
    },
    AgentCliDefinition {
        id: AgentCliId::Opencode,
        label: "OpenCode",
        binary: "opencode",
        acp_arg: "acp",
    },
];

pub fn load_app_settings(paths: &AppPaths) -> AppSettings {
    let path = settings_path(paths);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_else(default_settings)
}

pub fn save_app_settings(paths: &AppPaths, settings: &AppSettings) -> Result<()> {
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    let path = settings_path(paths);
    let content = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write app settings {}", path.display()))
}

pub fn settings_snapshot(paths: &AppPaths) -> AgentSettingsSnapshot {
    let settings = load_app_settings(paths);
    let agents = agent_statuses(settings.selected_agent);
    AgentSettingsSnapshot {
        settings,
        agents,
        env_override: std::env::var("ACP_AGENT_COMMAND").ok(),
    }
}

pub fn select_agent(paths: &AppPaths, agent: AgentCliId) -> Result<AgentSettingsSnapshot> {
    let status = detect_agent(agent);
    if !status.installed {
        anyhow::bail!("{} is not installed", status.binary);
    }

    let existing = load_app_settings(paths);
    let settings = AppSettings {
        selected_agent: agent,
        acp_port: existing.acp_port,
        theme: existing.theme,
    };
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn select_theme(paths: &AppPaths, theme: AppTheme) -> Result<AgentSettingsSnapshot> {
    let existing = load_app_settings(paths);
    let settings = AppSettings { theme, ..existing };
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn resolve_agent_command_with_settings(paths: &AppPaths) -> String {
    if let Ok(command) = std::env::var("ACP_AGENT_COMMAND") {
        return command;
    }

    let settings = load_app_settings(paths);
    command_for_agent(settings.selected_agent)
        .unwrap_or_else(acp_core::platform_default_agent_command)
}

pub fn command_for_agent(agent: AgentCliId) -> Option<String> {
    let def = definition(agent)?;
    let status = detect_agent(agent);
    if let Some(path) = status.detected_path {
        return Some(format!("{} {}", shell_quote_path(&path), def.acp_arg));
    }
    Some(format!("{} {}", binary_name(def.binary), def.acp_arg))
}

pub fn detect_agent(agent: AgentCliId) -> AgentCliStatus {
    let definition = definition(agent).expect("supported agent id");
    let detected_path = find_binary(definition.binary);
    AgentCliStatus {
        id: definition.id,
        label: definition.label.to_string(),
        binary: binary_name(definition.binary),
        installed: detected_path.is_some(),
        detected_path,
        selected: false,
    }
}

fn agent_statuses(selected_agent: AgentCliId) -> Vec<AgentCliStatus> {
    AGENTS
        .iter()
        .map(|definition| {
            let mut status = detect_agent(definition.id);
            status.selected = definition.id == selected_agent;
            status
        })
        .collect()
}

fn definition(agent: AgentCliId) -> Option<&'static AgentCliDefinition> {
    AGENTS.iter().find(|definition| definition.id == agent)
}

/// Given an agent command string, derive a human-friendly label.
pub fn agent_label_for_command(command: &str) -> String {
    let lower = command.to_lowercase();
    for agent in AGENTS {
        if lower.contains(agent.binary) {
            return agent.label.to_string();
        }
    }
    "CodeBuddy".to_string()
}

fn settings_path(paths: &AppPaths) -> PathBuf {
    paths.config_dir().join(SETTINGS_FILE)
}

fn binary_name(binary: &str) -> String {
    if cfg!(windows) {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    }
}

fn find_binary(binary: &str) -> Option<PathBuf> {
    let mut search_paths: Vec<PathBuf> = Vec::new();

    // Start with the current process PATH
    if let Some(paths) = std::env::var_os("PATH") {
        search_paths.extend(std::env::split_paths(&paths));
    }

    // On macOS, GUI apps launched from Finder/Dock do not inherit the
    // shell’s PATH (e.g. /opt/homebrew/bin). Append common directories
    // so CLI detection works out of the box.
    #[cfg(target_os = "macos")]
    {
        for extra in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/opt/homebrew/sbin",
            "/usr/local/sbin",
            "/usr/sbin",
            "/sbin",
        ] {
            let p = PathBuf::from(extra);
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
        }
    }

    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            format!("{binary}.bat"),
        ]
    } else {
        vec![binary.to_string()]
    };

    search_paths
        .into_iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())
}

fn shell_quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn missing_settings_default_to_codebuddy() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let settings = load_app_settings(&paths);

        assert_eq!(settings.selected_agent, AgentCliId::Codebuddy);
        assert_eq!(settings.theme, AppTheme::KodexDark);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let settings = AppSettings {
            selected_agent: AgentCliId::Opencode,
            acp_port: 9988,
            theme: AppTheme::Midnight,
        };

        save_app_settings(&paths, &settings).unwrap();
        let loaded = load_app_settings(&paths);

        assert_eq!(loaded, settings);
    }

    #[test]
    fn invalid_settings_default_to_codebuddy() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(settings_path(&paths), "not json").unwrap();

        let settings = load_app_settings(&paths);

        assert_eq!(settings.selected_agent, AgentCliId::Codebuddy);
        assert_eq!(settings.theme, AppTheme::KodexDark);
    }

    #[test]
    fn command_for_agent_uses_selected_binary_name() {
        let codebuddy = command_for_agent(AgentCliId::Codebuddy).unwrap();
        let opencode = command_for_agent(AgentCliId::Opencode).unwrap();

        assert!(codebuddy.to_lowercase().contains("codebuddy"));
        assert!(opencode.to_lowercase().contains("opencode"));
        assert!(codebuddy.ends_with(" --acp"));
        assert!(opencode.ends_with(" acp"));
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
                selected_agent: AgentCliId::Opencode,
                acp_port: 9988,
                theme: AppTheme::Graphite,
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
}
