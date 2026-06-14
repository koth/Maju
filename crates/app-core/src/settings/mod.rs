use crate::AppPaths;
mod agent_cli;
mod lsp;
mod remote;

pub use agent_cli::{
    agent_env_for_command, agent_id_for_label, agent_label_for_command, agent_label_for_id,
    command_for_agent, command_for_agent_label, command_for_agent_label_with_paths,
    command_for_agent_with_paths, default_agent_for_new_work, detect_agent,
    detect_agent_with_paths, ensure_agent_ready_for_command, is_claude_agent_acp_command,
    is_codex_acp_command, remote_agent_env_for_command, remote_codex_home,
    remote_codex_proxy_config, remote_linux_command_for_agent,
    remote_linux_command_for_agent_label, resolve_agent_command_with_settings,
};

use agent_cli::{agent_statuses, binary_name};

pub use lsp::{
    DEFAULT_LSP_SERVERS, DefaultLspServerDefinition, EffectiveLspServerConfig,
    all_effective_lsp_servers, effective_lsp_server, reset_lsp_server_config,
    save_lsp_server_config,
};

pub use remote::{
    remote_lsp_settings_snapshot, remote_probe_lsp_server, remote_reset_lsp_server_config,
    remote_reset_provider_models, remote_save_agent_provider_secret, remote_save_lsp_server_config,
    remote_save_provider_models, remote_select_agent, remote_select_agent_provider_profile,
    remote_select_claude_fast_model, remote_select_theme, remote_settings_snapshot,
};

#[cfg(test)]
use remote::{
    remote_select_agent_with_runner, remote_settings_snapshot_with_runner,
    remote_update_settings_with_runner,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};
use workspace_model::{
    AgentCliId, AgentModelOption, AgentProviderFamily, AgentProviderProfile,
    AgentProviderProxyKind, AgentSettingsSnapshot, AppSettings, AppTheme, ClaudeProviderSettings,
    ClaudeProviderSettingsStatus, CodexAcpSettingsStatus, CodexConnectionMode,
};

const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_SECRETS_FILE: &str = "provider-secrets.json";
const PROVIDER_MODELS_FILE: &str = "provider-models.json";
const PROVIDER_MODELS_VERSION: u32 = 1;
const KODEX_MODEL_PROVIDER_MAP_ENV: &str = "KODEX_MODEL_PROVIDER_MAP";
const CODEX_CONFIG_FILE: &str = "config.toml";
const CODEX_HOME_ENV: &str = "CODEX_HOME";
const CODEX_DEFAULT_PROVIDER_ID: &str = "default";
const BYOK_PROVIDER_ID: &str = "byok";
const BYOK_PROVIDER_NAME: &str = "BYOK";
const BYOK_API_KEY_ENV: &str = "BYOK_API_KEY";
const BYOK_SOURCE_PROVIDER_IDS: &[&str] = &[
    TIMIAI_PROVIDER_ID,
    COMMANDCODE_PROVIDER_ID,
    DEEPSEEK_PROVIDER_ID,
    KIMI_PROVIDER_ID,
    MIMO_PROVIDER_ID,
];
const CODEX_PROXY_WIRE_API: &str = "responses";
const COMMANDCODE_PROVIDER_ID: &str = "commandcode";
const COMMANDCODE_PROVIDER_NAME: &str = "CommandCode";
const COMMANDCODE_MODEL: &str = "claude-sonnet-4-6";
const COMMANDCODE_API_KEY_ENV: &str = "COMMANDCODE_API_KEY";
const COMMANDCODE_BASE_URL: &str = "https://api.commandcode.ai/provider/v1";
const DEEPSEEK_MODEL: &str = "deepseek-v4-pro";
const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
const DEEPSEEK_PROVIDER_NAME: &str = "DeepSeek";
const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const DEEPSEEK_UPSTREAM_HELP_URL: &str = "https://api.deepseek.com/v1/chat/completions";
const KIMI_PROVIDER_ID: &str = "kimi_code";
const KIMI_PROVIDER_NAME: &str = "Kimi Code";
const KIMI_MODEL: &str = "kimi-for-coding";
const KIMI_API_KEY_ENV: &str = "KIMI_CODE_API_KEY";
const KIMI_CODE_OPENAI_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const KIMI_CODE_ANTHROPIC_BASE_URL: &str = "https://api.kimi.com/coding/";
const MIMO_PROVIDER_ID: &str = "xiaomi_mimo";
const MIMO_PROVIDER_NAME: &str = "Xiaomi Token Plan";
const MIMO_MODEL: &str = "MiMo-V2.5-Pro";
const MIMO_API_KEY_ENV: &str = "XIAOMI_MIMO_API_KEY";
const MIMO_OPENAI_BASE_URL: &str = "https://token-plan-cn.xiaomimimo.com/v1";
const MIMO_ANTHROPIC_BASE_URL: &str = "https://token-plan-cn.xiaomimimo.com/anthropic";
const TIMIAI_PROVIDER_ID: &str = "timiai";
const TIMIAI_PROVIDER_NAME: &str = "TimiAI";
const TIMIAI_API_KEY_ENV: &str = "TIMIAI_API_KEY";
const TIMIAI_BASE_URL: &str = "http://api.timiai.woa.com/ai_api_manage/llmproxy";
const TIMIAI_CODEX_MODEL: &str = "gpt-5.5";
const TIMIAI_CLAUDE_MODEL: &str = "claude-opus-4.8";
const TIMIAI_CATALOG_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "claude-opus-4.8",
    "claude-opus-4.7",
    "claude-opus-4.6",
    "claude-sonnet-4.6",
    "gemini-3.5-flash",
];
const CLAUDE_FAST_MODEL_ALIASES: &[&str] =
    &["haiku", "claude-haiku-4-5", "claude-haiku-4-5-20251001"];
const DEFAULT_MODEL_CONTEXT_WINDOW: i64 = 200_000;
const DEFAULT_MODEL_MAX_OUTPUT_TOKENS: i64 = 128_000;
const KIMI_MODEL_CONTEXT_WINDOW: i64 = 262_144;
const KIMI_MODEL_MAX_OUTPUT_TOKENS: i64 = 32_768;
const MIMO_MODEL_CONTEXT_WINDOW: i64 = 1_000_000;
const MIMO_MODEL_MAX_OUTPUT_TOKENS: i64 = 128_000;
const CODEX_AUTH_METHOD_API_KEY: &str = "apikey";
const CODEX_REASONING_EFFORT_NONE: &str = "none";
const PROVIDER_MODEL_ID_PREFIX: &str = "kodex-provider/";

const DEEPSEEK_CATALOG_MODELS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];
const COMMANDCODE_CATALOG_MODELS: &[&str] = &[
    "claude-sonnet-4-6",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-haiku-4-5-20251001",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.3-codex",
    "gpt-5.4-mini",
    "Qwen/Qwen3.7-Max-Free",
    "moonshotai/Kimi-K2.6",
    "moonshotai/Kimi-K2.5",
    "zai-org/GLM-5.1",
    "zai-org/GLM-5",
    "MiniMaxAI/MiniMax-M3",
    "MiniMaxAI/MiniMax-M2.7",
    "MiniMaxAI/MiniMax-M2.5",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash",
    "Qwen/Qwen3.6-Max-Preview",
    "Qwen/Qwen3.6-Plus",
    "Qwen/Qwen3.7-Max",
    "stepfun/Step-3.7-Flash",
    "stepfun/Step-3.5-Flash",
    "xiaomi/mimo-v2.5-pro",
    "xiaomi/mimo-v2.5",
    "google/gemini-3.5-flash",
    "google/gemini-3.1-flash-lite",
];
const COMMANDCODE_MODEL_CONTEXT_WINDOWS: &[(&str, i64)] = &[
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-haiku-4-5-20251001", 200_000),
    ("gpt-5.5", 200_000),
    ("gpt-5.4", 400_000),
    ("gpt-5.3-codex", 400_000),
    ("gpt-5.4-mini", 400_000),
    ("Qwen/Qwen3.7-Max-Free", 1_000_000),
    ("moonshotai/Kimi-K2.6", 256_000),
    ("moonshotai/Kimi-K2.5", 256_000),
    ("zai-org/GLM-5.1", 200_000),
    ("zai-org/GLM-5", 200_000),
    ("MiniMaxAI/MiniMax-M3", 1_000_000),
    ("MiniMaxAI/MiniMax-M2.7", 204_800),
    ("MiniMaxAI/MiniMax-M2.5", 204_800),
    ("deepseek/deepseek-v4-pro", 1_000_000),
    ("deepseek/deepseek-v4-flash", 1_000_000),
    ("Qwen/Qwen3.6-Max-Preview", 256_000),
    ("Qwen/Qwen3.6-Plus", 1_000_000),
    ("Qwen/Qwen3.7-Max", 1_000_000),
    ("stepfun/Step-3.7-Flash", 256_000),
    ("stepfun/Step-3.5-Flash", 1_000_000),
    ("xiaomi/mimo-v2.5-pro", 1_000_000),
    ("xiaomi/mimo-v2.5", 1_000_000),
    ("google/gemini-3.5-flash", 1_000_000),
    ("google/gemini-3.1-flash-lite", 1_000_000),
];
const COMMANDCODE_MODEL_MAX_OUTPUT_TOKENS: &[(&str, i64)] = &[
    ("Qwen/Qwen3.7-Max-Free", 65_536),
    ("MiniMaxAI/MiniMax-M3", 128_000),
    ("MiniMaxAI/MiniMax-M2.7", 128_000),
    ("MiniMaxAI/MiniMax-M2.5", 128_000),
    ("Qwen/Qwen3.6-Max-Preview", 65_536),
    ("Qwen/Qwen3.6-Plus", 65_536),
    ("Qwen/Qwen3.7-Max", 65_536),
];
const KIMI_CATALOG_MODELS: &[&str] = &[KIMI_MODEL];
const MIMO_CATALOG_MODELS: &[&str] = &["MiMo-V2.5-Pro", "MiMo-V2.5"];
const TIMIAI_MODEL_SLUG_MAP: &[(&str, &str)] = &[
    ("gpt-5.2", "gpt-5.2"),
    ("gpt-5.3", "gpt-5.3"),
    ("gpt-5.4", "gpt-5.4"),
    ("gpt-5.5", "gpt-5.5"),
    ("claude-opus-4.5", "claude-opus-4-5-20251101"),
    ("claude-sonnet-4.5", "claude-4-5-sonnet-20250929"),
    ("claude-opus-4.6", "claude-opus-4-6"),
    ("claude-sonnet-4.6", "claude-sonnet-4-6"),
    ("claude-opus-4.7", "claude-opus-4-7"),
    ("claude-opus-4.8", "claude-opus-4-8"),
];

