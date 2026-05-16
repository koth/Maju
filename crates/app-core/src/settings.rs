use crate::AppPaths;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};
use workspace_model::{
    AgentCliId, AgentCliStatus, AgentSettingsSnapshot, AppSettings, AppTheme,
    CodexAcpSettingsStatus, CodexConnectionMode, LspServerConfigInput, LspServerSettings,
};

const SETTINGS_FILE: &str = "settings.json";
const CODEX_CONFIG_FILE: &str = "config.toml";
const CODEX_HOME_ENV: &str = "CODEX_HOME";
const VENUS_MODEL: &str = "glm-5.1";
const VENUS_PROVIDER_ID: &str = "venus";
const VENUS_PROVIDER_NAME: &str = "Venus LLM";
const VENUS_WIRE_API: &str = "responses";
const VENUS_API_KEY_ENV: &str = "VENUS_API_KEY";
const DEEPSEEK_MODEL: &str = "deepseek-v4-pro";
const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
const DEEPSEEK_PROVIDER_NAME: &str = "DeepSeek";
const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const VENUS_MODEL_CONTEXT_WINDOW: i64 = 200_000;
const VENUS_MODEL_MAX_OUTPUT_TOKENS: i64 = 128_000;
const CODEX_AUTH_METHOD_API_KEY: &str = "apikey";
const CODEX_REASONING_EFFORT_NONE: &str = "none";
const VENUS_CATALOG_MODELS: &[&str] = &[
    VENUS_MODEL,
    "gpt-5.2",
    "gpt-5.3",
    "gpt-5.4",
    "gpt-5.5",
    "claude-opus-4.5",
    "claude-sonnet-4.5",
    "claude-opus-4.6",
    "claude-sonnet-4.6",
    "claude-opus-4.7",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
];
const DEEPSEEK_CATALOG_MODELS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];
/// Maps display model names to actual model slugs sent to the server.
/// If a model is not in this map, the display name is used as-is.
const VENUS_MODEL_SLUG_MAP: &[(&str, &str)] = &[
    ("glm-5.1", "glm-5.1"),
    ("gpt-5.2", "gpt-5.2"),
    ("gpt-5.3", "gpt-5.3"),
    ("gpt-5.4", "gpt-5.4"),
    ("gpt-5.5", "gpt-5.5"),
    ("claude-opus-4.5", "claude-opus-4-5-20251101"),
    ("claude-sonnet-4.5", "claude-4-5-sonnet-20250929"),
    ("claude-opus-4.6", "claude-opus-4-6"),
    ("claude-sonnet-4.6", "claude-sonnet-4-6"),
    ("claude-opus-4.7", "claude-opus-4-7"),
    ("deepseek-v4-pro", "deepseek-v4-pro"),
    ("deepseek-v4-flash", "deepseek-v4-flash"),
];

