use super::*;
use crate::AppPaths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, Item, value};
use workspace_model::{
    AgentCliId, AgentCliStatus, AgentProviderFamily, AppSettings, CodexConnectionMode,
};

/// npm package backing the DeepSeek Harness `dsh` CLI.
pub const DSH_NPM_PACKAGE: &str = "@deepseek-ai/dsh";

fn npm_program() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

/// Read the installed `dsh` CLI version via `dsh --version`, parsing the first
/// semver-looking token from stdout (e.g. `dsh 1.2.3` → `1.2.3`).
fn dsh_current_version(binary_path: &Path) -> Option<String> {
    let output = dsh_version_command(binary_path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version_token(&stdout)
}

/// Build the `dsh --version` command. On Windows, run the real `node` + dsh
/// entry directly (no npm/volta shim) with `CREATE_NO_WINDOW` so the shim chain
/// (`cmd.exe → volta.exe → node.exe`) does not open a visible console window —
/// the "window that flashes by" seen at agent startup. Falls back to the shim
/// path when the direct launch cannot be resolved.
#[cfg(windows)]
fn dsh_version_command(binary_path: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    if let Some((node, entry)) = dsh_bridge::resolve_dsh_launch() {
        let mut command = Command::new(node);
        command.arg(entry).arg("--version");
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    } else {
        let mut command = Command::new(binary_path);
        command.arg("--version");
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }
}

#[cfg(not(windows))]
fn dsh_version_command(binary_path: &Path) -> Command {
    let mut command = Command::new(binary_path);
    command.arg("--version");
    command
}

/// Extract the first semver-looking token (`1.2.3`, `v1.2.3`) from version
/// output; returns the bare version without a leading `v`.
fn parse_version_token(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| {
            let t = token.trim_start_matches('v');
            t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.')
        })
        .map(|token| token.trim_start_matches('v').to_string())
}

/// Build the `npm view` command used for the latest-version check.
///
/// Resolve npm to an absolute path and inject the augmented search PATH:
/// GUI-launched apps (Finder/Dock) inherit a minimal PATH without
/// /opt/homebrew/bin or version-manager dirs, so a bare `Command::new("npm")`
/// fails to locate npm and the version check silently returns None.
/// On Windows, prefer running the real `node` + `npm-cli.js` directly so the
/// npm command-line wrapper (`.cmd`/Volta shim) does not flash a console
/// window during the settings version check. The shim fallback is also
/// created with `CREATE_NO_WINDOW` for systems where the direct launch cannot
/// be resolved.
fn npm_view_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some((node, entry)) = dsh_bridge::resolve_npm_launch() {
            let mut command = Command::new(node);
            command.arg(entry);
            if let Ok(joined) = std::env::join_paths(search_paths()) {
                command.env("PATH", joined);
            }
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            return command;
        }
    }

    let resolved = search_paths()
        .iter()
        .map(|dir| dir.join(npm_program()))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(npm_program()));
    let mut command = Command::new(resolved);
    if let Ok(joined) = std::env::join_paths(search_paths()) {
        command.env("PATH", joined);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    command
}