const MIMO_MODEL_SLUG_MAP: &[(&str, &str)] = &[
    ("MiMo-V2.5-Pro", "mimo-v2.5-pro"),
    ("MiMo-V2.5", "mimo-v2.5"),
];

const MODEL_CONTEXT_WINDOWS: &[(&str, i64)] = &[
    ("gpt-5.3-codex", 200_000),
    ("gpt-5.2-codex", 200_000),
    ("gpt-5.1-codex-max", 1_000_000),
    ("gpt-5.1-codex-mini", 128_000),
    ("gpt-5.2", 400_000),
    ("gpt-5.3", 128_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("claude-opus-4.5", 200_000),
    ("claude-sonnet-4.5", 200_000),
    ("claude-opus-4.6", 1_000_000),
    ("claude-sonnet-4.6", 1_000_000),
    ("claude-opus-4.7", 1_000_000),
    ("claude-opus-4.8", 1_000_000),
    ("gemini-3.5-flash", 1_000_000),
    ("deepseek-v4-pro", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    (KIMI_MODEL, KIMI_MODEL_CONTEXT_WINDOW),
    ("MiMo-V2.5-Pro", MIMO_MODEL_CONTEXT_WINDOW),
    ("MiMo-V2.5", MIMO_MODEL_CONTEXT_WINDOW),
];

const MODEL_MAX_OUTPUT_TOKENS: &[(&str, i64)] = &[
    ("gpt-5.3-codex", 128_000),
    ("gpt-5.2-codex", 128_000),
    ("gpt-5.1-codex-max", 128_000),
    ("gpt-5.1-codex-mini", 64_000),
    ("gpt-5.2", 128_000),
    ("gpt-5.3", 16_384),
    ("gpt-5.4", 128_000),
    ("gpt-5.5", 128_000),
    ("claude-opus-4.5", 64_000),
    ("claude-sonnet-4.5", 64_000),
    ("claude-opus-4.6", 128_000),
    ("claude-sonnet-4.6", 64_000),
    ("claude-opus-4.7", 128_000),
    ("claude-opus-4.8", 128_000),
    ("gemini-3.5-flash", 128_000),
    ("deepseek-v4-pro", 384_000),
    ("deepseek-v4-flash", 384_000),
    (KIMI_MODEL, KIMI_MODEL_MAX_OUTPUT_TOKENS),
    ("MiMo-V2.5-Pro", MIMO_MODEL_MAX_OUTPUT_TOKENS),
    ("MiMo-V2.5", MIMO_MODEL_MAX_OUTPUT_TOKENS),
];
const CODEX_ACP_BASE_INSTRUCTIONS: &str = r#"You are Codex, a coding agent. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled.

Use the available tools to inspect files, run commands, and edit the workspace when the request calls for action. Do not merely say that you will inspect or change something; perform the needed tool calls and then report the result.

When editing files, creating files, or deleting files, you MUST use the apply_patch tool. Do not use shell commands to modify the filesystem. Shell commands may only be used for reading, searching, building, testing, formatting, or inspecting files. Never use shell redirection, rm, mv, cp, sed -i, perl -pi, tee, truncate, or scripts to create, edit, move, or delete repository files.

When you need repository context, read the relevant files first. Keep responses concise, concrete, and grounded in what you actually observed or changed."#;

#[derive(Debug, Clone, Copy)]
struct ProviderProfileDefinition {
    family: AgentProviderFamily,
    id: &'static str,
    label: &'static str,
    proxy_kind: AgentProviderProxyKind,
    base_url: Option<&'static str>,
    default_model: Option<&'static str>,
    models: &'static [&'static str],
    credential_label: Option<&'static str>,
    requires_credential: bool,
    help_text: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderModelsCatalog {
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, ProviderModelsEntry>,
}

impl Default for ProviderModelsCatalog {
    fn default() -> Self {
        Self {
            version: PROVIDER_MODELS_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProviderModelsEntry {
    #[serde(default)]
    models: Vec<String>,
}

const CODEX_PROVIDER_PROFILES: &[ProviderProfileDefinition] = &[
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: CODEX_DEFAULT_PROVIDER_ID,
        label: "默认",
        proxy_kind: AgentProviderProxyKind::CodexDefault,
        base_url: None,
        default_model: None,
        models: &[],
        credential_label: None,
        requires_credential: false,
        help_text: "不设置 CODEX_HOME，使用用户自己的 Codex 配置。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: TIMIAI_PROVIDER_ID,
        label: TIMIAI_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::Responses,
        base_url: Some(TIMIAI_BASE_URL),
        default_model: Some(TIMIAI_CODEX_MODEL),
        models: TIMIAI_CATALOG_MODELS,
        credential_label: Some("TimiAI key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 转发到 TimiAI Responses API，Codex / Claude 共用同一个 key。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: COMMANDCODE_PROVIDER_ID,
        label: COMMANDCODE_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: Some(COMMANDCODE_BASE_URL),
        default_model: Some(COMMANDCODE_MODEL),
        models: COMMANDCODE_CATALOG_MODELS,
        credential_label: Some("CommandCode API key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 将 Responses 请求转为 CommandCode chat completions。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: BYOK_PROVIDER_ID,
        label: BYOK_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: None,
        default_model: None,
        models: &[],
        credential_label: None,
        requires_credential: false,
        help_text: "用户自带 Key 的共享模型池，通过本机 proxy 按模型路由。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: DEEPSEEK_PROVIDER_ID,
        label: "DeepSeek",
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: Some(DEEPSEEK_UPSTREAM_HELP_URL),
        default_model: Some(DEEPSEEK_MODEL),
        models: DEEPSEEK_CATALOG_MODELS,
        credential_label: Some("DeepSeek API key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 将 Responses 请求转为 DeepSeek chat completions。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: KIMI_PROVIDER_ID,
        label: "Kimi Code",
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: Some(KIMI_CODE_OPENAI_BASE_URL),
        default_model: Some(KIMI_MODEL),
        models: KIMI_CATALOG_MODELS,
        credential_label: Some("Kimi API key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 将 Responses 请求转为 Kimi Code Anthropic Messages。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: MIMO_PROVIDER_ID,
        label: MIMO_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: Some(MIMO_OPENAI_BASE_URL),
        default_model: Some(MIMO_MODEL),
        models: MIMO_CATALOG_MODELS,
        credential_label: Some("Xiaomi Token Plan API key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 将 Responses 请求转为 Xiaomi Token Plan chat completions。",
    },
];

const CLAUDE_PROVIDER_PROFILES: &[ProviderProfileDefinition] = &[
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: TIMIAI_PROVIDER_ID,
        label: TIMIAI_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::ClaudeNative,
        base_url: Some(TIMIAI_BASE_URL),
        default_model: Some(TIMIAI_CLAUDE_MODEL),
        models: TIMIAI_CATALOG_MODELS,
        credential_label: Some("TimiAI key"),
        requires_credential: true,
        help_text: "通过本机 proxy 转发到 TimiAI Anthropic Messages API，Codex / Claude 共用同一个 key。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: COMMANDCODE_PROVIDER_ID,
        label: COMMANDCODE_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::ClaudeNative,
        base_url: Some(COMMANDCODE_BASE_URL),
        default_model: Some(COMMANDCODE_MODEL),
        models: COMMANDCODE_CATALOG_MODELS,
        credential_label: Some("CommandCode API key"),
        requires_credential: true,
        help_text: "通过本机 proxy 转发到 CommandCode Anthropic-compatible Messages API。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: BYOK_PROVIDER_ID,
        label: BYOK_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::ClaudeNative,
        base_url: None,
        default_model: None,
        models: &[],
        credential_label: None,
        requires_credential: false,
        help_text: "用户自带 Key 的共享模型池，通过本机 proxy 按模型路由。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: DEEPSEEK_PROVIDER_ID,
        label: "DeepSeek",
        proxy_kind: AgentProviderProxyKind::CompletionToClaude,
        base_url: Some(DEEPSEEK_UPSTREAM_HELP_URL),
        default_model: Some(DEEPSEEK_MODEL),
        models: DEEPSEEK_CATALOG_MODELS,
        credential_label: Some("DeepSeek API key"),
        requires_credential: true,
        help_text: "通过 completion-to-Claude 代理对接 DeepSeek。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: KIMI_PROVIDER_ID,
        label: "Kimi Code",
        proxy_kind: AgentProviderProxyKind::ClaudeNative,
        base_url: Some(KIMI_CODE_ANTHROPIC_BASE_URL),
        default_model: Some(KIMI_MODEL),
        models: KIMI_CATALOG_MODELS,
        credential_label: Some("Kimi API key"),
        requires_credential: true,
        help_text: "通过 Kimi Code Anthropic-compatible Messages API 对接 Claude Agent ACP。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: MIMO_PROVIDER_ID,
        label: MIMO_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::ClaudeNative,
        base_url: Some(MIMO_ANTHROPIC_BASE_URL),
        default_model: Some(MIMO_MODEL),
        models: MIMO_CATALOG_MODELS,
        credential_label: Some("Xiaomi Token Plan API key"),
        requires_credential: true,
        help_text: "通过 Xiaomi Token Plan Anthropic-compatible Messages API 对接 Claude Agent ACP。",
    },
];

fn default_settings() -> AppSettings {
    AppSettings {
        selected_agent: AgentCliId::ClaudeAgentAcp,
        acp_port: 0,
        theme: AppTheme::Graphite,
        lsp_servers: BTreeMap::new(),
        codex_connection_mode: CodexConnectionMode::Managed,
        selected_codex_provider_profile_id: None,
        selected_claude_provider_profile_id: Some(BYOK_PROVIDER_ID.to_string()),
        claude: ClaudeProviderSettings::default(),
    }
}

#[cfg(test)]
fn default_claude_available_models() -> Vec<String> {
    ["claude-opus-4-7[1m]", "claude-opus-4-6[1m]"]
        .iter()
        .map(|model| (*model).to_string())
        .collect()
}

pub fn load_app_settings(paths: &AppPaths) -> AppSettings {
    let path = settings_path(paths);
    let mut settings = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_else(default_settings);
    let migrated = migrate_app_settings(paths, &mut settings);
    if migrated && path.exists() {
        let _ = save_app_settings(paths, &settings);
    }
    settings
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

fn migrate_app_settings(paths: &AppPaths, settings: &mut AppSettings) -> bool {
    let mut changed = false;

    if settings.selected_agent == AgentCliId::Goose {
        settings.selected_agent = if detect_agent_with_paths(paths, AgentCliId::CodexAcp).installed
        {
            AgentCliId::CodexAcp
        } else {
            AgentCliId::Codebuddy
        };
        changed = true;
    }

    if settings.selected_codex_provider_profile_id.is_none() {
        settings.selected_codex_provider_profile_id =
            Some(infer_legacy_codex_provider_profile_id(paths, settings).to_string());
        changed = true;
    }
    if settings
        .selected_codex_provider_profile_id
        .as_deref()
        .is_some_and(codex_is_byok_source)
    {
        settings.selected_codex_provider_profile_id = Some(BYOK_PROVIDER_ID.to_string());
        changed = true;
    }
    if settings
        .selected_codex_provider_profile_id
        .as_deref()
        .is_some_and(|profile_id| {
            profile_definition(AgentProviderFamily::Codex, profile_id).is_none()
        })
    {
        settings.selected_codex_provider_profile_id =
            Some(infer_legacy_codex_provider_profile_id(paths, settings).to_string());
        changed = true;
    }

    if settings.selected_claude_provider_profile_id.is_none() {
        settings.selected_claude_provider_profile_id = Some(BYOK_PROVIDER_ID.to_string());
        changed = true;
    }
    if settings
        .selected_claude_provider_profile_id
        .as_deref()
        .is_some_and(claude_is_byok_source)
    {
        settings.selected_claude_provider_profile_id = Some(BYOK_PROVIDER_ID.to_string());
        changed = true;
    }
    if settings
        .selected_claude_provider_profile_id
        .as_deref()
        .is_some_and(|profile_id| {
            profile_definition(AgentProviderFamily::Claude, profile_id).is_none()
        })
    {
        settings.selected_claude_provider_profile_id = Some(BYOK_PROVIDER_ID.to_string());
        changed = true;
    }
    changed
}

fn infer_legacy_codex_provider_profile_id(
    paths: &AppPaths,
    settings: &AppSettings,
) -> &'static str {
    if settings.codex_connection_mode == CodexConnectionMode::Default {
        return CODEX_DEFAULT_PROVIDER_ID;
    }
    normalize_codex_provider(&codex_active_provider(&codex_config_path(paths)))
        .unwrap_or(BYOK_PROVIDER_ID)
}

pub fn settings_snapshot(paths: &AppPaths) -> AgentSettingsSnapshot {
    let settings = load_app_settings(paths);
    let agents = agent_statuses(paths, settings.selected_agent);
    AgentSettingsSnapshot {
        settings,
        agents,
        env_override: std::env::var("ACP_AGENT_COMMAND").ok(),
        codex_acp: codex_acp_settings_status(paths),
        claude: claude_provider_settings_status(paths),
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
        selected_codex_provider_profile_id: existing.selected_codex_provider_profile_id,
        selected_claude_provider_profile_id: existing.selected_claude_provider_profile_id,
        claude: existing.claude,
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

fn settings_path(paths: &AppPaths) -> PathBuf {
    paths.config_dir().join(SETTINGS_FILE)
}

fn provider_secrets_path(paths: &AppPaths) -> PathBuf {
    paths.config_dir().join(PROVIDER_SECRETS_FILE)
}

fn provider_models_path(paths: &AppPaths) -> PathBuf {
    paths.config_dir().join(PROVIDER_MODELS_FILE)
}

fn read_provider_models_catalog(paths: &AppPaths) -> Result<ProviderModelsCatalog> {
    let path = provider_models_path(paths);
    if !path.exists() {
        return Ok(ProviderModelsCatalog::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read provider model catalog {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse provider model catalog {}", path.display()))
}

fn load_provider_models_catalog(paths: &AppPaths) -> ProviderModelsCatalog {
    read_provider_models_catalog(paths).unwrap_or_default()
}

fn save_provider_models_catalog(paths: &AppPaths, catalog: &ProviderModelsCatalog) -> Result<()> {
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    let path = provider_models_path(paths);
    let content = serde_json::to_string_pretty(catalog)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write provider model catalog {}", path.display()))
}

fn normalize_model_list(models: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if model.len() > 160 {
            anyhow::bail!("model name is too long: {model}");
        }
        if !normalized.iter().any(|item: &String| item == model) {
            normalized.push(model.to_string());
        }
    }
    if normalized.is_empty() {
        anyhow::bail!("model list cannot be empty");
    }
    if normalized.len() > 200 {
        anyhow::bail!("model list cannot contain more than 200 models");
    }
    Ok(normalized)
}

fn normalize_model_source_provider(provider: &str) -> Result<&'static str> {
    let provider = normalize_codex_provider(provider)?;
    if !BYOK_SOURCE_PROVIDER_IDS.contains(&provider) {
        anyhow::bail!(
            "{} does not have an editable model list",
            provider_label(provider)
        );
    }
    Ok(provider)
}

fn provider_secret_key(family: AgentProviderFamily, profile_id: &str) -> String {
    let family = match family {
        AgentProviderFamily::Codex => "codex",
        AgentProviderFamily::Claude => "claude",
    };
    format!("{family}:{profile_id}")
}

fn provider_secret_storage_key(family: AgentProviderFamily, profile_id: &str) -> String {
    if profile_id == TIMIAI_PROVIDER_ID {
        return format!("shared:{TIMIAI_PROVIDER_ID}");
    }
    provider_secret_key(family, profile_id)
}

fn load_provider_secrets(paths: &AppPaths) -> BTreeMap<String, String> {
    std::fs::read_to_string(provider_secrets_path(paths))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_provider_secrets(paths: &AppPaths, secrets: &BTreeMap<String, String>) -> Result<()> {
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    let path = provider_secrets_path(paths);
    let content = serde_json::to_string_pretty(secrets)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write provider secrets {}", path.display()))
}

fn provider_secret(
    paths: &AppPaths,
    family: AgentProviderFamily,
    profile_id: &str,
) -> Option<String> {
    let from_store = provider_secret_from_store(paths, family, profile_id);
    if profile_id == TIMIAI_PROVIDER_ID {
        return from_store.or_else(|| {
            codex_provider_key(&codex_config_path(paths), TIMIAI_PROVIDER_ID)
                .filter(|secret| !secret.trim().is_empty())
        });
    }
    from_store
}

fn provider_secret_from_store(
    paths: &AppPaths,
    family: AgentProviderFamily,
    profile_id: &str,
) -> Option<String> {
    let mut secrets = load_provider_secrets(paths);
    secrets.remove(&provider_secret_storage_key(family, profile_id))
}

fn save_provider_secret(
    paths: &AppPaths,
    family: AgentProviderFamily,
    profile_id: &str,
    secret: &str,
) -> Result<()> {
    let secret = secret.trim();
    if secret.is_empty() {
        anyhow::bail!("api_key cannot be empty");
    }
    let mut secrets = load_provider_secrets(paths);
    secrets.insert(
        provider_secret_storage_key(family, profile_id),
        secret.to_string(),
    );
    save_provider_secrets(paths, &secrets)
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

pub fn claude_agent_acp_binary_path(paths: &AppPaths) -> PathBuf {
    codex_acp_bin_dir(paths).join(binary_name("claude-agent-acp"))
}

pub fn claude_agent_acp_package_dir(paths: &AppPaths) -> PathBuf {
    codex_acp_bin_dir(paths).join("claude-agent-acp-package")
}

pub fn claude_provider_settings_status(paths: &AppPaths) -> ClaudeProviderSettingsStatus {
    let settings = load_app_settings(paths);
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    let fast_model_options = claude_fast_model_options(paths);
    let fast_model = settings
        .claude
        .fast_model
        .as_ref()
        .filter(|model| fast_model_options.iter().any(|option| &option.id == *model))
        .cloned();
    ClaudeProviderSettingsStatus {
        selected_profile_id: selected_profile_id.clone(),
        profiles: provider_profiles(paths, AgentProviderFamily::Claude, &selected_profile_id),
        fast_model,
        fast_model_options,
    }
}

pub fn codex_acp_settings_status(paths: &AppPaths) -> CodexAcpSettingsStatus {
    let config_path = codex_config_path(paths);
    let settings = load_app_settings(paths);
    let connection_mode = settings.codex_connection_mode;
    let selected_profile_id = selected_codex_provider_profile_id(paths, &settings);
    CodexAcpSettingsStatus {
        provider: selected_profile_id.clone(),
        selected_profile_id: selected_profile_id.clone(),
        profiles: provider_profiles(paths, AgentProviderFamily::Codex, &selected_profile_id),
        connection_mode,
        deepseek_key_configured: codex_deepseek_key_configured(&config_path),
        config_path,
    }
}

pub fn codex_current_provider(paths: &AppPaths) -> String {
    let settings = load_app_settings(paths);
    selected_codex_provider_profile_id(paths, &settings)
}

fn selected_codex_provider_profile_id(paths: &AppPaths, settings: &AppSettings) -> String {
    let candidate = settings
        .selected_codex_provider_profile_id
        .as_deref()
        .unwrap_or_else(|| infer_legacy_codex_provider_profile_id(paths, settings));
    if codex_is_byok_source(candidate) {
        return BYOK_PROVIDER_ID.to_string();
    }
    if profile_definition(AgentProviderFamily::Codex, candidate).is_some() {
        candidate.to_string()
    } else {
        infer_legacy_codex_provider_profile_id(paths, settings).to_string()
    }
}

fn selected_claude_provider_profile_id(settings: &AppSettings) -> String {
    let candidate = settings
        .selected_claude_provider_profile_id
        .as_deref()
        .unwrap_or(BYOK_PROVIDER_ID);
    if claude_is_byok_source(candidate) {
        return BYOK_PROVIDER_ID.to_string();
    }
    if profile_definition(AgentProviderFamily::Claude, candidate).is_some() {
        candidate.to_string()
    } else {
        BYOK_PROVIDER_ID.to_string()
    }
}

fn provider_profiles(
    paths: &AppPaths,
    family: AgentProviderFamily,
    selected_profile_id: &str,
) -> Vec<AgentProviderProfile> {
    profile_definitions(family)
        .iter()
        .map(|definition| provider_profile(paths, definition, selected_profile_id))
        .collect()
}

fn provider_profile(
    paths: &AppPaths,
    definition: &ProviderProfileDefinition,
    selected_profile_id: &str,
) -> AgentProviderProfile {
    let models = if definition.id == BYOK_PROVIDER_ID {
        match definition.family {
            AgentProviderFamily::Codex => configured_codex_byok_models(paths),
            AgentProviderFamily::Claude => configured_claude_byok_models(paths),
        }
    } else if BYOK_SOURCE_PROVIDER_IDS.contains(&definition.id) {
        effective_catalog_models_for_provider(paths, definition.id)
    } else {
        definition
            .models
            .iter()
            .map(|model| (*model).to_string())
            .collect()
    };
    AgentProviderProfile {
        family: definition.family,
        id: definition.id.to_string(),
        label: definition.label.to_string(),
        proxy_kind: definition.proxy_kind,
        selected: definition.id == selected_profile_id,
        configured: provider_profile_configured(paths, definition),
        base_url: definition.base_url.map(str::to_string),
        default_model: definition.default_model.map(str::to_string),
        models,
        credential_label: definition.credential_label.map(str::to_string),
        requires_credential: definition.requires_credential,
        help_text: definition.help_text.to_string(),
    }
}

fn provider_profile_configured(paths: &AppPaths, definition: &ProviderProfileDefinition) -> bool {
    if definition.id == BYOK_PROVIDER_ID {
        return BYOK_SOURCE_PROVIDER_IDS
            .iter()
            .any(|provider| byok_source_secret(paths, definition.family, provider).is_some());
    }
    if !definition.requires_credential {
        return true;
    }
    if definition.id == TIMIAI_PROVIDER_ID {
        return provider_secret(paths, definition.family, definition.id)
            .map(|secret| !secret.trim().is_empty())
            .unwrap_or(false);
    }
    match definition.family {
        AgentProviderFamily::Codex => codex_provider_key(&codex_config_path(paths), definition.id)
            .map(|key| !key.trim().is_empty())
            .unwrap_or(false),
        AgentProviderFamily::Claude => provider_secret(paths, definition.family, definition.id)
            .map(|secret| !secret.trim().is_empty())
            .unwrap_or(false),
    }
}

fn profile_definitions(family: AgentProviderFamily) -> &'static [ProviderProfileDefinition] {
    match family {
        AgentProviderFamily::Codex => CODEX_PROVIDER_PROFILES,
        AgentProviderFamily::Claude => CLAUDE_PROVIDER_PROFILES,
    }
}

fn profile_definition(
    family: AgentProviderFamily,
    profile_id: &str,
) -> Option<&'static ProviderProfileDefinition> {
    profile_definitions(family)
        .iter()
        .find(|definition| definition.id == profile_id)
}

pub fn select_codex_default_mode(paths: &AppPaths) -> Result<AgentSettingsSnapshot> {
    select_agent_provider_profile(paths, AgentProviderFamily::Codex, CODEX_DEFAULT_PROVIDER_ID)
}

pub fn select_agent_provider_profile(
    paths: &AppPaths,
    family: AgentProviderFamily,
    profile_id: &str,
) -> Result<AgentSettingsSnapshot> {
    let normalized_profile_id = match family {
        AgentProviderFamily::Codex if codex_is_byok_source(profile_id) => BYOK_PROVIDER_ID,
        AgentProviderFamily::Claude if claude_is_byok_source(profile_id) => BYOK_PROVIDER_ID,
        _ => profile_id,
    };
    let definition = profile_definition(family, normalized_profile_id)
        .ok_or_else(|| anyhow::anyhow!("Unsupported provider profile: {profile_id}"))?;
    let mut settings = load_app_settings(paths);
    match family {
        AgentProviderFamily::Codex => {
            settings.selected_codex_provider_profile_id = Some(definition.id.to_string());
            settings.codex_connection_mode =
                if definition.proxy_kind == AgentProviderProxyKind::CodexDefault {
                    CodexConnectionMode::Default
                } else {
                    CodexConnectionMode::Managed
                };
            if definition.id == BYOK_PROVIDER_ID {
                write_codex_byok_channel_config(paths)?;
            }
            if definition.requires_credential {
                let api_key = codex_provider_key(&codex_config_path(paths), definition.id)
                    .or_else(|| provider_secret(paths, family, definition.id));
                let Some(api_key) = api_key else {
                    save_app_settings(paths, &settings)?;
                    return Ok(settings_snapshot(paths));
                };
                if !api_key.trim().is_empty() {
                    write_codex_acp_provider_config(paths, definition.id, &api_key)?;
                }
            }
        }
        AgentProviderFamily::Claude => {
            settings.selected_claude_provider_profile_id = Some(definition.id.to_string());
        }
    }
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn save_agent_provider_secret(
    paths: &AppPaths,
    family: AgentProviderFamily,
    profile_id: &str,
    secret: &str,
) -> Result<AgentSettingsSnapshot> {
    let definition = profile_definition(family, profile_id)
        .ok_or_else(|| anyhow::anyhow!("Unsupported provider profile: {profile_id}"))?;
    if !definition.requires_credential {
        anyhow::bail!("{} does not require a credential", definition.label);
    }
    match family {
        AgentProviderFamily::Codex => {
            if codex_is_byok_source(definition.id) {
                save_codex_byok_source_secret(paths, definition.id, secret)?;
            } else {
                save_provider_secret(paths, family, definition.id, secret)?;
                write_codex_acp_provider_config(paths, definition.id, secret)?;
                save_codex_managed_mode_with_profile(paths, definition.id)?;
            }
        }
        AgentProviderFamily::Claude => {
            save_provider_secret(paths, family, definition.id, secret)?;
        }
    }
    Ok(settings_snapshot(paths))
}

pub fn save_provider_models(
    paths: &AppPaths,
    provider: &str,
    models: Vec<String>,
) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_model_source_provider(provider)?;
    let models = normalize_model_list(models)?;
    let mut catalog = read_provider_models_catalog(paths)?;
    catalog.version = PROVIDER_MODELS_VERSION;
    catalog
        .providers
        .insert(provider.to_string(), ProviderModelsEntry { models });
    save_provider_models_catalog(paths, &catalog)?;
    refresh_codex_model_catalog_after_provider_models_change(paths)?;
    Ok(settings_snapshot(paths))
}

pub fn reset_provider_models(paths: &AppPaths, provider: &str) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_model_source_provider(provider)?;
    let mut catalog = read_provider_models_catalog(paths)?;
    catalog.providers.remove(provider);
    catalog.version = PROVIDER_MODELS_VERSION;
    save_provider_models_catalog(paths, &catalog)?;
    refresh_codex_model_catalog_after_provider_models_change(paths)?;
    Ok(settings_snapshot(paths))
}

pub fn select_claude_fast_model(
    paths: &AppPaths,
    model_id: Option<String>,
) -> Result<AgentSettingsSnapshot> {
    let model_id = model_id.and_then(|model| {
        let trimmed = model.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    if let Some(model_id) = &model_id {
        let options = claude_fast_model_options(paths);
        if !options.iter().any(|option| &option.id == model_id) {
            anyhow::bail!("Unsupported Claude fast model: {model_id}");
        }
    }

    let mut settings = load_app_settings(paths);
    settings.claude.fast_model = model_id;
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn save_codex_acp_provider_key(
    paths: &AppPaths,
    provider: &str,
    api_key: &str,
) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_codex_provider(provider)?;
    if codex_is_byok_source(provider) {
        save_codex_byok_source_secret(paths, provider, api_key)?;
        save_codex_managed_mode_with_profile(paths, BYOK_PROVIDER_ID)?;
        return Ok(settings_snapshot(paths));
    }
    write_codex_acp_provider_config(paths, provider, api_key)?;
    save_codex_managed_mode_with_profile(paths, provider)?;
    Ok(settings_snapshot(paths))
}

pub fn select_codex_acp_provider(
    paths: &AppPaths,
    provider: &str,
) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_codex_provider(provider)?;
    let config_path = codex_config_path(paths);
    let Some(api_key) = codex_provider_key(&config_path, provider)
        .or_else(|| provider_secret(paths, AgentProviderFamily::Codex, provider))
    else {
        anyhow::bail!("请先填写并保存 {} API key", provider_label(provider));
    };
    if api_key.trim().is_empty() {
        anyhow::bail!("请先填写并保存 {} API key", provider_label(provider));
    }

    if codex_is_byok_source(provider) {
        save_codex_byok_source_secret(paths, provider, &api_key)?;
        save_codex_managed_mode_with_profile(paths, BYOK_PROVIDER_ID)?;
        return Ok(settings_snapshot(paths));
    }

    write_codex_acp_provider_config(paths, provider, &api_key)?;
    save_codex_managed_mode_with_profile(
        paths,
        if codex_is_byok_source(provider) {
            BYOK_PROVIDER_ID
        } else {
            provider
        },
    )?;
    Ok(settings_snapshot(paths))
}

fn save_codex_managed_mode_with_profile(paths: &AppPaths, profile_id: &str) -> Result<()> {
    let mut settings = load_app_settings(paths);
    settings.codex_connection_mode = CodexConnectionMode::Managed;
    settings.selected_codex_provider_profile_id = Some(
        if codex_is_byok_source(profile_id) {
            BYOK_PROVIDER_ID
        } else {
            profile_id
        }
        .to_string(),
    );
    save_app_settings(paths, &settings)?;
    Ok(())
}

fn write_codex_byok_channel_config(paths: &AppPaths) -> Result<()> {
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

    let (default_model, default_model_provider) =
        catalog_models_for_provider_with_paths(paths, BYOK_PROVIDER_ID)
            .into_iter()
            .next()
            .unwrap_or_else(|| (TIMIAI_CODEX_MODEL.to_string(), TIMIAI_PROVIDER_ID));
    let default_provider_key =
        byok_source_secret(paths, AgentProviderFamily::Codex, default_model_provider);
    let runtime_provider = BYOK_PROVIDER_ID;
    doc["model"] = value(byok_encoded_model_slug(
        &default_model,
        default_model_provider,
    ));
    doc["model_provider"] = value(runtime_provider);
    doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
    doc["model_context_window"] = value(model_context_window_for_provider(
        &default_model,
        default_model_provider,
    ));
    doc["model_max_output_tokens"] = value(model_max_output_tokens_for_provider(
        &default_model,
        default_model_provider,
    ));
    doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
    doc["model_catalog_json"] = value(
        codex_model_catalog_path(paths)
            .to_string_lossy()
            .to_string(),
    );
    write_codex_byok_provider_table(&mut doc);
    if let Some(key) = default_provider_key {
        write_codex_provider_table(&mut doc, default_model_provider, &key);
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    write_codex_acp_model_catalog(paths, BYOK_PROVIDER_ID)?;
    sync_codex_api_proxy_model_provider_map_for_paths(paths);
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

    let default_model = default_model_for_provider_with_paths(paths, provider);
    doc["model"] = value(default_model.as_str());
    let active_provider = codex_channel_provider_for_source(provider);
    doc["model_provider"] = value(active_provider);
    doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
    doc["model_context_window"] =
        value(model_context_window_for_provider(&default_model, provider));
    doc["model_max_output_tokens"] = value(model_max_output_tokens_for_provider(
        &default_model,
        provider,
    ));
    doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
    doc["model_catalog_json"] = value(
        codex_model_catalog_path(paths)
            .to_string_lossy()
            .to_string(),
    );
    write_codex_provider_table(&mut doc, provider, key);
    if active_provider == BYOK_PROVIDER_ID {
        write_codex_byok_provider_table(&mut doc);
    }

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    write_codex_acp_model_catalog(paths, catalog_provider_for_active_provider(active_provider))?;
    sync_codex_api_proxy_model_provider_map_for_paths(paths);
    Ok(())
}

fn save_codex_byok_source_secret(paths: &AppPaths, provider: &str, api_key: &str) -> Result<()> {
    let provider = normalize_codex_provider(provider)?;
    if provider == BYOK_PROVIDER_ID {
        anyhow::bail!("{} is not a BYOK model source", provider_label(provider));
    }
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

    let active_provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .and_then(|provider| normalize_codex_provider(provider).ok())
        .unwrap_or(BYOK_PROVIDER_ID);

    write_codex_provider_table(&mut doc, provider, key);
    write_codex_byok_provider_table(&mut doc);

    if active_provider == BYOK_PROVIDER_ID || codex_is_byok_source(active_provider) {
        let source_provider_hint = codex_is_byok_source(active_provider).then_some(active_provider);
        let runtime_provider = BYOK_PROVIDER_ID;
        doc["model_provider"] = value(runtime_provider);
        let active_model = doc
            .get("model")
            .and_then(|item| item.as_str())
            .unwrap_or_else(|| default_model_for_provider(provider))
            .to_string();
        let active_model_slug =
            repair_byok_model_slug_with_paths(paths, &active_model, source_provider_hint);
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug.clone());
        }
        let active_source_provider =
            byok_source_provider_for_model_with_hint(&active_model_slug, source_provider_hint);
        let active_upstream_model =
            byok_upstream_model_for_model_with_hint(&active_model_slug, source_provider_hint);
        if doc.get("preferred_auth_method").is_none() {
            doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
        }
        doc["model_context_window"] = value(model_context_window_for_provider(
            &active_upstream_model,
            active_source_provider,
        ));
        doc["model_max_output_tokens"] = value(model_max_output_tokens_for_provider(
            &active_upstream_model,
            active_source_provider,
        ));
        if doc.get("model_reasoning_effort").is_none() {
            doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
        }
        if doc.get("model_catalog_json").is_none() {
            doc["model_catalog_json"] = value(
                codex_model_catalog_path(paths)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    if active_provider == BYOK_PROVIDER_ID || codex_is_byok_source(active_provider) {
        write_codex_acp_model_catalog(paths, BYOK_PROVIDER_ID)?;
        sync_codex_api_proxy_model_provider_map_for_paths(paths);
    }
    Ok(())
}

fn codex_channel_provider_for_source(provider: &str) -> &'static str {
    match provider {
        CODEX_DEFAULT_PROVIDER_ID => CODEX_DEFAULT_PROVIDER_ID,
        BYOK_PROVIDER_ID => BYOK_PROVIDER_ID,
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_ID,
        COMMANDCODE_PROVIDER_ID => COMMANDCODE_PROVIDER_ID,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_PROVIDER_ID,
        KIMI_PROVIDER_ID => KIMI_PROVIDER_ID,
        MIMO_PROVIDER_ID => MIMO_PROVIDER_ID,
        _ => BYOK_PROVIDER_ID,
    }
}

fn catalog_provider_for_active_provider(provider: &str) -> &str {
    if provider == BYOK_PROVIDER_ID {
        BYOK_PROVIDER_ID
    } else {
        provider
    }
}

fn codex_selected_catalog_provider(paths: &AppPaths) -> String {
    let settings = load_app_settings(paths);
    selected_codex_provider_profile_id(paths, &settings)
}

fn codex_is_byok_source(provider: &str) -> bool {
    BYOK_SOURCE_PROVIDER_IDS.contains(&provider)
}

fn claude_is_byok_source(provider: &str) -> bool {
    BYOK_SOURCE_PROVIDER_IDS.contains(&provider)
}

fn custom_catalog_models_for_provider(paths: &AppPaths, provider: &str) -> Option<Vec<String>> {
    let provider = normalize_codex_provider(provider).ok()?;
    let catalog = load_provider_models_catalog(paths);
    let models = catalog.providers.get(provider)?.models.clone();
    (!models.is_empty()).then_some(models)
}

fn effective_catalog_models_for_provider(paths: &AppPaths, provider: &str) -> Vec<String> {
    if let Some(models) = custom_catalog_models_for_provider(paths, provider) {
        return models;
    }
    default_catalog_models_for_provider(provider)
        .iter()
        .map(|model| (*model).to_string())
        .collect()
}

fn configured_codex_byok_models(paths: &AppPaths) -> Vec<String> {
    let mut models = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if byok_source_secret(paths, AgentProviderFamily::Codex, provider).is_none() {
            continue;
        }
        for model in effective_catalog_models_for_provider(paths, provider) {
            let display_name = byok_display_model_name(&model, provider);
            if !models.contains(&display_name) {
                models.push(display_name);
            }
        }
    }
    models
}

fn byok_source_secret(
    paths: &AppPaths,
    family: AgentProviderFamily,
    provider: &str,
) -> Option<String> {
    provider_secret(paths, family, provider)
        .filter(|secret| !secret.trim().is_empty())
        .or_else(|| {
            codex_provider_key(&codex_config_path(paths), provider)
                .filter(|secret| !secret.trim().is_empty())
        })
}

fn codex_deepseek_key_configured(path: &Path) -> bool {
    codex_provider_key(path, DEEPSEEK_PROVIDER_ID)
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false)
}

fn codex_active_provider(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return BYOK_PROVIDER_ID.to_string();
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return BYOK_PROVIDER_ID.to_string();
    };
    doc.get("model_provider")
        .and_then(|item| item.as_str())
        .and_then(|provider| normalize_codex_provider(provider).ok())
        .unwrap_or(BYOK_PROVIDER_ID)
        .to_string()
}

fn codex_provider_keys(paths: &AppPaths) -> Vec<(String, String)> {
    let path = codex_config_path(paths);
    let mut providers = Vec::new();
    if codex_active_provider(&path) == BYOK_PROVIDER_ID {
        providers.push(BYOK_PROVIDER_ID);
    }
    providers.extend(BYOK_SOURCE_PROVIDER_IDS.iter().copied());
    providers
        .into_iter()
        .filter_map(|provider| {
            byok_source_secret(paths, AgentProviderFamily::Codex, provider)
                .filter(|key| !key.trim().is_empty())
                .map(|key| (env_key_for_provider(provider).to_string(), key))
        })
        .collect()
}

fn codex_model_provider_map_env(paths: &AppPaths) -> Option<(String, String)> {
    let catalog_provider = codex_selected_catalog_provider(paths);
    if catalog_provider == CODEX_DEFAULT_PROVIDER_ID {
        return None;
    }
    let entries = catalog_models_for_provider_with_paths(paths, &catalog_provider)
        .into_iter()
        .collect::<Vec<_>>();
    model_provider_map_env_from_entries(entries, catalog_provider == BYOK_PROVIDER_ID)
}

fn claude_model_provider_map_env(paths: &AppPaths) -> Option<(String, String)> {
    let mut entries = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if byok_source_secret(paths, AgentProviderFamily::Claude, provider).is_none() {
            continue;
        }
        entries.extend(
            effective_catalog_models_for_provider(paths, provider)
                .into_iter()
                .map(|model| (model, *provider)),
        );
    }
    model_provider_map_env_from_entries(entries, true)
}

fn model_provider_map_env_from_entries(
    entries: Vec<(String, &'static str)>,
    encode_provider_models: bool,
) -> Option<(String, String)> {
    if entries.is_empty() {
        return None;
    }
    let entries = entries
        .into_iter()
        .map(|(model, provider)| {
            let model_id = if encode_provider_models {
                byok_encoded_model_slug(&model, provider)
            } else {
                model_slug_for_provider(&model, provider).to_string()
            };
            let display_name = if encode_provider_models {
                byok_display_model_name(&model, provider)
            } else {
                model.clone()
            };
            json!({
                "model": model_id,
                "display_name": display_name,
                "provider": provider,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&entries)
        .ok()
        .map(|value| (KODEX_MODEL_PROVIDER_MAP_ENV.to_string(), value))
}

fn sync_codex_api_proxy_model_provider_map(provider_map: Option<&str>) {
    if let Some(value) = provider_map {
        acp_core::configure_codex_api_proxy_model_provider_map(value);
    } else {
        acp_core::clear_codex_api_proxy_model_provider_map();
    }
}

fn sync_codex_api_proxy_model_provider_map_for_paths(paths: &AppPaths) {
    let provider_map = codex_model_provider_map_env(paths);
    sync_codex_api_proxy_model_provider_map(provider_map.as_ref().map(|(_, value)| value.as_str()));
}

fn codex_active_provider_key(path: &Path) -> Option<(String, String)> {
    let provider = codex_active_provider(path);
    if provider == BYOK_PROVIDER_ID {
        return None;
    }
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

    let raw_provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let normalized_provider = raw_provider
        .as_deref()
        .and_then(|provider| normalize_codex_provider(provider).ok());
    let provider = normalized_provider.unwrap_or(BYOK_PROVIDER_ID);
    let repaired_provider = normalized_provider.is_none();
    if repaired_provider {
        doc["model_provider"] = value(provider);
    }
    if provider == BYOK_PROVIDER_ID || codex_is_byok_source(provider) {
        let mut changed = repaired_provider;
        let source_provider_hint = codex_is_byok_source(provider).then_some(provider);
        let runtime_provider = BYOK_PROVIDER_ID;
        if doc.get("model_provider").and_then(|item| item.as_str()) != Some(runtime_provider) {
            doc["model_provider"] = value(runtime_provider);
            changed = true;
        }
        write_codex_byok_provider_table(&mut doc);
        let active_model = doc
            .get("model")
            .and_then(|item| item.as_str())
            .unwrap_or_else(|| default_model_for_provider(provider))
            .to_string();
        let paths_for_repair = path.parent().map(AppPaths::from_root);
        let active_model_slug = paths_for_repair
            .as_ref()
            .map(|paths| {
                repair_byok_model_slug_with_paths(paths, &active_model, source_provider_hint)
            })
            .unwrap_or_else(|| byok_model_slug_with_hint(&active_model, source_provider_hint));
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug.clone());
            changed = true;
        }
        let active_model_provider =
            byok_source_provider_for_model_with_hint(&active_model_slug, source_provider_hint);
        let base_url = base_url_for_provider(runtime_provider);
        if !provider_field_eq(&doc, runtime_provider, "base_url", &base_url) {
            doc["model_providers"][runtime_provider]["base_url"] = value(base_url);
            changed = true;
        }
        if doc.get("preferred_auth_method").is_none() {
            doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
            changed = true;
        }
        let active_upstream_model =
            byok_upstream_model_for_model_with_hint(&active_model_slug, source_provider_hint);
        let expected_context_window =
            model_context_window_for_provider(&active_upstream_model, active_model_provider);
        if doc
            .get("model_context_window")
            .and_then(|item| item.as_integer())
            != Some(expected_context_window)
        {
            doc["model_context_window"] = value(expected_context_window);
            changed = true;
        }
        let expected_max_output_tokens =
            model_max_output_tokens_for_provider(&active_upstream_model, active_model_provider);
        if doc
            .get("model_max_output_tokens")
            .and_then(|item| item.as_integer())
            != Some(expected_max_output_tokens)
        {
            doc["model_max_output_tokens"] = value(expected_max_output_tokens);
            changed = true;
        }
        if doc.get("model_reasoning_effort").is_none() {
            doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
            changed = true;
        }
        if let Some(parent) = path.parent() {
            let paths = AppPaths::from_root(parent);
            let _ = write_codex_acp_model_catalog(
                &paths,
                catalog_provider_for_active_provider(provider),
            );
        }
        if changed {
            std::fs::write(path, doc.to_string())
                .with_context(|| format!("failed to write Codex config {}", path.display()))?;
        }
        return Ok(());
    }
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
    let expected_context_window = model_context_window_for_provider(&active_model, provider);
    if doc
        .get("model_context_window")
        .and_then(|item| item.as_integer())
        != Some(expected_context_window)
    {
        doc["model_context_window"] = value(expected_context_window);
        changed = true;
    }
    let expected_max_output_tokens = model_max_output_tokens_for_provider(&active_model, provider);
    if doc
        .get("model_max_output_tokens")
        .and_then(|item| item.as_integer())
        != Some(expected_max_output_tokens)
    {
        doc["model_max_output_tokens"] = value(expected_max_output_tokens);
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

fn write_codex_byok_provider_table(doc: &mut DocumentMut) {
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
    providers.insert(BYOK_PROVIDER_ID, Item::Table(Table::new()));
    let provider_table = providers
        .get_mut(BYOK_PROVIDER_ID)
        .and_then(|item| item.as_table_mut())
        .expect("byok provider should be a table");
    provider_table.insert("name", value(BYOK_PROVIDER_NAME));
    provider_table.insert("base_url", value(base_url_for_provider(BYOK_PROVIDER_ID)));
    provider_table.insert("wire_api", value(wire_api_for_provider(BYOK_PROVIDER_ID)));
    provider_table.insert("env_key", value(BYOK_API_KEY_ENV));
    provider_table.insert("api_key", value("byok"));
}

fn codex_proxy_base_url() -> String {
    acp_core::codex_api_proxy_base_url()
}

fn codex_proxy_provider_base_url(provider: &str) -> String {
    format!("{}/providers/{provider}", codex_proxy_base_url())
}

fn normalize_codex_provider(provider: &str) -> Result<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        BYOK_PROVIDER_ID => Ok(BYOK_PROVIDER_ID),
        TIMIAI_PROVIDER_ID | "timi" | "timi-ai" | "timi_ai" => Ok(TIMIAI_PROVIDER_ID),
        COMMANDCODE_PROVIDER_ID | "command-code" | "command_code" => Ok(COMMANDCODE_PROVIDER_ID),
        DEEPSEEK_PROVIDER_ID => Ok(DEEPSEEK_PROVIDER_ID),
        KIMI_PROVIDER_ID | "kimi" | "kimi-code" => Ok(KIMI_PROVIDER_ID),
        MIMO_PROVIDER_ID | "mimo" | "xiaomi-mimo" => Ok(MIMO_PROVIDER_ID),
        other => anyhow::bail!("Unsupported Codex provider: {other}"),
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => model_slug_for_provider(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID),
        TIMIAI_PROVIDER_ID => model_slug_for_provider(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID),
        COMMANDCODE_PROVIDER_ID => {
            model_slug_for_provider(COMMANDCODE_MODEL, COMMANDCODE_PROVIDER_ID)
        }
        DEEPSEEK_PROVIDER_ID => model_slug_for_provider(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID),
        KIMI_PROVIDER_ID => model_slug_for_provider(KIMI_MODEL, KIMI_PROVIDER_ID),
        MIMO_PROVIDER_ID => model_slug_for_provider(MIMO_MODEL, MIMO_PROVIDER_ID),
        _ => model_slug_for_provider(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID),
    }
}

fn default_model_for_provider_with_paths(paths: &AppPaths, provider: &str) -> String {
    effective_catalog_models_for_provider(paths, provider)
        .into_iter()
        .next()
        .unwrap_or_else(|| default_model_for_provider(provider).to_string())
}

fn provider_name(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => BYOK_PROVIDER_NAME,
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_NAME,
        COMMANDCODE_PROVIDER_ID => COMMANDCODE_PROVIDER_NAME,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_PROVIDER_NAME,
        KIMI_PROVIDER_ID => KIMI_PROVIDER_NAME,
        MIMO_PROVIDER_ID => MIMO_PROVIDER_NAME,
        _ => TIMIAI_PROVIDER_NAME,
    }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => "BYOK",
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_NAME,
        COMMANDCODE_PROVIDER_ID => COMMANDCODE_PROVIDER_NAME,
        DEEPSEEK_PROVIDER_ID => "DeepSeek",
        KIMI_PROVIDER_ID => "Kimi Code",
        MIMO_PROVIDER_ID => MIMO_PROVIDER_NAME,
        _ => TIMIAI_PROVIDER_NAME,
    }
}

fn wire_api_for_provider(_provider: &str) -> &'static str {
    CODEX_PROXY_WIRE_API
}

fn env_key_for_provider(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => BYOK_API_KEY_ENV,
        TIMIAI_PROVIDER_ID => TIMIAI_API_KEY_ENV,
        COMMANDCODE_PROVIDER_ID => COMMANDCODE_API_KEY_ENV,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_API_KEY_ENV,
        KIMI_PROVIDER_ID => KIMI_API_KEY_ENV,
        MIMO_PROVIDER_ID => MIMO_API_KEY_ENV,
        _ => TIMIAI_API_KEY_ENV,
    }
}

fn base_url_for_provider(provider: &str) -> String {
    match provider {
        BYOK_PROVIDER_ID => codex_proxy_base_url(),
        TIMIAI_PROVIDER_ID => codex_proxy_provider_base_url(TIMIAI_PROVIDER_ID),
        COMMANDCODE_PROVIDER_ID => codex_proxy_provider_base_url(COMMANDCODE_PROVIDER_ID),
        DEEPSEEK_PROVIDER_ID => codex_proxy_provider_base_url(DEEPSEEK_PROVIDER_ID),
        KIMI_PROVIDER_ID => codex_proxy_provider_base_url(KIMI_PROVIDER_ID),
        MIMO_PROVIDER_ID => codex_proxy_provider_base_url(MIMO_PROVIDER_ID),
        _ => codex_proxy_base_url(),
    }
}

fn codex_model_catalog_path(paths: &AppPaths) -> PathBuf {
    paths.root().join("model_catalog.json")
}

fn refresh_codex_model_catalog_after_provider_models_change(paths: &AppPaths) -> Result<()> {
    let config_path = codex_config_path(paths);
    if !config_path.exists() {
        return Ok(());
    }
    let provider = codex_selected_catalog_provider(paths);
    if provider == CODEX_DEFAULT_PROVIDER_ID {
        return Ok(());
    }
    write_codex_acp_model_catalog(paths, &provider)
}

pub fn remote_codex_model_catalog_content(paths: &AppPaths) -> Result<Option<String>> {
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        return Ok(None);
    }
    let provider = codex_selected_catalog_provider(paths);
    if provider == CODEX_DEFAULT_PROVIDER_ID {
        return Ok(None);
    }
    codex_acp_model_catalog_content(paths, &provider).map(Some)
}

fn write_codex_acp_model_catalog(paths: &AppPaths, provider: &str) -> Result<()> {
    let path = codex_model_catalog_path(paths);
    let content = codex_acp_model_catalog_content(paths, provider)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write Codex model catalog {}", path.display()))
}

fn codex_acp_model_catalog_content(paths: &AppPaths, provider: &str) -> Result<String> {
    let catalog_models = catalog_models_for_provider_with_paths(paths, provider);
    let encode_provider_models = provider == BYOK_PROVIDER_ID;
    let models = catalog_models
        .iter()
        .enumerate()
        .map(|(priority, (model, source_provider))| {
            codex_acp_model_catalog_entry(model, source_provider, priority, encode_provider_models)
        })
        .collect::<Vec<_>>();
    let catalog = json!({
        "models": models
    });
    serde_json::to_string_pretty(&catalog).map_err(Into::into)
}

fn catalog_models_for_provider_with_paths(
    paths: &AppPaths,
    provider: &str,
) -> Vec<(String, &'static str)> {
    let provider = normalize_codex_provider(provider).unwrap_or(TIMIAI_PROVIDER_ID);
    if provider != BYOK_PROVIDER_ID {
        return effective_catalog_models_for_provider(paths, provider)
            .into_iter()
            .map(|model| (model, provider))
            .collect();
    }
    let mut models = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if byok_source_secret(paths, AgentProviderFamily::Codex, provider).is_none() {
            continue;
        }
        models.extend(
            effective_catalog_models_for_provider(paths, provider)
                .into_iter()
                .map(|model| (model, *provider)),
        );
    }
    if models.is_empty() {
        models.push((TIMIAI_CODEX_MODEL.to_string(), TIMIAI_PROVIDER_ID));
    }
    models
}

fn default_catalog_models_for_provider(provider: &str) -> &'static [&'static str] {
    match provider {
        TIMIAI_PROVIDER_ID => TIMIAI_CATALOG_MODELS,
        COMMANDCODE_PROVIDER_ID => COMMANDCODE_CATALOG_MODELS,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_CATALOG_MODELS,
        KIMI_PROVIDER_ID => KIMI_CATALOG_MODELS,
        MIMO_PROVIDER_ID => MIMO_CATALOG_MODELS,
        _ => TIMIAI_CATALOG_MODELS,
    }
}

fn codex_acp_model_catalog_entry(
    model: &str,
    provider: &str,
    priority: usize,
    encode_provider_models: bool,
) -> serde_json::Value {
    let slug = if encode_provider_models {
        byok_encoded_model_slug(model, provider)
    } else {
        model_slug_for_provider(model, provider).to_string()
    };
    let display_name = if encode_provider_models {
        byok_display_model_name(model, provider)
    } else {
        model.to_string()
    };
    let context_window = model_context_window_for_provider(model, provider);
    let max_output_tokens = model_max_output_tokens_for_provider(model, provider);
    let is_deepseek = provider == DEEPSEEK_PROVIDER_ID || model.contains("deepseek");
    let apply_patch_tool_type = apply_patch_tool_type_for_provider(provider);
    let description = format!("Codex {display_name}");
    json!({
        "slug": slug,
        "display_name": display_name,
        "description": description,
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
        "apply_patch_tool_type": apply_patch_tool_type,
        "web_search_tool_type": "text_and_image",
        "truncation_policy": {
            "mode": "tokens",
            "limit": 10000
        },
        "supports_parallel_tool_calls": !is_deepseek,
        "supports_image_detail_original": !is_deepseek,
        "provider": provider,
        "_meta": {
            "provider": provider
        },
        "context_window": context_window,
        "max_context_window": context_window,
        "max_output_tokens": max_output_tokens,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": ["request_user_input"],
        "input_modalities": if is_deepseek { json!(["text"]) } else { json!(["text", "image"]) },
        "supports_search_tool": !is_deepseek
    })
}

fn apply_patch_tool_type_for_provider(_provider: &str) -> &'static str {
    "freeform"
}

/// Resolve a display model name with the default BYOK slug mapping.
#[cfg(test)]
fn model_slug(display_name: &str) -> &str {
    model_slug_for_provider(display_name, TIMIAI_PROVIDER_ID)
}

fn model_slug_for_provider<'a>(display_name: &'a str, provider: &str) -> &'a str {
    match provider {
        TIMIAI_PROVIDER_ID => lookup_model_slug(display_name, TIMIAI_MODEL_SLUG_MAP),
        MIMO_PROVIDER_ID => lookup_model_slug(display_name, MIMO_MODEL_SLUG_MAP),
        _ => display_name,
    }
}