const VENUS_MODEL_CONTEXT_WINDOWS: &[(&str, i64)] = &[
    (VENUS_MODEL, VENUS_MODEL_CONTEXT_WINDOW),
    ("gpt-5.2", 400_000),
    ("gpt-5.3", 128_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("claude-opus-4.5", 200_000),
    ("claude-sonnet-4.5", 200_000),
    ("claude-opus-4.6", 1_000_000),
    ("claude-sonnet-4.6", 1_000_000),
    ("claude-opus-4.7", 1_000_000),
    ("deepseek-v4-pro", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
];

const MODEL_MAX_OUTPUT_TOKENS: &[(&str, i64)] = &[
    (VENUS_MODEL, VENUS_MODEL_MAX_OUTPUT_TOKENS),
    ("gpt-5.2", 128_000),
    ("gpt-5.3", 16_384),
    ("gpt-5.4", 128_000),
    ("gpt-5.5", 128_000),
    ("claude-opus-4.5", 64_000),
    ("claude-sonnet-4.5", 64_000),
    ("claude-opus-4.6", 128_000),
    ("claude-sonnet-4.6", 64_000),
    ("claude-opus-4.7", 128_000),
    ("deepseek-v4-pro", 384_000),
    ("deepseek-v4-flash", 384_000),
];
const CODEX_ACP_BASE_INSTRUCTIONS: &str = r#"You are Codex, a coding agent. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled.

Use the available tools to inspect files, run commands, and edit the workspace when the request calls for action. Do not merely say that you will inspect or change something; perform the needed tool calls and then report the result.

When you need repository context, read the relevant files first. Keep responses concise, concrete, and grounded in what you actually observed or changed."#;

fn default_settings() -> AppSettings {
    AppSettings {
        selected_agent: AgentCliId::Codebuddy,
        acp_port: 0,
        theme: AppTheme::Graphite,
        lsp_servers: BTreeMap::new(),
        codex_connection_mode: CodexConnectionMode::Managed,
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
        id: AgentCliId::Goose,
        label: "goose",
        binary: "goose",
        acp_arg: "acp",
    },
    AgentCliDefinition {
        id: AgentCliId::CodexAcp,
        label: "Codex",
        binary: "codex-acp",
        acp_arg: "",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultLspServerDefinition {
    pub language_id: &'static str,
    pub display_name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLspServerConfig {
    pub language_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub default_command: String,
    pub default_args: Vec<String>,
    pub customized: bool,
}

pub const DEFAULT_LSP_SERVERS: &[DefaultLspServerDefinition] = &[
    DefaultLspServerDefinition {
        language_id: "rust",
        display_name: "Rust",
        command: "rust-analyzer",
        args: &[],
    },
    DefaultLspServerDefinition {
        language_id: "typescript",
        display_name: "TypeScript",
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "typescriptreact",
        display_name: "TSX",
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "javascript",
        display_name: "JavaScript",
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "javascriptreact",
        display_name: "JSX",
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "python",
        display_name: "Python",
        command: "pyright-langserver",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "json",
        display_name: "JSON",
        command: "vscode-json-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "css",
        display_name: "CSS",
        command: "vscode-css-language-server",
        args: &["--stdio"],
    },
    DefaultLspServerDefinition {
        language_id: "html",
        display_name: "HTML",
        command: "vscode-html-language-server",
        args: &["--stdio"],
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
    let agents = agent_statuses(paths, settings.selected_agent);
    AgentSettingsSnapshot {
        settings,
        agents,
        env_override: std::env::var("ACP_AGENT_COMMAND").ok(),
        codex_acp: codex_acp_settings_status(paths),
    }
}

pub fn select_agent(paths: &AppPaths, agent: AgentCliId) -> Result<AgentSettingsSnapshot> {
    let status = detect_agent_with_paths(paths, agent);
    if !status.installed {
        anyhow::bail!("{} is not installed", status.binary);
    }

    let existing = load_app_settings(paths);
    let settings = AppSettings {
        selected_agent: agent,
        acp_port: existing.acp_port,
        theme: existing.theme,
        lsp_servers: existing.lsp_servers,
        codex_connection_mode: existing.codex_connection_mode,
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

pub fn all_effective_lsp_servers(settings: &AppSettings) -> Vec<EffectiveLspServerConfig> {
    DEFAULT_LSP_SERVERS
        .iter()
        .map(|definition| effective_lsp_server_from_definition(settings, definition))
        .collect()
}

pub fn effective_lsp_server(
    settings: &AppSettings,
    language_id: &str,
) -> Option<EffectiveLspServerConfig> {
    DEFAULT_LSP_SERVERS
        .iter()
        .find(|definition| definition.language_id == language_id)
        .map(|definition| effective_lsp_server_from_definition(settings, definition))
}

pub fn save_lsp_server_config(
    paths: &AppPaths,
    input: LspServerConfigInput,
) -> Result<AppSettings> {
    if !is_known_lsp_language(&input.language_id) {
        anyhow::bail!("Unsupported language server: {}", input.language_id);
    }

    let mut settings = load_app_settings(paths);
    settings.lsp_servers.insert(
        input.language_id.clone(),
        LspServerSettings {
            enabled: Some(input.enabled),
            command: Some(input.command.trim().to_string()),
            args: Some(input.args),
        },
    );
    save_app_settings(paths, &settings)?;
    Ok(settings)
}

pub fn reset_lsp_server_config(paths: &AppPaths, language_id: &str) -> Result<AppSettings> {
    if !is_known_lsp_language(language_id) {
        anyhow::bail!("Unsupported language server: {language_id}");
    }
    let mut settings = load_app_settings(paths);
    settings.lsp_servers.remove(language_id);
    save_app_settings(paths, &settings)?;
    Ok(settings)
}

pub fn resolve_agent_command_with_settings(paths: &AppPaths) -> String {
    if let Ok(command) = std::env::var("ACP_AGENT_COMMAND") {
        return command;
    }

    let settings = load_app_settings(paths);
    command_for_agent_with_paths(settings.selected_agent, paths)
        .unwrap_or_else(acp_core::platform_default_agent_command)
}

pub fn command_for_agent(agent: AgentCliId) -> Option<String> {
    let def = definition(agent)?;
    let status = detect_agent(agent);
    if let Some(path) = status.detected_path {
        return Some(command_from_binary(&shell_quote_path(&path), def.acp_arg));
    }
    Some(command_from_binary(&binary_name(def.binary), def.acp_arg))
}

pub fn command_for_agent_with_paths(agent: AgentCliId, paths: &AppPaths) -> Option<String> {
    if agent == AgentCliId::CodexAcp {
        let def = definition(agent)?;
        let command = command_from_binary(
            &shell_quote_path(&codex_acp_binary_path(paths)),
            def.acp_arg,
        );
        if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
            return Some(command);
        }
        Some(format!(
            "{}={} {}",
            CODEX_HOME_ENV,
            shell_words::quote(&paths.root().to_string_lossy()),
            command
        ))
    } else {
        command_for_agent(agent)
    }
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

pub fn detect_agent_with_paths(paths: &AppPaths, agent: AgentCliId) -> AgentCliStatus {
    if agent != AgentCliId::CodexAcp {
        return detect_agent(agent);
    }

    let definition = definition(agent).expect("supported agent id");
    let binary_path = codex_acp_binary_path(paths);
    let detected_path = binary_path.is_file().then_some(binary_path);
    AgentCliStatus {
        id: definition.id,
        label: definition.label.to_string(),
        binary: binary_name(definition.binary),
        installed: detected_path.is_some(),
        detected_path,
        selected: false,
    }
}

fn agent_statuses(paths: &AppPaths, selected_agent: AgentCliId) -> Vec<AgentCliStatus> {
    AGENTS
        .iter()
        .map(|definition| {
            let mut status = detect_agent_with_paths(paths, definition.id);
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

/// Resolve a previously persisted human-friendly agent label back to an ACP command.
pub fn command_for_agent_label(label: &str) -> Option<String> {
    let normalized = label.trim().to_lowercase();
    AGENTS
        .iter()
        .find(|agent| {
            normalized == agent.label.to_lowercase() || normalized == agent.binary.to_lowercase()
        })
        .and_then(|agent| command_for_agent(agent.id))
}

pub fn command_for_agent_label_with_paths(label: &str, paths: &AppPaths) -> Option<String> {
    let normalized = label.trim().to_lowercase();
    AGENTS
        .iter()
        .find(|agent| {
            normalized == agent.label.to_lowercase() || normalized == agent.binary.to_lowercase()
        })
        .and_then(|agent| command_for_agent_with_paths(agent.id, paths))
}

pub fn agent_env_for_command(command: &str, paths: &AppPaths) -> Vec<(String, String)> {
    if !command.to_ascii_lowercase().contains("codex-acp") {
        return Vec::new();
    }
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        return Vec::new();
    }

    let config_path = codex_config_path(paths);
    let _ = ensure_codex_acp_env_key(&config_path);
    codex_active_provider_key(&config_path)
        .map(|(env_key, api_key)| vec![(env_key, api_key)])
        .unwrap_or_default()
}

fn settings_path(paths: &AppPaths) -> PathBuf {
    paths.config_dir().join(SETTINGS_FILE)
}

pub fn codex_config_path(paths: &AppPaths) -> PathBuf {
    paths.root().join(CODEX_CONFIG_FILE)
}

pub fn codex_acp_bin_dir(paths: &AppPaths) -> PathBuf {
    paths.root().join("bin")
}

pub fn codex_acp_binary_path(paths: &AppPaths) -> PathBuf {
    codex_acp_bin_dir(paths).join(binary_name("codex-acp"))
}

pub fn codex_acp_settings_status(paths: &AppPaths) -> CodexAcpSettingsStatus {
    let config_path = codex_config_path(paths);
    let connection_mode = load_app_settings(paths).codex_connection_mode;
    CodexAcpSettingsStatus {
        provider: codex_current_provider(paths),
        connection_mode,
        venus_key_configured: codex_venus_key_configured(&config_path),
        deepseek_key_configured: codex_deepseek_key_configured(&config_path),
        config_path,
    }
}

pub fn codex_current_provider(paths: &AppPaths) -> String {
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        "default".to_string()
    } else {
        codex_active_provider(&codex_config_path(paths))
    }
}

pub fn select_codex_default_mode(paths: &AppPaths) -> Result<AgentSettingsSnapshot> {
    let mut settings = load_app_settings(paths);
    settings.codex_connection_mode = CodexConnectionMode::Default;
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn save_codex_acp_venus_key(
    paths: &AppPaths,
    venus_key: &str,
) -> Result<AgentSettingsSnapshot> {
    write_codex_acp_provider_config(paths, VENUS_PROVIDER_ID, venus_key)?;
    save_codex_managed_mode(paths)?;
    Ok(settings_snapshot(paths))
}

pub fn write_codex_acp_venus_config(paths: &AppPaths, venus_key: &str) -> Result<()> {
    write_codex_acp_provider_config(paths, VENUS_PROVIDER_ID, venus_key)
}

pub fn save_codex_acp_provider_key(
    paths: &AppPaths,
    provider: &str,
    api_key: &str,
) -> Result<AgentSettingsSnapshot> {
    write_codex_acp_provider_config(paths, provider, api_key)?;
    save_codex_managed_mode(paths)?;
    Ok(settings_snapshot(paths))
}

pub fn select_codex_acp_provider(
    paths: &AppPaths,
    provider: &str,
) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_codex_provider(provider)?;
    let config_path = codex_config_path(paths);
    let Some(api_key) = codex_provider_key(&config_path, provider) else {
        anyhow::bail!("请先填写并保存 {} API key", provider_label(provider));
    };
    if api_key.trim().is_empty() {
        anyhow::bail!("请先填写并保存 {} API key", provider_label(provider));
    }

    write_codex_acp_provider_config(paths, provider, &api_key)?;
    save_codex_managed_mode(paths)?;
    Ok(settings_snapshot(paths))
}

fn save_codex_managed_mode(paths: &AppPaths) -> Result<()> {
    let mut settings = load_app_settings(paths);
    if settings.codex_connection_mode != CodexConnectionMode::Managed {
        settings.codex_connection_mode = CodexConnectionMode::Managed;
        save_app_settings(paths, &settings)?;
    }
    Ok(())
}

pub fn write_codex_acp_provider_config(
    paths: &AppPaths,
    provider: &str,
    api_key: &str,
) -> Result<()> {
    let provider = normalize_codex_provider(provider)?;
    let key = api_key.trim();
    if key.is_empty() {
        anyhow::bail!("api_key cannot be empty");
    }

    paths.ensure_root()?;
    let path = codex_config_path(paths);
    let mut doc = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read Codex config {}", path.display()))?;
        content
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse Codex config {}", path.display()))?
    } else {
        DocumentMut::new()
    };

    let default_model = default_model_for_provider(provider);
    doc["model"] = value(default_model);
    doc["model_provider"] = value(provider);
    doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
    doc["model_context_window"] = value(model_context_window(default_model));
    doc["model_max_output_tokens"] = value(model_max_output_tokens(default_model));
    doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
    doc["model_catalog_json"] = value(
        codex_model_catalog_path(paths)
            .to_string_lossy()
            .to_string(),
    );
    write_codex_provider_table(&mut doc, provider, key);
    write_codex_acp_model_catalog(paths, provider)?;

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))
}

fn codex_venus_key_configured(path: &Path) -> bool {
    codex_provider_key(path, VENUS_PROVIDER_ID)
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false)
}

fn codex_deepseek_key_configured(path: &Path) -> bool {
    codex_provider_key(path, DEEPSEEK_PROVIDER_ID)
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false)
}

fn codex_active_provider(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return VENUS_PROVIDER_ID.to_string();
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return VENUS_PROVIDER_ID.to_string();
    };
    doc.get("model_provider")
        .and_then(|item| item.as_str())
        .and_then(|provider| normalize_codex_provider(provider).ok())
        .unwrap_or(VENUS_PROVIDER_ID)
        .to_string()
}

fn codex_active_provider_key(path: &Path) -> Option<(String, String)> {
    let provider = codex_active_provider(path);
    let env_key = env_key_for_provider(&provider).to_string();
    codex_provider_key(path, &provider).map(|key| (env_key, key))
}

fn codex_provider_key(path: &Path, provider: &str) -> Option<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return None;
    };
    doc.get("model_providers")
        .and_then(|item| item.get(provider))
        .and_then(|item| item.get("api_key"))
        .and_then(|item| item.as_str())
        .map(|key| key.to_string())
}

fn ensure_codex_acp_env_key(path: &Path) -> Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read Codex config {}", path.display()))?;
    let mut doc = content
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse Codex config {}", path.display()))?;

    let provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .and_then(|provider| normalize_codex_provider(provider).ok())
        .unwrap_or(VENUS_PROVIDER_ID);
    let Some(api_key) = codex_provider_key_from_doc(&doc, provider) else {
        return Ok(());
    };
    if api_key.trim().is_empty() {
        return Ok(());
    }
    let proxy_base_url = acp_core::ensure_codex_api_proxy(provider, &api_key);
    let active_model = doc
        .get("model")
        .and_then(|item| item.as_str())
        .unwrap_or_else(|| default_model_for_provider(provider))
        .to_string();
    let mut changed = false;

    let provider_is_table = doc
        .get("model_providers")
        .and_then(|item| item.get(provider))
        .and_then(|item| item.as_table())
        .is_some();
    if !provider_is_table {
        write_codex_provider_table(&mut doc, provider, &api_key);
        changed = true;
    } else {
        if !provider_field_eq(&doc, provider, "name", provider_name(provider)) {
            doc["model_providers"][provider]["name"] = value(provider_name(provider));
            changed = true;
        }
        let base_url = proxy_base_url.clone();
        if !provider_field_eq(&doc, provider, "base_url", &base_url) {
            doc["model_providers"][provider]["base_url"] = value(base_url);
            changed = true;
        }
        if !provider_field_eq(&doc, provider, "wire_api", wire_api_for_provider(provider)) {
            doc["model_providers"][provider]["wire_api"] = value(wire_api_for_provider(provider));
            changed = true;
        }
        if !provider_field_eq(&doc, provider, "env_key", env_key_for_provider(provider)) {
            doc["model_providers"][provider]["env_key"] = value(env_key_for_provider(provider));
            changed = true;
        }
    }
    if doc.get("preferred_auth_method").is_none() {
        doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
        changed = true;
    }
    if !provider_field_eq(&doc, provider, "base_url", &proxy_base_url) {
        doc["model_providers"][provider]["base_url"] = value(proxy_base_url);
        changed = true;
    }
    if doc.get("model_context_window").is_none() {
        doc["model_context_window"] = value(model_context_window(&active_model));
        changed = true;
    }
    if doc.get("model_max_output_tokens").is_none() {
        doc["model_max_output_tokens"] = value(model_max_output_tokens(&active_model));
        changed = true;
    }
    if doc.get("model_reasoning_effort").is_none() {
        doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
        changed = true;
    }
    if let Some(parent) = path.parent() {
        let catalog_path = parent.join("model_catalog.json");
        let catalog_path_string = catalog_path.to_string_lossy().to_string();
        if doc.get("model_catalog_json").and_then(|item| item.as_str())
            != Some(catalog_path_string.as_str())
        {
            doc["model_catalog_json"] = value(catalog_path_string);
            changed = true;
        }
        let paths = AppPaths::from_root(parent);
        let _ = write_codex_acp_model_catalog(&paths, provider);
    }
    if !changed {
        return Ok(());
    }

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))
}