/// Query the npm registry for the latest published `dsh` version.
fn dsh_latest_version() -> Option<String> {
    let output = npm_view_command()
        .args(["view", DSH_NPM_PACKAGE, "version"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Check for a DeepSeek Harness update: returns `(current, latest)` versions.
/// `current` comes from `dsh --version` (None when the CLI is missing or its
/// version cannot be parsed); `latest` comes from the npm registry (None when
/// the query fails or dsh is not installed).
pub fn dsh_version_info() -> (Option<String>, Option<String>) {
    let definition = definition(AgentCliId::DeepSeekHarness).expect("dsh agent id");
    let detected_path = find_binary(definition.binary);
    let current = detected_path.as_deref().and_then(dsh_current_version);
    let latest = detected_path
        .is_some()
        .then(|| dsh_latest_version())
        .flatten();
    (current, latest)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AgentCliDefinition {
    pub(super) id: AgentCliId,
    pub(super) label: &'static str,
    pub(super) binary: &'static str,
    pub(super) acp_arg: &'static str,
}

pub(super) const AGENTS: &[AgentCliDefinition] = &[
    AgentCliDefinition {
        id: AgentCliId::CodexAcp,
        label: "Codex",
        binary: "codex-acp",
        acp_arg: "",
    },
    AgentCliDefinition {
        id: AgentCliId::ClaudeAgentAcp,
        label: "Claude",
        binary: "claude-agent-acp",
        acp_arg: "",
    },
    AgentCliDefinition {
        id: AgentCliId::Codebuddy,
        label: "CodeBuddy",
        binary: "codebuddy",
        acp_arg: "--acp",
    },
    /// DeepSeek Harness — not an ACP agent. `binary` is the `dsh` CLI (installed
    /// via `npm i -g @deepseek-ai/dsh`); `acp_arg` is unused (harness backend
    /// takes a different path, see `is_deepseek_harness_agent`).
    AgentCliDefinition {
        id: AgentCliId::DeepSeekHarness,
        label: "DeepSeek Harness",
        binary: "dsh",
        acp_arg: "",
    },
];

pub fn resolve_agent_command_with_settings(paths: &AppPaths) -> String {
    if let Ok(command) = std::env::var("ACP_AGENT_COMMAND") {
        return command;
    }

    command_for_agent_with_paths(default_agent_for_new_work(paths), paths)
        .unwrap_or_else(acp_core::platform_default_agent_command)
}

pub fn default_agent_for_new_work(paths: &AppPaths) -> AgentCliId {
    let settings = load_app_settings(paths);
    if settings.selected_agent == AgentCliId::ClaudeAgentAcp
        && !claude_agent_configured_for_settings(paths, &settings)
    {
        if codex_agent_configured_for_settings(paths, &settings)
            && detect_agent_with_paths(paths, AgentCliId::CodexAcp).installed
        {
            return AgentCliId::CodexAcp;
        }
        return settings.selected_agent;
    }
    settings.selected_agent
}

pub fn command_for_agent(agent: AgentCliId) -> Option<String> {
    let def = definition(agent)?;
    let status = detect_agent(agent);
    if let Some(path) = status.detected_path {
        return Some(command_from_binary(&shell_quote_path(&path), def.acp_arg));
    }
    Some(command_from_binary(&binary_name(def.binary), def.acp_arg))
}

pub fn remote_linux_command_for_agent(agent: AgentCliId) -> Option<String> {
    let def = definition(agent)?;
    Some(command_from_binary(def.binary, def.acp_arg))
}

pub fn is_deepseek_harness_agent(agent: AgentCliId) -> bool {
    agent == AgentCliId::DeepSeekHarness
}

/// Whether an agent command string selects the DeepSeek Harness backend.
/// `command_for_agent_with_paths` returns the `dsh` binary name for
/// `AgentCliId::DeepSeekHarness`, so a command whose basename is `dsh` selects
/// the harness backend (which runs `dsh web` instead of an ACP subprocess).
pub fn is_deepseek_harness_command(command: &str) -> bool {
    let command = command.trim();
    let basename = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim()
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".bat");
    basename.eq_ignore_ascii_case("dsh")
}

pub fn command_for_agent_with_paths(agent: AgentCliId, paths: &AppPaths) -> Option<String> {
    // DeepSeek Harness is not an ACP subprocess — `harness_endpoint` selects
    // the backend — but `agent_command` still carries the `dsh` binary name so
    // label inference (`agent_label_for_command`) and session persistence see
    // "DeepSeek Harness" instead of falling back to the default.
    if is_deepseek_harness_agent(agent) {
        return Some(binary_name("dsh"));
    }
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
    } else if agent == AgentCliId::ClaudeAgentAcp {
        Some(claude_agent_acp_command(paths))
    } else {
        command_for_agent(agent)
    }
}

pub fn detect_agent(agent: AgentCliId) -> AgentCliStatus {
    let definition = definition(agent).expect("supported agent id");
    let detected_path = find_binary(definition.binary);
    // DeepSeek Harness: do NOT spawn `dsh --version` here. The settings page
    // fetches the installed version on demand (when the dsh tab is opened) via
    // `dsh_version_info`, so a plain settings snapshot / app startup does not
    // pay for an extra subprocess + npm registry round-trip.
    AgentCliStatus {
        id: definition.id,
        label: definition.label.to_string(),
        binary: binary_name(definition.binary),
        installed: detected_path.is_some(),
        detected_path,
        selected: false,
        current_version: None,
        latest_version: None,
    }
}

pub fn detect_agent_with_paths(paths: &AppPaths, agent: AgentCliId) -> AgentCliStatus {
    if agent == AgentCliId::ClaudeAgentAcp {
        let definition = definition(agent).expect("supported agent id");
        let detected_path = claude_agent_acp_detected_path(paths);
        return AgentCliStatus {
            id: definition.id,
            label: definition.label.to_string(),
            binary: binary_name(definition.binary),
            installed: detected_path.is_some(),
            detected_path,
            selected: false,
            current_version: None,
            latest_version: None,
        };
    }
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
        current_version: None,
        latest_version: None,
    }
}

pub(super) fn agent_statuses(paths: &AppPaths, selected_agent: AgentCliId) -> Vec<AgentCliStatus> {
    AGENTS
        .iter()
        .filter(|definition| definition.id != AgentCliId::Codebuddy)
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

pub fn agent_label_for_id(agent: AgentCliId) -> Option<&'static str> {
    definition(agent).map(|definition| definition.label)
}

pub fn agent_id_for_label(label: &str) -> Option<AgentCliId> {
    definition_for_label(label).map(|definition| definition.id)
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
    definition_for_label(label).and_then(|agent| command_for_agent(agent.id))
}

pub fn command_for_agent_label_with_paths(label: &str, paths: &AppPaths) -> Option<String> {
    definition_for_label(label).and_then(|agent| command_for_agent_with_paths(agent.id, paths))
}

pub fn remote_linux_command_for_agent_label(label: &str) -> Option<String> {
    definition_for_label(label).and_then(|agent| remote_linux_command_for_agent(agent.id))
}

fn definition_for_label(label: &str) -> Option<&'static AgentCliDefinition> {
    let normalized = label.trim().to_lowercase();
    AGENTS.iter().find(|agent| {
        normalized == agent.label.to_lowercase() || normalized == agent.binary.to_lowercase()
    })
}