fn byok_encoded_model_slug(model: &str, provider: &str) -> String {
    format!(
        "{PROVIDER_MODEL_ID_PREFIX}{}/{}",
        normalize_codex_provider(provider).unwrap_or(TIMIAI_PROVIDER_ID),
        model_slug_for_provider(model, provider)
    )
}

fn byok_display_model_name(model: &str, provider: &str) -> String {
    let _ = provider;
    model.to_string()
}

fn decode_provider_model_id(model: &str) -> Option<(&'static str, &str)> {
    let rest = model.trim().strip_prefix(PROVIDER_MODEL_ID_PREFIX)?;
    let (provider, upstream_model) = rest.split_once('/')?;
    let provider = normalize_codex_provider(provider).ok()?;
    if upstream_model.trim().is_empty() {
        return None;
    }
    Some((provider, upstream_model))
}

fn lookup_model_slug<'a>(
    display_name: &'a str,
    map: &'static [(&'static str, &'static str)],
) -> &'a str {
    map.iter()
        .find_map(|(name, slug)| (*name == display_name).then_some(*slug))
        .unwrap_or(display_name)
}

fn byok_model_slug(model: &str) -> String {
    byok_model_slug_with_hint(model, None)
}

fn byok_model_slug_with_hint(model: &str, provider_hint: Option<&'static str>) -> String {
    if decode_provider_model_id(model).is_some() {
        return model.to_string();
    }
    if let Some(display_name) = legacy_deepseek_external_slug_display_name(model) {
        return display_name.to_string();
    }
    let provider = byok_source_provider_for_model_with_hint(model, provider_hint);
    byok_encoded_model_slug(model, provider)
}