fn write_codex_provider_table(doc: &mut DocumentMut, provider: &str, api_key: &str) {
    if doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .is_none()
    {
        doc.insert("model_providers", Item::Table(Table::new()));
    }
    let providers = doc["model_providers"]
        .as_table_mut()
        .expect("model_providers should be a table");
    providers.insert(provider, Item::Table(Table::new()));
    let provider_table = providers
        .get_mut(provider)
        .and_then(|item| item.as_table_mut())
        .expect("provider should be a table");
    provider_table.insert("name", value(provider_name(provider)));
    provider_table.insert("base_url", value(base_url_for_provider(provider)));
    provider_table.insert("wire_api", value(wire_api_for_provider(provider)));
    provider_table.insert("env_key", value(env_key_for_provider(provider)));
    provider_table.insert("api_key", value(api_key));
}

fn venus_base_url() -> String {
    acp_core::codex_api_proxy_base_url()
}

fn normalize_codex_provider(provider: &str) -> Result<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        VENUS_PROVIDER_ID => Ok(VENUS_PROVIDER_ID),
        DEEPSEEK_PROVIDER_ID => Ok(DEEPSEEK_PROVIDER_ID),
        other => anyhow::bail!("Unsupported Codex provider: {other}"),
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        DEEPSEEK_PROVIDER_ID => model_slug(DEEPSEEK_MODEL),
        _ => model_slug(VENUS_MODEL),
    }
}