pub fn agent_env_for_command(command: &str, paths: &AppPaths) -> Vec<(String, String)> {
    if is_claude_agent_acp_command(command) {
        return claude_agent_acp_env(paths);
    }

    if !is_codex_acp_command(command) {
        return Vec::new();
    }
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        return Vec::new();
    }

    let _ = refresh_codex_acp_config_for_launch(paths);
    let config_path = codex_config_path(paths);
    let _ = ensure_codex_acp_env_key(&config_path);
    let provider_keys = codex_provider_keys(paths);
    let mut env = if provider_keys.is_empty() {
        codex_active_provider_key(&config_path)
            .map(|(env_key, api_key)| vec![(env_key, api_key)])
            .unwrap_or_default()
    } else {
        provider_keys
    };
    let provider_map = codex_model_provider_map_env(paths);
    sync_codex_api_proxy_model_provider_map(provider_map.as_ref().map(|(_, value)| value.as_str()));
    if let Some(provider_map) = provider_map {
        env.push(provider_map);
    }
    env
}

pub fn remote_agent_env_for_command(
    command: &str,
    paths: &AppPaths,
    remote_home: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = agent_env_for_command(command, paths);
    if is_codex_acp_command(command)
        && load_app_settings(paths).codex_connection_mode != CodexConnectionMode::Default
    {
        ensure_codex_proxy_from_provider_key_env(&env);
        if let Some(codex_home) = remote_home.and_then(remote_codex_home) {
            env.push((CODEX_HOME_ENV.to_string(), codex_home));
        }
        scrub_remote_codex_provider_key_env(&mut env);
    }
    env
}

pub fn remote_codex_home(remote_home: &str) -> Option<String> {
    let home = remote_home.trim();
    (!home.is_empty()).then(|| format!("{}/.kodex", home.trim_end_matches('/')))
}

pub fn remote_codex_proxy_config(
    paths: &AppPaths,
    remote_codex_home: Option<&str>,
) -> Result<Option<String>> {
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        return Ok(None);
    }
    let path = codex_config_path(paths);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read Codex config {}", path.display()))?;
    let mut doc = content
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse Codex config {}", path.display()))?;
    scrub_codex_config_api_keys(&mut doc);
    scrub_remote_codex_local_paths(&mut doc);
    if let Some(remote_codex_home) = remote_codex_home {
        doc["model_catalog_json"] = value(format!(
            "{}/model_catalog.json",
            remote_codex_home.trim_end_matches('/')
        ));
    }
    Ok(Some(doc.to_string()))
}