fn repair_byok_model_slug_with_paths(
    paths: &AppPaths,
    model: &str,
    provider_hint: Option<&'static str>,
) -> String {
    let slug = byok_model_slug_with_hint(model, provider_hint);
    let Some((provider, upstream_model)) = decode_provider_model_id(&slug) else {
        return slug;
    };
    if provider_catalog_contains_model(paths, provider, upstream_model) {
        return slug;
    }

    let inferred_provider = byok_source_provider_for_model_with_hint(upstream_model, None);
    if inferred_provider != provider
        && provider_catalog_contains_model(paths, inferred_provider, upstream_model)
    {
        return byok_encoded_model_slug(upstream_model, inferred_provider);
    }

    if let Some(unique_provider) = unique_catalog_provider_for_model(paths, upstream_model) {
        return byok_encoded_model_slug(upstream_model, unique_provider);
    }

    slug
}

fn unique_catalog_provider_for_model(paths: &AppPaths, model: &str) -> Option<&'static str> {
    let mut providers = BYOK_SOURCE_PROVIDER_IDS
        .iter()
        .copied()
        .filter(|provider| provider_catalog_contains_model(paths, provider, model))
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
    (providers.len() == 1).then(|| providers[0])
}

fn provider_catalog_contains_model(paths: &AppPaths, provider: &str, model: &str) -> bool {
    effective_catalog_models_for_provider(paths, provider)
        .iter()
        .any(|candidate| {
            candidate == model || model_slug_for_provider(candidate, provider) == model
        })
}