fn provider_name(provider: &str) -> &'static str {
    match provider {
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_PROVIDER_NAME,
        _ => VENUS_PROVIDER_NAME,
    }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        DEEPSEEK_PROVIDER_ID => "DeepSeek",
        _ => "Venus",
    }
}

fn wire_api_for_provider(_provider: &str) -> &'static str {
    VENUS_WIRE_API
}

fn env_key_for_provider(provider: &str) -> &'static str {
    match provider {
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_API_KEY_ENV,
        _ => VENUS_API_KEY_ENV,
    }
}

fn base_url_for_provider(provider: &str) -> String {
    match provider {
        DEEPSEEK_PROVIDER_ID => venus_base_url(),
        _ => venus_base_url(),
    }
}

fn codex_model_catalog_path(paths: &AppPaths) -> PathBuf {
    paths.root().join("model_catalog.json")
}

fn write_codex_acp_model_catalog(paths: &AppPaths, provider: &str) -> Result<()> {
    let path = codex_model_catalog_path(paths);
    let models = catalog_models_for_provider(provider)
        .iter()
        .enumerate()
        .map(|(priority, model)| codex_acp_model_catalog_entry(model, provider, priority))
        .collect::<Vec<_>>();
    let catalog = json!({
        "models": models
    });
    let content = serde_json::to_string_pretty(&catalog)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write Codex model catalog {}", path.display()))
}

