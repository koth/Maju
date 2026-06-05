use crate::AppPaths;
use crate::remote_ssh::{
    RemoteSshCommand, RemoteSshCommandRunner, SystemRemoteSshCommandRunner, first_nonempty,
    sanitize_ssh_diagnostic,
};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml_edit::{DocumentMut, Item, Table, value};
use workspace_model::{
    AgentCliId, AgentCliStatus, AgentProviderFamily, AgentProviderProfile, AgentProviderProxyKind,
    AgentSettingsSnapshot, AppSettings, AppTheme, ClaudeProviderSettings,
    ClaudeProviderSettingsStatus, CodexAcpSettingsStatus, CodexConnectionMode, LspProbeResult,
    LspServerConfigInput, LspServerSettings, LspServerSettingsEntry, LspSettingsSnapshot,
    RemoteMachineProfile,
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

#[derive(Debug, Clone, Copy)]
struct AgentCliDefinition {
    id: AgentCliId,
    label: &'static str,
    binary: &'static str,
    acp_arg: &'static str,
}

const AGENTS: &[AgentCliDefinition] = &[
    AgentCliDefinition {
        id: AgentCliId::ClaudeAgentAcp,
        label: "Claude",
        binary: "claude-agent-acp",
        acp_arg: "",
    },
    AgentCliDefinition {
        id: AgentCliId::CodexAcp,
        label: "Codex",
        binary: "codex-acp",
        acp_arg: "",
    },
    AgentCliDefinition {
        id: AgentCliId::Codebuddy,
        label: "CodeBuddy",
        binary: "codebuddy",
        acp_arg: "--acp",
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

pub fn remote_settings_snapshot(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
) -> Result<AgentSettingsSnapshot> {
    remote_settings_snapshot_with_runner(profile, ssh_password, &SystemRemoteSshCommandRunner)
}

pub fn remote_select_agent(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    agent: AgentCliId,
) -> Result<AgentSettingsSnapshot> {
    remote_select_agent_with_runner(profile, ssh_password, agent, &SystemRemoteSshCommandRunner)
}

pub fn remote_select_theme(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    theme: AppTheme,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| select_theme(paths, theme),
    )
}

pub fn remote_select_agent_provider_profile(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    family: AgentProviderFamily,
    profile_id: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| select_agent_provider_profile(paths, family, profile_id),
    )
}

pub fn remote_save_agent_provider_secret(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    family: AgentProviderFamily,
    profile_id: &str,
    secret: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_agent_provider_secret(paths, family, profile_id, secret),
    )
}

pub fn remote_save_provider_models(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    provider: &str,
    models: Vec<String>,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_provider_models(paths, provider, models),
    )
}

pub fn remote_reset_provider_models(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    provider: &str,
) -> Result<AgentSettingsSnapshot> {
    remote_update_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| reset_provider_models(paths, provider),
    )
}

pub fn remote_lsp_settings_snapshot(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
) -> Result<LspSettingsSnapshot> {
    remote_lsp_settings_snapshot_with_runner(profile, ssh_password, &SystemRemoteSshCommandRunner)
}

pub fn remote_save_lsp_server_config(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    config: LspServerConfigInput,
) -> Result<LspSettingsSnapshot> {
    remote_update_lsp_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| save_lsp_server_config(paths, config).map(|_| ()),
    )
}

pub fn remote_reset_lsp_server_config(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    language_id: &str,
) -> Result<LspSettingsSnapshot> {
    remote_update_lsp_settings_with_runner(
        profile,
        ssh_password,
        &SystemRemoteSshCommandRunner,
        |paths| reset_lsp_server_config(paths, language_id).map(|_| ()),
    )
}

pub fn remote_probe_lsp_server(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    command: &str,
) -> Result<LspProbeResult> {
    remote_probe_lsp_server_with_runner(
        profile,
        ssh_password,
        command,
        &SystemRemoteSshCommandRunner,
    )
}

const REMOTE_SETTINGS_TIMEOUT: Duration = Duration::from_secs(12);
const REMOTE_SETTINGS_WRITE_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_SETTINGS_FILES: &[&str] = &[
    "config/settings.json",
    "config/provider-secrets.json",
    "config/provider-models.json",
    "config/config.toml",
];