fn byok_upstream_model_for_model_with_hint(
    model: &str,
    provider_hint: Option<&'static str>,
) -> String {
    if let Some((_, upstream_model)) = decode_provider_model_id(model) {
        return upstream_model.to_string();
    }
    if let Some(display_name) = legacy_deepseek_external_slug_display_name(model) {
        return display_name.to_string();
    }
    let provider = byok_source_provider_for_model_with_hint(model, provider_hint);
    model_slug_for_provider(model, provider).to_string()
}

#[cfg(test)]
fn byok_source_provider_for_model(model: &str) -> &'static str {
    byok_source_provider_for_model_with_hint(model, None)
}

fn byok_source_provider_for_model_with_hint(
    model: &str,
    provider_hint: Option<&'static str>,
) -> &'static str {
    if let Some((provider, _)) = decode_provider_model_id(model) {
        return provider;
    }
    if let Some(provider) = provider_hint {
        return provider;
    }
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.starts_with("qwen/")
        || normalized.starts_with("minimaxai/")
        || normalized.starts_with("moonshotai/")
        || normalized.starts_with("zai-org/")
        || normalized.starts_with("stepfun/")
        || normalized.starts_with("google/")
    {
        COMMANDCODE_PROVIDER_ID
    } else if normalized.contains("deepseek") {
        DEEPSEEK_PROVIDER_ID
    } else if normalized.contains("kimi") {
        KIMI_PROVIDER_ID
    } else if normalized.contains("mimo") {
        MIMO_PROVIDER_ID
    } else {
        TIMIAI_PROVIDER_ID
    }
}