fn catalog_models_for_provider(provider: &str) -> &'static [&'static str] {
    match provider {
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_CATALOG_MODELS,
        _ => VENUS_CATALOG_MODELS,
    }
}

fn codex_acp_model_catalog_entry(
    model: &str,
    provider: &str,
    priority: usize,
) -> serde_json::Value {
    let slug = model_slug(model);
    let context_window = model_context_window(model);
    let max_output_tokens = model_max_output_tokens(model);
    let is_deepseek = provider == DEEPSEEK_PROVIDER_ID;
    json!({
        "slug": slug,
        "display_name": model,
        "description": format!("Codex {model}"),
        "default_reasoning_level": CODEX_REASONING_EFFORT_NONE,
        "supported_reasoning_levels": [{
            "effort": CODEX_REASONING_EFFORT_NONE,
            "description": "No reasoning effort"
        }],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "additional_speed_tiers": [],
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": CODEX_ACP_BASE_INSTRUCTIONS,
        "model_messages": {
            "instructions_template": format!("{CODEX_ACP_BASE_INSTRUCTIONS}\n\n{{{{ personality }}}}"),
            "instructions_variables": {
                "personality_default": "",
                "personality_friendly": "",
                "personality_pragmatic": ""
            }
        },
        "supports_reasoning_summaries": false,
        "default_reasoning_summary": "none",
        "support_verbosity": true,
        "default_verbosity": "low",
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text_and_image",
        "truncation_policy": {
            "mode": "tokens",
            "limit": 10000
        },
        "supports_parallel_tool_calls": !is_deepseek,
        "supports_image_detail_original": !is_deepseek,
        "context_window": context_window,
        "max_context_window": context_window,
        "max_output_tokens": max_output_tokens,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": if is_deepseek { json!(["text"]) } else { json!(["text", "image"]) },
        "supports_search_tool": !is_deepseek
    })
}