#[derive(Debug, Clone, Deserialize)]
struct RemoteSettingsExport {
    home: String,
    files: BTreeMap<String, Option<String>>,
    agents: BTreeMap<String, Option<String>>,
    env_override: Option<String>,
}

struct RemoteSettingsMirror {
    paths: AppPaths,
    temp_root: PathBuf,
    export: RemoteSettingsExport,
}

impl Drop for RemoteSettingsMirror {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

fn remote_settings_snapshot_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mirror = pull_remote_settings(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

fn remote_select_agent_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    agent: AgentCliId,
    runner: &R,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    let status = remote_agent_statuses(&mirror.export, agent)
        .into_iter()
        .find(|status| status.id == agent)
        .ok_or_else(|| anyhow!("Unsupported agent"))?;
    if !status.installed {
        anyhow::bail!("{} is not installed on remote", status.binary);
    }
    let mut settings = load_app_settings(&mirror.paths);
    settings.selected_agent = agent;
    save_app_settings(&mirror.paths, &settings)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

fn remote_update_settings_with_runner<R, F>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    update: F,
) -> Result<AgentSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
    F: FnOnce(&AppPaths) -> Result<AgentSettingsSnapshot>,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    let _ = update(&mirror.paths)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    Ok(settings_snapshot_from_remote_mirror(&mirror))
}

fn remote_lsp_settings_snapshot_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let mirror = pull_remote_settings(profile, ssh_password, runner)?;
    remote_lsp_snapshot_from_mirror(profile, ssh_password, runner, &mirror)
}

fn remote_update_lsp_settings_with_runner<R, F>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    update: F,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
    F: FnOnce(&AppPaths) -> Result<()>,
{
    let mut mirror = pull_remote_settings(profile, ssh_password, runner)?;
    update(&mirror.paths)?;
    push_remote_settings(profile, ssh_password, runner, &mirror.paths)?;
    mirror.export = pull_remote_settings_export(profile, ssh_password, runner)?;
    remote_lsp_snapshot_from_mirror(profile, ssh_password, runner, &mirror)
}

fn pull_remote_settings<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<RemoteSettingsMirror>
where
    R: RemoteSshCommandRunner,
{
    let export = pull_remote_settings_export(profile, ssh_password, runner)?;
    let temp_root = unique_remote_settings_temp_root();
    let paths = AppPaths::from_root(temp_root.join(".kodex"));
    for (relative, content) in &export.files {
        let Some(content) = content else {
            continue;
        };
        let target = paths.root().join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create mirror directory {}", parent.display())
            })?;
        }
        std::fs::write(&target, content)
            .with_context(|| format!("failed to write mirror file {}", target.display()))?;
    }
    Ok(RemoteSettingsMirror {
        paths,
        temp_root,
        export,
    })
}

fn pull_remote_settings_export<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
) -> Result<RemoteSettingsExport>
where
    R: RemoteSshCommandRunner,
{
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        profile.ssh_target.clone(),
        profile.ssh_port,
        remote_settings_export_command(),
        ssh_password,
        REMOTE_SETTINGS_TIMEOUT,
    ));
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程设置读取失败", &output)));
    }
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("远程设置响应不是有效 JSON：{}", output.stdout.trim()))
}

fn push_remote_settings<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    paths: &AppPaths,
) -> Result<()>
where
    R: RemoteSshCommandRunner,
{
    let mut files = BTreeMap::<String, Option<String>>::new();
    for relative in REMOTE_SETTINGS_FILES {
        let path = paths.root().join(relative);
        let content = std::fs::read_to_string(&path).ok();
        files.insert((*relative).to_string(), content);
    }
    let stdin = serde_json::to_vec(&json!({ "files": files }))?;
    let output = runner.run_ssh_command(
        &RemoteSshCommand::new(
            profile.ssh_target.clone(),
            profile.ssh_port,
            remote_settings_import_command(),
            ssh_password,
            REMOTE_SETTINGS_WRITE_TIMEOUT,
        )
        .with_stdin(stdin),
    );
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程设置写入失败", &output)));
    }
    Ok(())
}