fn legacy_deepseek_external_slug_display_name(model: &str) -> Option<&'static str> {
    match model {
        "deepseek-v4-pro-external" => Some("deepseek-v4-pro"),
        "deepseek-v4-flash-external" => Some("deepseek-v4-flash"),
        _ => None,
    }
}

fn model_context_window(model: &str) -> i64 {
    model_i64_metadata(model, MODEL_CONTEXT_WINDOWS, DEFAULT_MODEL_CONTEXT_WINDOW)
}

fn model_context_window_for_provider(model: &str, provider: &str) -> i64 {
    if provider == COMMANDCODE_PROVIDER_ID {
        return model_i64_metadata(
            model,
            COMMANDCODE_MODEL_CONTEXT_WINDOWS,
            DEFAULT_MODEL_CONTEXT_WINDOW,
        );
    }
    model_context_window(model)
}

fn model_max_output_tokens(model: &str) -> i64 {
    model_i64_metadata(
        model,
        MODEL_MAX_OUTPUT_TOKENS,
        DEFAULT_MODEL_MAX_OUTPUT_TOKENS,
    )
}

fn model_max_output_tokens_for_provider(model: &str, provider: &str) -> i64 {
    if provider == COMMANDCODE_PROVIDER_ID {
        return model_i64_metadata(
            model,
            COMMANDCODE_MODEL_MAX_OUTPUT_TOKENS,
            model_max_output_tokens(model),
        );
    }
    model_max_output_tokens(model)
}