/// Resolve a display model name to the actual slug sent to the server.
fn model_slug(display_name: &str) -> &str {
    VENUS_MODEL_SLUG_MAP
        .iter()
        .find_map(|(name, slug)| (*name == display_name).then_some(*slug))
        .unwrap_or(display_name)
}

fn model_context_window(model: &str) -> i64 {
    model_i64_metadata(
        model,
        VENUS_MODEL_CONTEXT_WINDOWS,
        VENUS_MODEL_CONTEXT_WINDOW,
    )
}

fn model_max_output_tokens(model: &str) -> i64 {
    model_i64_metadata(
        model,
        MODEL_MAX_OUTPUT_TOKENS,
        VENUS_MODEL_MAX_OUTPUT_TOKENS,
    )
}

fn model_i64_metadata(model: &str, metadata: &[(&str, i64)], fallback: i64) -> i64 {
    metadata
        .iter()
        .find_map(|(candidate, value)| {
            (*candidate == model || model_slug(candidate) == model).then_some(*value)
        })
        .unwrap_or(fallback)
}

fn codex_provider_key_from_doc(doc: &DocumentMut, provider: &str) -> Option<String> {
    doc.get("model_providers")
        .and_then(|item| item.get(provider))
        .and_then(|item| item.get("api_key"))
        .and_then(|item| item.as_str())
        .map(|key| key.to_string())
}

fn provider_field_eq(doc: &DocumentMut, provider: &str, field: &str, expected: &str) -> bool {
    doc.get("model_providers")
        .and_then(|item| item.get(provider))
        .and_then(|item| item.get(field))
        .and_then(|item| item.as_str())
        == Some(expected)
}

fn command_from_binary(binary: &str, acp_arg: &str) -> String {
    if acp_arg.is_empty() {
        binary.to_string()
    } else {
        format!("{binary} {acp_arg}")
    }
}

fn effective_lsp_server_from_definition(
    settings: &AppSettings,
    definition: &DefaultLspServerDefinition,
) -> EffectiveLspServerConfig {
    let override_settings = settings.lsp_servers.get(definition.language_id);
    let default_args = definition
        .args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let command = override_settings
        .and_then(|server| server.command.as_ref())
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .unwrap_or(definition.command)
        .to_string();
    let args = override_settings
        .and_then(|server| server.args.clone())
        .unwrap_or_else(|| default_args.clone());
    let enabled = override_settings
        .and_then(|server| server.enabled)
        .unwrap_or(true);

    EffectiveLspServerConfig {
        language_id: definition.language_id.to_string(),
        display_name: definition.display_name.to_string(),
        enabled,
        command,
        args,
        default_command: definition.command.to_string(),
        default_args,
        customized: override_settings.is_some(),
    }
}