fn settings_snapshot_from_remote_mirror(mirror: &RemoteSettingsMirror) -> AgentSettingsSnapshot {
    let mut snapshot = settings_snapshot(&mirror.paths);
    snapshot.agents = remote_agent_statuses(&mirror.export, snapshot.settings.selected_agent);
    snapshot.env_override = mirror.export.env_override.clone();
    snapshot.codex_acp.config_path =
        remote_settings_path(&mirror.export.home, "config/config.toml");
    snapshot
}

fn remote_lsp_snapshot_from_mirror<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    runner: &R,
    mirror: &RemoteSettingsMirror,
) -> Result<LspSettingsSnapshot>
where
    R: RemoteSshCommandRunner,
{
    let settings = load_app_settings(&mirror.paths);
    let mut servers = Vec::new();
    for server in all_effective_lsp_servers(&settings) {
        let probe = if server.enabled {
            remote_probe_lsp_server_with_runner(profile, ssh_password, &server.command, runner)?
        } else {
            LspProbeResult {
                available: false,
                resolved_path: None,
                message: Some("Language server disabled".into()),
            }
        };
        servers.push(LspServerSettingsEntry {
            language_id: server.language_id,
            display_name: server.display_name,
            enabled: server.enabled,
            command: server.command,
            args: server.args,
            default_command: server.default_command,
            default_args: server.default_args,
            available: probe.available,
            resolved_path: probe.resolved_path,
            running: false,
            message: probe.message,
            customized: server.customized,
        });
    }
    Ok(LspSettingsSnapshot { servers })
}

fn remote_probe_lsp_server_with_runner<R>(
    profile: &RemoteMachineProfile,
    ssh_password: Option<&str>,
    command: &str,
    runner: &R,
) -> Result<LspProbeResult>
where
    R: RemoteSshCommandRunner,
{
    let output = runner.run_ssh_command(&RemoteSshCommand::new(
        profile.ssh_target.clone(),
        profile.ssh_port,
        remote_lsp_probe_command(command),
        ssh_password,
        REMOTE_SETTINGS_TIMEOUT,
    ));
    if !output.success {
        return Err(anyhow!(remote_settings_error("远程 LSP 探测失败", &output)));
    }
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("远程 LSP 探测响应不是有效 JSON：{}", output.stdout.trim()))
}

fn remote_agent_statuses(
    export: &RemoteSettingsExport,
    selected_agent: AgentCliId,
) -> Vec<AgentCliStatus> {
    AGENTS
        .iter()
        .map(|definition| {
            let detected_path = export
                .agents
                .get(definition.binary)
                .and_then(|path| path.as_deref())
                .filter(|path| !path.trim().is_empty())
                .map(|path| PathBuf::from(path.trim()));
            AgentCliStatus {
                id: definition.id,
                label: definition.label.to_string(),
                binary: definition.binary.to_string(),
                installed: detected_path.is_some(),
                detected_path,
                selected: definition.id == selected_agent,
            }
        })
        .collect()
}

fn unique_remote_settings_temp_root() -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "kodex-remote-settings-{}-{now}",
        std::process::id()
    ))
}

fn remote_settings_path(home: &str, relative: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/.kodex/{}",
        home.trim_end_matches('/'),
        relative.trim_start_matches('/')
    ))
}

fn remote_settings_error(prefix: &str, output: &crate::remote_ssh::RemoteSshOutput) -> String {
    if output.timed_out {
        return format!("{prefix}：SSH 命令超时");
    }
    let message = first_nonempty(&output.stderr, &output.stdout)
        .map(sanitize_ssh_diagnostic)
        .unwrap_or_else(|| "SSH 命令失败但没有输出".into());
    format!("{prefix}：{message}")
}

fn remote_settings_export_command() -> String {
    format!(
        "node -e {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const path = require('path');
const cp = require('child_process');
const os = require('os');
const home = process.env.HOME || os.homedir();
const root = path.join(home, '.kodex');
const rels = ['config/settings.json', 'config/provider-secrets.json', 'config/config.toml'];
function read(rel) {
  try { return fs.readFileSync(path.join(root, rel), 'utf8'); }
  catch (error) {
    if (error && error.code === 'ENOENT') return null;
    throw error;
  }
}
function which(binary) {
  const result = cp.spawnSync('sh', ['-lc', `command -v ${binary} 2>/dev/null || true`], { encoding: 'utf8' });
  return (result.stdout || '').trim() || null;
}
const files = {};
for (const rel of rels) files[rel] = read(rel);
const agents = {
  'claude-agent-acp': which('claude-agent-acp'),
  'codex-acp': which('codex-acp'),
  'codebuddy': which('codebuddy')
};
console.log(JSON.stringify({ home, files, agents, env_override: process.env.ACP_AGENT_COMMAND || null }));
"#,
        )
    )
}