fn model_i64_metadata(model: &str, metadata: &[(&str, i64)], fallback: i64) -> i64 {
    metadata
        .iter()
        .find_map(|(candidate, value)| {
            (*candidate == model
                || model_slug_for_provider(candidate, TIMIAI_PROVIDER_ID) == model
                || model_slug_for_provider(candidate, MIMO_PROVIDER_ID) == model)
                .then_some(*value)
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

fn configured_claude_byok_models(paths: &AppPaths) -> Vec<String> {
    configured_claude_byok_model_entries(paths)
        .into_iter()
        .map(|(model, _)| model)
        .collect()
}

fn configured_claude_byok_model_entries(paths: &AppPaths) -> Vec<(String, &'static str)> {
    let mut models = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if byok_source_secret(paths, AgentProviderFamily::Claude, provider).is_none() {
            continue;
        }
        for model in effective_catalog_models_for_provider(paths, provider) {
            let display_name = byok_display_model_name(&model, provider);
            if !models.iter().any(|(existing, _)| existing == &display_name) {
                models.push((display_name, *provider));
            }
        }
    }
    models
}

fn claude_fast_model_options(paths: &AppPaths) -> Vec<AgentModelOption> {
    configured_claude_byok_model_entries(paths)
        .into_iter()
        .map(|(model, provider)| AgentModelOption {
            id: byok_encoded_model_slug(&model, provider),
            label: model,
            provider_id: provider.to_string(),
            provider_label: provider_label(provider).to_string(),
        })
        .collect()
}

fn selected_claude_fast_model_slug(
    settings: &AppSettings,
    model_entries: &[(String, &'static str)],
) -> Option<String> {
    if let Some(model_id) = settings
        .claude
        .fast_model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
    {
        if let Some((model, provider)) = model_entries.iter().find(|(model, provider)| {
            byok_encoded_model_slug(model, provider) == model_id || model == model_id
        }) {
            return Some(byok_encoded_model_slug(model, provider));
        }
    }

    default_claude_fast_model_entry(model_entries)
        .map(|(model, provider)| byok_encoded_model_slug(model, provider))
}

fn default_claude_fast_model_entry<'a>(
    model_entries: &'a [(String, &'static str)],
) -> Option<&'a (String, &'static str)> {
    model_entries
        .iter()
        .find(|(model, _)| model.to_ascii_lowercase().contains("haiku"))
        .or_else(|| {
            model_entries
                .iter()
                .find(|(model, _)| model.to_ascii_lowercase().contains("sonnet"))
        })
        .or_else(|| {
            model_entries
                .iter()
                .find(|(model, _)| model.to_ascii_lowercase().contains("claude"))
        })
        .or_else(|| model_entries.first())
}