pub fn ensure_agent_ready_for_command(command: &str, paths: &AppPaths) -> Result<()> {
    let settings = load_app_settings(paths);
    if is_codex_acp_command(command) {
        refresh_codex_acp_config_for_launch(paths)?;
        return Ok(());
    }
    if !is_claude_agent_acp_command(command) {
        return Ok(());
    }
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    if selected_profile_id == BYOK_PROVIDER_ID {
        if !configured_claude_byok_source_keys(paths).is_empty() {
            return Ok(());
        }
        anyhow::bail!("请先在 BYOK 模型池中保存至少一个 API key");
    }
    if provider_secret(paths, AgentProviderFamily::Claude, &selected_profile_id)
        .map(|secret| !secret.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    anyhow::bail!(
        "请先填写并保存 {} API key",
        profile_definition(AgentProviderFamily::Claude, &selected_profile_id)
            .map(|profile| profile.label)
            .unwrap_or("Claude provider")
    );
}

pub fn is_claude_agent_acp_command(command: &str) -> bool {
    command.to_ascii_lowercase().contains("claude-agent-acp")
}

pub fn is_codex_acp_command(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("codex-acp") || command.contains("kodex-acp")
}

fn scrub_remote_codex_provider_key_env(env: &mut [(String, String)]) {
    for (name, value) in env {
        if matches!(
            name.as_str(),
            BYOK_API_KEY_ENV
                | TIMIAI_API_KEY_ENV
                | COMMANDCODE_API_KEY_ENV
                | DEEPSEEK_API_KEY_ENV
                | KIMI_API_KEY_ENV
                | MIMO_API_KEY_ENV
        ) {
            *value = "kodex-proxy".to_string();
        }
    }
}

fn ensure_codex_proxy_from_provider_key_env(env: &[(String, String)]) {
    for (env_key, provider) in [
        (TIMIAI_API_KEY_ENV, TIMIAI_PROVIDER_ID),
        (COMMANDCODE_API_KEY_ENV, COMMANDCODE_PROVIDER_ID),
        (DEEPSEEK_API_KEY_ENV, DEEPSEEK_PROVIDER_ID),
        (KIMI_API_KEY_ENV, KIMI_PROVIDER_ID),
        (MIMO_API_KEY_ENV, MIMO_PROVIDER_ID),
    ] {
        let Some((_, api_key)) = env.iter().find(|(name, _)| name == env_key) else {
            continue;
        };
        let api_key = api_key.trim();
        if api_key.is_empty() || api_key == "kodex-proxy" {
            continue;
        }
        acp_core::ensure_codex_api_proxy(provider, api_key);
    }
}

fn scrub_codex_config_api_keys(doc: &mut DocumentMut) {
    let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(Item::as_table_like_mut)
    else {
        return;
    };
    for (_, provider) in providers.iter_mut() {
        if let Some(table) = provider.as_table_like_mut() {
            table.insert("api_key", value("kodex-proxy"));
        }
    }
}

fn scrub_remote_codex_local_paths(doc: &mut DocumentMut) {
    for key in [
        "model_catalog_json",
        "model_instructions_file",
        "experimental_instructions_file",
    ] {
        doc.remove(key);
    }

    let Some(profiles) = doc.get_mut("profiles").and_then(Item::as_table_like_mut) else {
        return;
    };
    for (_, profile) in profiles.iter_mut() {
        if let Some(table) = profile.as_table_like_mut() {
            table.remove("model_instructions_file");
            table.remove("experimental_instructions_file");
        }
    }
}

pub(super) fn claude_agent_configured_for_settings(
    paths: &AppPaths,
    settings: &AppSettings,
) -> bool {
    let selected_profile_id = selected_claude_provider_profile_id(settings);
    if selected_profile_id == BYOK_PROVIDER_ID {
        return !configured_claude_byok_source_keys(paths).is_empty();
    }
    provider_secret(paths, AgentProviderFamily::Claude, &selected_profile_id)
        .map(|secret| !secret.trim().is_empty())
        .unwrap_or(false)
}

pub(super) fn codex_agent_configured_for_settings(
    paths: &AppPaths,
    settings: &AppSettings,
) -> bool {
    if settings.codex_connection_mode == CodexConnectionMode::Default {
        return false;
    }
    let selected_profile_id = selected_codex_provider_profile_id(paths, settings);
    if selected_profile_id == CODEX_DEFAULT_PROVIDER_ID {
        return false;
    }
    if selected_profile_id == BYOK_PROVIDER_ID {
        return !configured_codex_byok_models(paths).is_empty();
    }
    provider_secret(paths, AgentProviderFamily::Codex, &selected_profile_id)
        .map(|secret| !secret.trim().is_empty())
        .unwrap_or(false)
}

fn command_from_binary(binary: &str, acp_arg: &str) -> String {
    if acp_arg.is_empty() {
        binary.to_string()
    } else {
        format!("{binary} {acp_arg}")
    }
}

fn claude_agent_acp_command(paths: &AppPaths) -> String {
    claude_agent_acp_detected_path(paths)
        .map(|path| shell_quote_path(&path))
        .unwrap_or_else(|| binary_name("claude-agent-acp"))
}

fn claude_agent_acp_env(paths: &AppPaths) -> Vec<(String, String)> {
    let settings = load_app_settings(paths);
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    let selected_profile = profile_definition(AgentProviderFamily::Claude, &selected_profile_id);
    let byok_model_entries = (selected_profile_id == BYOK_PROVIDER_ID)
        .then(|| configured_claude_byok_model_entries(paths));
    let available_models = if let Some(entries) = &byok_model_entries {
        entries.iter().map(|(model, _)| model.clone()).collect()
    } else {
        selected_profile
            .map(|profile| {
                profile
                    .models
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mut env = Vec::new();
    let fast_model_slug = byok_model_entries
        .as_ref()
        .and_then(|entries| selected_claude_fast_model_slug(&settings, entries));
    if available_models.is_empty() {
    } else {
        let model_config = if let Some(entries) = &byok_model_entries {
            claude_model_config_for_byok_entries(entries, fast_model_slug.as_deref())
        } else if selected_profile_id == TIMIAI_PROVIDER_ID {
            claude_model_config_for_provider(&available_models, TIMIAI_PROVIDER_ID)
        } else {
            claude_model_config(&available_models)
        };
        if let Ok(value) = serde_json::to_string(&model_config) {
            env.push(("CLAUDE_MODEL_CONFIG".to_string(), value));
        }
    }

    if selected_profile_id == BYOK_PROVIDER_ID {
        let configured_sources = configured_claude_byok_source_keys(paths);
        for (provider, key) in &configured_sources {
            acp_core::ensure_codex_api_proxy(provider, key);
        }
        let provider_map = claude_model_provider_map_env(paths);
        sync_codex_api_proxy_model_provider_map(
            provider_map.as_ref().map(|(_, value)| value.as_str()),
        );
        if let Some((name, value)) = provider_map {
            env.push((name, value));
        }
        if let Some(default_model) = default_claude_byok_model(&available_models) {
            env.push(("ANTHROPIC_MODEL".to_string(), default_model.to_string()));
        }
        if let Some(fast_model_slug) = fast_model_slug {
            env.push(("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), fast_model_slug));
        }
        env.push(("ANTHROPIC_API_KEY".to_string(), "byok".to_string()));
        env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), "byok".to_string()));
        env.push(("AUTH_TOKEN".to_string(), "byok".to_string()));
        env.push((
            "ANTHROPIC_BASE_URL".to_string(),
            claude_provider_proxy_base_url(),
        ));
        env.push((
            "CLAUDE_PROVIDER_PROXY_KIND".to_string(),
            "byok_proxy".to_string(),
        ));
        return env;
    }

    let Some(profile) = selected_profile else {
        return env;
    };
    if let Some(secret) = provider_secret(paths, AgentProviderFamily::Claude, profile.id) {
        acp_core::ensure_codex_api_proxy(profile.id, &secret);
        env.push(("ANTHROPIC_API_KEY".to_string(), profile.id.to_string()));
        env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), profile.id.to_string()));
        env.push(("AUTH_TOKEN".to_string(), profile.id.to_string()));
    }
    env.push((
        "ANTHROPIC_BASE_URL".to_string(),
        claude_provider_proxy_base_url_for_provider(profile.id),
    ));
    if let Some(model) = profile.default_model {
        env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
    }
    env.push((
        "CLAUDE_PROVIDER_PROXY_KIND".to_string(),
        claude_proxy_kind_env(profile.proxy_kind).to_string(),
    ));
    env
}