fn is_known_lsp_language(language_id: &str) -> bool {
    DEFAULT_LSP_SERVERS
        .iter()
        .any(|definition| definition.language_id == language_id)
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

    // GUI apps launched from Finder/Dock often do not inherit the user's
    // interactive shell PATH. Include common user-local and system locations
    // so CLIs installed by scripts (e.g. goose in ~/.local/bin) are detected.
    if let Some(home) = dirs_next::home_dir() {
        for suffix in [".local/bin", "bin"] {
            let p = home.join(suffix);
            if !search_paths.contains(&p) {
                search_paths.push(p);
            }
        }
    }

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
        assert_eq!(settings.theme, AppTheme::Graphite);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let settings = AppSettings {
            selected_agent: AgentCliId::Goose,
            acp_port: 0,
            theme: AppTheme::Midnight,
            lsp_servers: BTreeMap::new(),
            codex_connection_mode: CodexConnectionMode::Managed,
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
        assert_eq!(settings.theme, AppTheme::Graphite);
    }

    #[test]
    fn command_for_agent_uses_selected_binary_name() {
        let codebuddy = command_for_agent(AgentCliId::Codebuddy).unwrap();
        let goose = command_for_agent(AgentCliId::Goose).unwrap();
        let codex_acp = command_for_agent(AgentCliId::CodexAcp).unwrap();

        assert!(codebuddy.to_lowercase().contains("codebuddy"));
        assert!(goose.to_lowercase().contains("goose"));
        assert!(codex_acp.to_lowercase().contains("codex-acp"));
        assert!(codebuddy.ends_with(" --acp"));
        assert!(goose.ends_with(" acp"));
        assert!(!codex_acp.ends_with(' '));
    }

    #[test]
    fn command_for_agent_label_resolves_persisted_labels() {
        let goose = command_for_agent_label("goose").unwrap();
        let codebuddy = command_for_agent_label("CodeBuddy").unwrap();
        let codex_acp = command_for_agent_label("codex-acp").unwrap();

        assert!(goose.to_lowercase().contains("goose"));
        assert!(codebuddy.to_lowercase().contains("codebuddy"));
        assert!(codex_acp.to_lowercase().contains("codex-acp"));
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
        assert_eq!(status.provider, DEEPSEEK_PROVIDER_ID);
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
    fn codex_acp_venus_config_creates_config_file() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        write_codex_acp_venus_config(&paths, "venus-secret").unwrap();

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        assert!(content.contains("[model_providers.venus]"));
        assert!(!content.contains("model_providers = {"));
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(VENUS_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(VENUS_PROVIDER_ID));
        assert_eq!(
            doc["preferred_auth_method"].as_str(),
            Some(CODEX_AUTH_METHOD_API_KEY)
        );
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(VENUS_MODEL_CONTEXT_WINDOW)
        );
        assert_eq!(
            doc["model_max_output_tokens"].as_integer(),
            Some(VENUS_MODEL_MAX_OUTPUT_TOKENS)
        );
        assert_eq!(
            doc["model_reasoning_effort"].as_str(),
            Some(CODEX_REASONING_EFFORT_NONE)
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["name"].as_str(),
            Some(VENUS_PROVIDER_NAME)
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["base_url"].as_str(),
            Some(venus_base_url().as_str())
        );
        assert_eq!(
            doc["model_catalog_json"].as_str(),
            Some(codex_model_catalog_path(&paths).to_string_lossy().as_ref())
        );
        assert!(codex_model_catalog_path(&paths).is_file());
        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        assert!(catalog.contains("You are Codex, a coding agent"));
        assert!(!catalog.contains("{{ base_instructions }}"));
        assert!(catalog.contains("\"slug\": \"glm-5.1\""));
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        for model in catalog["models"].as_array().unwrap() {
            let display_name = model["display_name"].as_str().unwrap();
            assert_eq!(
                model["max_output_tokens"].as_i64(),
                Some(model_max_output_tokens(display_name))
            );
        }
        assert!(
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["display_name"].as_str() == Some("gpt-5.3"))
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["wire_api"].as_str(),
            Some(VENUS_WIRE_API)
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["env_key"].as_str(),
            Some(VENUS_API_KEY_ENV)
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["api_key"].as_str(),
            Some("venus-secret")
        );
        let status = codex_acp_settings_status(&paths);
        assert_eq!(status.provider, VENUS_PROVIDER_ID);
        assert!(status.venus_key_configured);
        assert!(!status.deepseek_key_configured);
    }

    #[test]
    fn codex_acp_deepseek_config_creates_provider_config() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        assert!(content.contains("[model_providers.deepseek]"));
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(DEEPSEEK_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(DEEPSEEK_PROVIDER_ID));
        assert_eq!(
            doc["model_max_output_tokens"].as_integer(),
            Some(model_max_output_tokens(DEEPSEEK_MODEL))
        );
        assert_eq!(
            doc["model_providers"][DEEPSEEK_PROVIDER_ID]["name"].as_str(),
            Some(DEEPSEEK_PROVIDER_NAME)
        );
        assert_eq!(
            doc["model_providers"][DEEPSEEK_PROVIDER_ID]["base_url"].as_str(),
            Some(venus_base_url().as_str())
        );
        assert_eq!(
            doc["model_providers"][DEEPSEEK_PROVIDER_ID]["wire_api"].as_str(),
            Some(VENUS_WIRE_API)
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
        assert_eq!(slugs, vec!["deepseek-v4-pro", "deepseek-v4-flash"]);
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
        for model in catalog["models"].as_array().unwrap() {
            let display_name = model["display_name"].as_str().unwrap();
            assert_eq!(
                model["max_output_tokens"].as_i64(),
                Some(model_max_output_tokens(display_name))
            );
        }
        let status = codex_acp_settings_status(&paths);
        assert_eq!(status.provider, DEEPSEEK_PROVIDER_ID);
        assert!(!status.venus_key_configured);
        assert!(status.deepseek_key_configured);
    }

    #[test]
    fn codex_acp_model_metadata_tracks_individual_model_limits() {
        assert_eq!(model_context_window("glm-5.1"), 200_000);
        assert_eq!(model_max_output_tokens("glm-5.1"), 128_000);
        assert_eq!(model_context_window("gpt-5.2"), 400_000);
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
        assert_eq!(model_context_window("deepseek-v4-pro"), 1_000_000);
        assert_eq!(model_max_output_tokens("deepseek-v4-pro"), 384_000);
        assert_eq!(model_context_window("deepseek-v4-flash"), 1_000_000);
        assert_eq!(model_max_output_tokens("deepseek-v4-flash"), 384_000);
    }

    #[test]
    fn selecting_codex_provider_reuses_saved_key_and_rewrites_catalog() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        write_codex_acp_provider_config(&paths, VENUS_PROVIDER_ID, "venus-secret").unwrap();
        write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

        let snapshot = select_codex_acp_provider(&paths, VENUS_PROVIDER_ID).unwrap();

        assert_eq!(snapshot.codex_acp.provider, VENUS_PROVIDER_ID);
        assert!(snapshot.codex_acp.venus_key_configured);
        assert!(snapshot.codex_acp.deepseek_key_configured);
        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(VENUS_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(VENUS_PROVIDER_ID));
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["api_key"].as_str(),
            Some("venus-secret")
        );
        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let slugs = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(slugs.contains(&VENUS_MODEL));
        assert!(slugs.contains(&"gpt-5.3"));
        assert!(slugs.contains(&"gpt-5.5"));
    }

    #[test]
    fn selecting_codex_provider_requires_existing_key() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let error = select_codex_acp_provider(&paths, DEEPSEEK_PROVIDER_ID).unwrap_err();

        assert!(error.to_string().contains("DeepSeek API key"));
    }

    #[test]
    fn codex_acp_venus_config_updates_existing_config_and_preserves_unrelated_entries() {
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

        write_codex_acp_venus_config(&paths, "new-secret").unwrap();

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(doc["profiles"]["dev"]["model"].as_str(), Some("old"));
        assert_eq!(
            doc["model_providers"]["other"]["name"].as_str(),
            Some("Other")
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["api_key"].as_str(),
            Some("new-secret")
        );
    }

    #[test]
    fn codex_acp_venus_config_rejects_empty_key() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let error = write_codex_acp_venus_config(&paths, "   ").unwrap_err();

        assert!(error.to_string().contains("api_key"));
        assert!(!codex_config_path(&paths).exists());
    }

    #[test]
    fn codex_acp_venus_config_rejects_malformed_existing_config_without_overwriting() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(codex_config_path(&paths), "[broken").unwrap();

        let error = write_codex_acp_venus_config(&paths, "venus-secret").unwrap_err();

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

        let snapshot = save_codex_acp_venus_key(&paths, "venus-secret").unwrap();
        let serialized = serde_json::to_string(&snapshot).unwrap();

        assert!(snapshot.codex_acp.venus_key_configured);
        assert!(serialized.contains("venus_key_configured"));
        assert!(!serialized.contains("venus-secret"));
    }

    #[test]
    fn codex_acp_agent_env_reads_saved_venus_key() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        write_codex_acp_venus_config(&paths, "venus-secret").unwrap();

        let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);

        assert_eq!(
            env,
            vec![(VENUS_API_KEY_ENV.to_string(), "venus-secret".to_string())]
        );
        assert!(!command.contains("venus-secret"));
    }

    #[test]
    fn codex_acp_agent_env_reads_saved_deepseek_key() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

        let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);

        assert_eq!(
            env,
            vec![(
                DEEPSEEK_API_KEY_ENV.to_string(),
                "deepseek-secret".to_string()
            )]
        );
        assert!(!command.contains("deepseek-secret"));
    }

    #[test]
    fn codex_acp_agent_env_migrates_existing_api_key_config() {
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

        assert_eq!(
            env,
            vec![(VENUS_API_KEY_ENV.to_string(), "old-secret".to_string())]
        );
        assert_eq!(
            doc["model_providers"][VENUS_PROVIDER_ID]["env_key"].as_str(),
            Some(VENUS_API_KEY_ENV)
        );
        assert_eq!(
            doc["preferred_auth_method"].as_str(),
            Some(CODEX_AUTH_METHOD_API_KEY)
        );
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(VENUS_MODEL_CONTEXT_WINDOW)
        );
        assert_eq!(
            doc["model_max_output_tokens"].as_integer(),
            Some(VENUS_MODEL_MAX_OUTPUT_TOKENS)
        );
        assert_eq!(
            doc["model_reasoning_effort"].as_str(),
            Some(CODEX_REASONING_EFFORT_NONE)
        );
        assert_eq!(doc["model"].as_str(), Some(VENUS_MODEL));
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
}