fn configured_claude_byok_source_keys(paths: &AppPaths) -> Vec<(&'static str, String)> {
    BYOK_SOURCE_PROVIDER_IDS
        .iter()
        .filter_map(|provider| {
            byok_source_secret(paths, AgentProviderFamily::Claude, provider)
                .map(|secret| (*provider, secret))
        })
        .collect()
}

fn default_claude_byok_model(available_models: &[String]) -> Option<&str> {
    available_models
        .iter()
        .find(|model| model.to_ascii_lowercase().contains("claude"))
        .or_else(|| available_models.first())
        .map(String::as_str)
}

fn claude_model_config(available_models: &[String]) -> serde_json::Value {
    claude_model_config_for_provider(available_models, BYOK_PROVIDER_ID)
}

fn claude_model_config_for_byok_entries(
    model_entries: &[(String, &'static str)],
    fast_model_slug: Option<&str>,
) -> serde_json::Value {
    let available_models = model_entries
        .iter()
        .map(|(model, _)| model.clone())
        .collect::<Vec<_>>();
    let model_overrides = model_entries
        .iter()
        .filter_map(|(model, provider)| {
            let slug = byok_encoded_model_slug(model, provider);
            (slug != model.as_str()).then(|| (model.clone(), serde_json::Value::String(slug)))
        })
        .collect::<serde_json::Map<_, _>>();
    let mut model_overrides = model_overrides;
    if let Some(fast_model_slug) = fast_model_slug {
        for alias in CLAUDE_FAST_MODEL_ALIASES {
            model_overrides.insert(
                (*alias).to_string(),
                serde_json::Value::String(fast_model_slug.to_string()),
            );
        }
    }
    claude_model_config_with_overrides(available_models, model_overrides)
}

fn claude_model_config_for_provider(
    available_models: &[String],
    provider: &str,
) -> serde_json::Value {
    let model_overrides = available_models
        .iter()
        .filter_map(|model| {
            let slug = if provider == BYOK_PROVIDER_ID {
                byok_model_slug(model)
            } else {
                model_slug_for_provider(model, provider).to_string()
            };
            (slug != model.as_str()).then(|| (model.clone(), serde_json::Value::String(slug)))
        })
        .collect::<serde_json::Map<_, _>>();
    claude_model_config_with_overrides(available_models.to_vec(), model_overrides)
}

fn claude_model_config_with_overrides(
    available_models: Vec<String>,
    model_overrides: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    config.insert("availableModels".to_string(), json!(available_models));
    config.insert("preserveDefaultModel".to_string(), json!(false));
    if !model_overrides.is_empty() {
        config.insert(
            "modelOverrides".to_string(),
            serde_json::Value::Object(model_overrides),
        );
    }
    serde_json::Value::Object(config)
}

fn claude_provider_proxy_base_url() -> String {
    let base_url = acp_core::codex_api_proxy_base_url();
    base_url
        .strip_suffix("/v1")
        .unwrap_or(base_url.as_str())
        .to_string()
}

fn claude_provider_proxy_base_url_for_provider(provider: &str) -> String {
    format!("{}/providers/{provider}", claude_provider_proxy_base_url())
}

fn claude_proxy_kind_env(proxy_kind: AgentProviderProxyKind) -> &'static str {
    match proxy_kind {
        AgentProviderProxyKind::ClaudeNative => "claude_native",
        AgentProviderProxyKind::CompletionToClaude => "completion_to_claude",
        _ => "unsupported",
    }
}

#[cfg(test)]
mod tests;