fn claude_agent_acp_detected_path(paths: &AppPaths) -> Option<PathBuf> {
    let managed = claude_agent_acp_binary_path(paths);
    if managed.is_file() {
        return Some(managed);
    }
    if cfg!(windows) {
        let cmd = codex_acp_bin_dir(paths).join("claude-agent-acp.cmd");
        if cmd.is_file() {
            return Some(cmd);
        }
    }
    let package_dir = claude_agent_acp_package_dir(paths);
    if package_dir.is_dir() {
        let launcher = codex_acp_bin_dir(paths).join(if cfg!(windows) {
            "claude-agent-acp.cmd"
        } else {
            "claude-agent-acp"
        });
        if launcher.is_file() {
            return Some(launcher);
        }
    }
    find_binary("claude-agent-acp")
}

pub(super) fn binary_name(binary: &str) -> String {
    if cfg!(windows) {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    }
}

/// Directories searched to locate bundled CLIs (and their runtimes).
///
/// Starts with the current process `PATH`, then adds common user-local and
/// platform locations that GUI-launched apps (Finder/Dock on macOS, or a
/// Tauri child process) do not inherit. Exposed so callers that spawn child
/// processes (e.g. the codebuddy proxy) can build a `PATH` that lets the
/// child resolve `node` / the CLI the same way [`find_binary`] does.
pub fn search_paths() -> Vec<PathBuf> {
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

    // Windows: `npm install -g <pkg>` puts the binary under
    // `%LOCALAPPDATA%\<pkg>\bin\` and adds that directory to the user's
    // interactive shell PATH, but Tauri-spawned children do not always
    // inherit the modified PATH. Look up the npm-global roots explicitly
    // so `find_binary("codebuddy")` still resolves when launching the
    // bundled proxy as a child process.
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            for suffix in ["codebuddy/bin", "Programs/codebuddy/bin"] {
                let p = PathBuf::from(&local_app_data).join(suffix);
                if !search_paths.contains(&p) {
                    search_paths.push(p);
                }
            }
        }
        if let Some(app_data) = std::env::var_os("APPDATA") {
            for suffix in ["npm"] {
                let p = PathBuf::from(&app_data).join(suffix);
                if !search_paths.contains(&p) {
                    search_paths.push(p);
                }
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
        // Common version-manager install roots; GUI-launched apps do not
        // inherit the shell hooks that put these on PATH.
        if let Some(home) = dirs_next::home_dir() {
            for suffix in [".volta/bin", ".local/share/mise/shims", ".asdf/shims"] {
                let p = home.join(suffix);
                if !search_paths.contains(&p) {
                    search_paths.push(p);
                }
            }
            // nvm installs each Node version under its own bin directory.
            if let Some(entries) = nvm_version_bin_dirs(&home) {
                for p in entries {
                    if !search_paths.contains(&p) {
                        search_paths.push(p);
                    }
                }
            }
        }
    }

    search_paths
}

/// nvm keeps each installed Node version at `~/.nvm/versions/node/vX.Y.Z/bin`.
/// Return those bin directories, newest version first, so a GUI-launched app
/// can find `node`/`npm`/shims installed through nvm without the shell hook.
#[cfg(target_os = "macos")]
fn nvm_version_bin_dirs(home: &Path) -> Option<Vec<PathBuf>> {
    let versions_dir = home.join(".nvm/versions/node");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&versions_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    if dirs.is_empty() { None } else { Some(dirs) }
}

fn find_binary(binary: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            format!("{binary}.bat"),
        ]
    } else {
        vec![binary.to_string()]
    };

    search_paths()
        .into_iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())
}

fn shell_quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_version_token;

    #[test]
    fn parses_bare_and_prefixed_versions() {
        assert_eq!(parse_version_token("1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(parse_version_token("dsh 1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(parse_version_token("dsh v0.4.0"), Some("0.4.0".to_string()));
        assert_eq!(
            parse_version_token("dsh, version 0.5.1-beta.2"),
            Some("0.5.1-beta.2".to_string())
        );
        assert_eq!(parse_version_token("dsh"), None);
        assert_eq!(parse_version_token(""), None);
    }
}