fn remote_settings_import_command() -> String {
    format!(
        "node -e {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const path = require('path');
const os = require('os');
const home = process.env.HOME || os.homedir();
const root = path.join(home, '.kodex');
const chunks = [];
process.stdin.on('data', (chunk) => chunks.push(chunk));
process.stdin.on('end', () => {
  const payload = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
  const files = payload.files || {};
  for (const [rel, content] of Object.entries(files)) {
    if (content == null) continue;
    if (path.isAbsolute(rel) || rel.split(/[\\/]+/).includes('..')) throw new Error('invalid settings path');
    const target = path.join(root, rel);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, String(content), 'utf8');
  }
  console.log(JSON.stringify({ ok: true }));
});
"#,
        )
    )
}

fn remote_lsp_probe_command(command: &str) -> String {
    format!(
        "node -e {} -- {}",
        shell_words::quote(
            r#"
const fs = require('fs');
const cp = require('child_process');
const command = process.argv[1] || '';
const binary = command.trim().split(/\s+/)[0] || '';
if (!binary) {
  console.log(JSON.stringify({ available: false, resolvedPath: null, message: 'Command is empty' }));
  process.exit(0);
}
let resolvedPath = null;
if (binary.includes('/')) {
  try {
    const stat = fs.statSync(binary);
    if (stat.isFile()) resolvedPath = binary;
  } catch (_) {}
} else {
  const escaped = binary.replace(/'/g, `'\\''`);
  const result = cp.spawnSync('sh', ['-lc', `command -v '${escaped}' 2>/dev/null || true`], { encoding: 'utf8' });
  resolvedPath = (result.stdout || '').trim() || null;
}
console.log(JSON.stringify({
  available: !!resolvedPath,
  resolvedPath,
  message: resolvedPath ? null : `${binary} not found on remote PATH`
}));
"#,
        ),
        shell_words::quote(command)
    )
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
        if !detect_agent_with_paths(paths, AgentCliId::Codebuddy).installed {
            return settings.selected_agent;
        }
        return AgentCliId::Codebuddy;
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
    } else if agent == AgentCliId::ClaudeAgentAcp {
        Some(claude_agent_acp_command(paths))
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
    if is_claude_agent_acp_command(command) {
        return claude_agent_acp_env(paths);
    }

    if !is_codex_acp_command(command) {
        return Vec::new();
    }
    if load_app_settings(paths).codex_connection_mode == CodexConnectionMode::Default {
        return Vec::new();
    }

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

pub fn ensure_agent_ready_for_command(command: &str, paths: &AppPaths) -> Result<()> {
    let settings = load_app_settings(paths);
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

fn is_codex_acp_command(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("codex-acp") || command.contains("kodex-acp")
}

fn claude_agent_configured_for_settings(paths: &AppPaths, settings: &AppSettings) -> bool {
    let selected_profile_id = selected_claude_provider_profile_id(settings);
    if selected_profile_id == BYOK_PROVIDER_ID {
        return !configured_claude_byok_source_keys(paths).is_empty();
    }
    provider_secret(paths, AgentProviderFamily::Claude, &selected_profile_id)
        .map(|secret| !secret.trim().is_empty())
        .unwrap_or(false)
}

fn codex_agent_configured_for_settings(paths: &AppPaths, settings: &AppSettings) -> bool {
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
    ClaudeProviderSettingsStatus {
        selected_profile_id: selected_profile_id.clone(),
        profiles: provider_profiles(paths, AgentProviderFamily::Claude, &selected_profile_id),
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
        let active_model_slug = byok_model_slug_with_hint(&active_model, source_provider_hint);
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug);
        }
        let active_source_provider =
            byok_source_provider_for_model_with_hint(&active_model, source_provider_hint);
        let active_upstream_model =
            byok_upstream_model_for_model_with_hint(&active_model, source_provider_hint);
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
        let active_model_slug = byok_model_slug_with_hint(&active_model, source_provider_hint);
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug.clone());
            changed = true;
        }
        let active_model_provider =
            byok_source_provider_for_model_with_hint(&active_model, source_provider_hint);
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
            byok_upstream_model_for_model_with_hint(&active_model, source_provider_hint);
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

fn write_codex_acp_model_catalog(paths: &AppPaths, provider: &str) -> Result<()> {
    let path = codex_model_catalog_path(paths);
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
    let content = serde_json::to_string_pretty(&catalog)?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write Codex model catalog {}", path.display()))
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
        "experimental_supported_tools": [],
        "input_modalities": if is_deepseek { json!(["text"]) } else { json!(["text", "image"]) },
        "supports_search_tool": !is_deepseek
    })
}

fn apply_patch_tool_type_for_provider(_provider: &str) -> &'static str {
    "function"
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
    let available_models = if selected_profile_id == BYOK_PROVIDER_ID {
        configured_claude_byok_models(paths)
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
    if available_models.is_empty() {
    } else {
        let model_config = if selected_profile_id == TIMIAI_PROVIDER_ID {
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

fn configured_claude_byok_models(paths: &AppPaths) -> Vec<String> {
    let mut models = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if byok_source_secret(paths, AgentProviderFamily::Claude, provider).is_none() {
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
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

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
        fn run_ssh_command(
            &self,
            command: &RemoteSshCommand,
        ) -> crate::remote_ssh::RemoteSshOutput {
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
                    "config/config.toml": null,
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

        assert_eq!(settings.selected_agent, AgentCliId::ClaudeAgentAcp);
        assert_eq!(settings.theme, AppTheme::Graphite);
        assert_eq!(
            settings.selected_claude_provider_profile_id.as_deref(),
            Some(BYOK_PROVIDER_ID)
        );
        assert!(settings.claude.available_models.is_empty());
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
        };

        save_app_settings(&paths, &settings).unwrap();
        let loaded = load_app_settings(&paths);

        assert_eq!(loaded, settings);
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
            Some(AgentCliId::ClaudeAgentAcp)
        );
        assert!(
            snapshot
                .agents
                .first()
                .map(|agent| agent.selected)
                .unwrap_or(false)
        );
        assert_eq!(snapshot.claude.selected_profile_id, BYOK_PROVIDER_ID);
        assert_eq!(snapshot.codex_acp.profiles.len(), 7);
        assert_eq!(snapshot.claude.profiles.len(), 6);
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

        let error = select_agent_provider_profile(&paths, AgentProviderFamily::Codex, "missing")
            .unwrap_err();

        assert!(error.to_string().contains("Unsupported provider profile"));
    }

    #[test]
    fn invalid_settings_default_to_claude_byok() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(settings_path(&paths), "not json").unwrap();

        let settings = load_app_settings(&paths);

        assert_eq!(settings.selected_agent, AgentCliId::ClaudeAgentAcp);
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
        let _path_guard =
            EnvVarGuard::set_path(std::env::join_paths([bin_dir.as_os_str()]).unwrap());
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        assert_eq!(default_agent_for_new_work(&paths), AgentCliId::Codebuddy);
        assert!(
            resolve_agent_command_with_settings(&paths)
                .to_lowercase()
                .contains("codebuddy")
        );
    }

    #[test]
    fn default_agent_for_new_work_keeps_configured_claude_byok() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join(binary_name("codebuddy")), "fake").unwrap();
        let _path_guard =
            EnvVarGuard::set_path(std::env::join_paths([bin_dir.as_os_str()]).unwrap());
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        save_agent_provider_secret(
            &paths,
            AgentProviderFamily::Claude,
            MIMO_PROVIDER_ID,
            "mimo-secret",
        )
        .unwrap();

        assert_eq!(
            default_agent_for_new_work(&paths),
            AgentCliId::ClaudeAgentAcp
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
        assert_eq!(doc["model"].as_str(), Some(TIMIAI_CODEX_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(TIMIAI_PROVIDER_ID));
        assert_eq!(
            doc["preferred_auth_method"].as_str(),
            Some(CODEX_AUTH_METHOD_API_KEY)
        );
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(model_context_window(TIMIAI_CODEX_MODEL))
        );
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
        assert!(catalog.contains("\"slug\": \"gpt-5.5\""));
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
                && model["provider"].as_str() == Some(TIMIAI_PROVIDER_ID)
                && model["_meta"]["provider"].as_str() == Some(TIMIAI_PROVIDER_ID)
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
        assert_eq!(slugs, vec!["deepseek-v4-pro", "deepseek-v4-flash"]);
        assert!(
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| { model["apply_patch_tool_type"].as_str() == Some("function") })
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

        write_codex_acp_provider_config(&paths, COMMANDCODE_PROVIDER_ID, "commandcode-secret")
            .unwrap();

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(COMMANDCODE_MODEL));
        assert_eq!(
            doc["model_provider"].as_str(),
            Some(COMMANDCODE_PROVIDER_ID)
        );
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(model_context_window_for_provider(
                COMMANDCODE_MODEL,
                COMMANDCODE_PROVIDER_ID
            ))
        );
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
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(model_context_window_for_provider(
                "Qwen/Qwen3.7-Max",
                COMMANDCODE_PROVIDER_ID
            ))
        );
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
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(model_context_window_for_provider(
                "Qwen/Qwen3.7-Max",
                COMMANDCODE_PROVIDER_ID
            ))
        );
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
            assert!(configured_models.contains(&expected_model));
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
            slugs.contains(
                &byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str()
            )
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

        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let display_names = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["display_name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(display_names.contains(&"gpt-5.6"));
        assert!(display_names.contains(&"claude-opus-4.9"));
        assert!(!display_names.contains(&TIMIAI_CODEX_MODEL));

        let snapshot = reset_provider_models(&paths, TIMIAI_PROVIDER_ID).unwrap();
        let profile = snapshot
            .codex_acp
            .profiles
            .iter()
            .find(|profile| profile.id == TIMIAI_PROVIDER_ID)
            .unwrap();
        assert!(profile.models.contains(&TIMIAI_CODEX_MODEL.to_string()));
        assert!(!profile.models.contains(&"gpt-5.6".to_string()));
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
        assert!(
            available_models.contains(&serde_json::Value::String(TIMIAI_CODEX_MODEL.to_string()))
        );
        assert!(
            available_models.contains(&serde_json::Value::String(TIMIAI_CLAUDE_MODEL.to_string()))
        );
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
        let snapshot = select_agent_provider_profile(
            &paths,
            AgentProviderFamily::Claude,
            COMMANDCODE_PROVIDER_ID,
        )
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
        assert!(
            available_models.contains(&serde_json::Value::String(COMMANDCODE_MODEL.to_string()))
        );
        assert!(
            available_models.contains(&serde_json::Value::String("Qwen/Qwen3.7-Max".to_string()))
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
                .all(|model| { model["apply_patch_tool_type"].as_str() == Some("function") })
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
        let slugs = models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!slugs.contains(&byok_model_slug(DEEPSEEK_MODEL).as_str()));
        assert!(slugs.contains(&model_slug_for_provider(KIMI_MODEL, KIMI_PROVIDER_ID)));
        assert!(!slugs.contains(&byok_model_slug(MIMO_MODEL).as_str()));
        assert!(models.iter().any(|model| {
            model["display_name"].as_str() == Some(KIMI_MODEL)
                && model["slug"].as_str() == Some(KIMI_MODEL)
        }));
    }

    #[test]
    fn codex_acp_model_metadata_tracks_individual_model_limits() {
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
            slugs.contains(
                &byok_encoded_model_slug(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID).as_str()
            )
        );
        assert!(
            slugs.contains(&byok_encoded_model_slug(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID).as_str())
        );
        assert_eq!(
            catalog["models"][0]["provider"].as_str(),
            Some(TIMIAI_PROVIDER_ID)
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

        let error = write_codex_acp_provider_config(&paths, TIMIAI_PROVIDER_ID, "timiai-secret")
            .unwrap_err();

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
        assert!(
            env.iter().any(|(name, value)| name == "ANTHROPIC_BASE_URL"
                && value.starts_with("http://127.0.0.1:"))
        );
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
        assert!(
            available_models.contains(&serde_json::Value::String("Qwen/Qwen3.7-Max".to_string()))
        );
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
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(DEFAULT_MODEL_CONTEXT_WINDOW)
        );
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
