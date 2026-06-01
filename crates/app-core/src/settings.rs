use crate::AppPaths;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item, Table, value};
use workspace_model::{
    AgentCliId, AgentCliStatus, AgentProviderFamily, AgentProviderProfile, AgentProviderProxyKind,
    AgentSettingsSnapshot, AppSettings, AppTheme, ClaudeWoaConfigInput, ClaudeWoaSettings,
    ClaudeWoaSettingsStatus, CodexAcpSettingsStatus, CodexConnectionMode,
    InitialSetupRecommendation, IoaEnvironmentStatus, LspServerConfigInput, LspServerSettings,
};

const SETTINGS_FILE: &str = "settings.json";
const PROVIDER_SECRETS_FILE: &str = "provider-secrets.json";
const CODEX_CONFIG_FILE: &str = "config.toml";
const CODEX_HOME_ENV: &str = "CODEX_HOME";
const IOA_ENV_DETECT_URL: &str = "https://cloud.tencent.com/auth-api/common/platform";
const IOA_ENV_DETECT_MAX_ATTEMPTS: usize = 3;
const IOA_ENV_DETECT_RETRY_DELAY: Duration = Duration::from_secs(2);
const IOA_ENV_DETECT_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_DEFAULT_PROVIDER_ID: &str = "default";
const BYOK_PROVIDER_ID: &str = "byok";
const BYOK_PROVIDER_NAME: &str = "BYOK";
const BYOK_API_KEY_ENV: &str = "BYOK_API_KEY";
const BYOK_SOURCE_PROVIDER_IDS: &[&str] =
    &[DEEPSEEK_PROVIDER_ID, KIMI_PROVIDER_ID, MIMO_PROVIDER_ID];
const VENUS_MODEL: &str = "glm-5.1";
const VENUS_PROVIDER_ID: &str = "venus";
const VENUS_PROVIDER_NAME: &str = "Venus LLM";
const VENUS_WIRE_API: &str = "responses";
const VENUS_API_KEY_ENV: &str = "VENUS_API_KEY";
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
    "deepseek-v4-pro",
    "deepseek-v4-flash",
];
const CLAUDE_WOA_PROVIDER_ID: &str = "woa";
const CODEX_WOA_PROVIDER_ID: &str = "woa";
const CODEX_WOA_PROVIDER_NAME: &str = "WOA";
const CODEX_WOA_API_KEY_ENV: &str = "AUTH_TOKEN";
const CODEX_WOA_LEGACY_API_KEY_ENV: &str = "CODEX_WOA_API_KEY";
const CODEX_WOA_OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const CODEX_WOA_CODEX_API_KEY_ENV: &str = "CODEX_API_KEY";
const CODEX_WOA_BASE_URL: &str = "https://copilot.code.woa.com/server/chat/codebuddy-gateway/codex";
const CODEX_WOA_APP_VERSION: &str = "0.0.9";
const CODEX_WOA_APP_VERSION_ENV: &str = "CODEX_INTERNAL_APP_VERSION";
const CODEX_WOA_USER_AGENT_ENV: &str = "CODEX_INTERNAL_USER_AGENT";
const CODEX_WOA_CONVERSATION_ID_ENV: &str = "CODEX_INTERNAL_CONVERSATION_ID";
const CODEX_WOA_GIT_REPOS_ENV: &str = "CODEX_INTERNAL_GIT_REPOS";
const CODEX_WOA_KNOT_API_KEY_ENV: &str = "CODEBUDDY_API_KEY";
const CODEX_WOA_YOLO_MODE_ENV: &str = "CODEX_INTERNAL_YOLO_MODE";
const CODEX_WOA_ANYDEV_MODE_ENV: &str = "CODEX_INTERNAL_ANYDEV_MODE";
const CODEX_WOA_SPECIFY_MODEL_ENV: &str = "CODEX_INTERNAL_SPECIFY_MODEL";
const CODEX_WOA_MODEL: &str = "gpt-5.4";
const CODEX_WOA_CATALOG_MODELS: &[&str] = &[
    CODEX_WOA_MODEL,
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
];
const DEFAULT_CLAUDE_WOA_AVAILABLE_MODELS: &[&str] =
    &["claude-opus-4-7[1m]", "claude-opus-4-6[1m]"];
const VENUS_MODEL_CONTEXT_WINDOW: i64 = 200_000;
const VENUS_MODEL_MAX_OUTPUT_TOKENS: i64 = 128_000;
const KIMI_MODEL_CONTEXT_WINDOW: i64 = 262_144;
const KIMI_MODEL_MAX_OUTPUT_TOKENS: i64 = 32_768;
const MIMO_MODEL_CONTEXT_WINDOW: i64 = 1_000_000;
const MIMO_MODEL_MAX_OUTPUT_TOKENS: i64 = 128_000;
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
const KIMI_CATALOG_MODELS: &[&str] = &[KIMI_MODEL];
const MIMO_CATALOG_MODELS: &[&str] = &["MiMo-V2.5-Pro", "MiMo-V2.5"];
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
    ("claude-opus-4.8", "claude-opus-4-8"),
    ("deepseek-v4-pro", "deepseek-v4-pro-external"),
    ("deepseek-v4-flash", "deepseek-v4-flash-external"),
];

const MIMO_MODEL_SLUG_MAP: &[(&str, &str)] = &[
    ("MiMo-V2.5-Pro", "mimo-v2.5-pro"),
    ("MiMo-V2.5", "mimo-v2.5"),
];

const VENUS_MODEL_CONTEXT_WINDOWS: &[(&str, i64)] = &[
    (VENUS_MODEL, VENUS_MODEL_CONTEXT_WINDOW),
    (CODEX_WOA_MODEL, 1_050_000),
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
    (VENUS_MODEL, VENUS_MODEL_MAX_OUTPUT_TOKENS),
    (CODEX_WOA_MODEL, 128_000),
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
        id: VENUS_PROVIDER_ID,
        label: "Venus",
        proxy_kind: AgentProviderProxyKind::CompletionToResponses,
        base_url: None,
        default_model: Some(VENUS_MODEL),
        models: VENUS_CATALOG_MODELS,
        credential_label: Some("Venus API key"),
        requires_credential: true,
        help_text: "通过本机 Codex API Proxy 将 Responses 请求转为 Venus chat completions。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Codex,
        id: CODEX_WOA_PROVIDER_ID,
        label: CODEX_WOA_PROVIDER_NAME,
        proxy_kind: AgentProviderProxyKind::Responses,
        base_url: Some(CODEX_WOA_BASE_URL),
        default_model: Some(CODEX_WOA_MODEL),
        models: CODEX_WOA_CATALOG_MODELS,
        credential_label: None,
        requires_credential: false,
        help_text: "复用 WOA 登录 token，直连 CodeBuddy Codex gateway 的 Responses API。",
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
        id: CLAUDE_WOA_PROVIDER_ID,
        label: "WOA",
        proxy_kind: AgentProviderProxyKind::ClaudeWoa,
        base_url: Some("codebuddy-gateway"),
        default_model: None,
        models: &[],
        credential_label: None,
        requires_credential: false,
        help_text: "使用 Tencent WOA 登录，并通过 Claude Agent ACP 启动会话。",
    },
    ProviderProfileDefinition {
        family: AgentProviderFamily::Claude,
        id: VENUS_PROVIDER_ID,
        label: "Venus",
        proxy_kind: AgentProviderProxyKind::CompletionToClaude,
        base_url: None,
        default_model: Some("claude-sonnet-4.6"),
        models: &["claude-sonnet-4.6", "claude-opus-4.6"],
        credential_label: Some("Venus API key"),
        requires_credential: true,
        help_text: "通过 completion-to-Claude 代理对接 Venus。",
    },
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
        claude_woa: ClaudeWoaSettings::default(),
    }
}

fn default_claude_woa_available_models() -> Vec<String> {
    DEFAULT_CLAUDE_WOA_AVAILABLE_MODELS
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
    if selected_claude_provider_profile_id(settings) == CLAUDE_WOA_PROVIDER_ID
        && settings.claude_woa.available_models.is_empty()
    {
        settings.claude_woa.available_models = default_claude_woa_available_models();
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
        .unwrap_or(VENUS_PROVIDER_ID)
}

pub fn settings_snapshot(paths: &AppPaths) -> AgentSettingsSnapshot {
    let settings = load_app_settings(paths);
    let agents = agent_statuses(paths, settings.selected_agent);
    AgentSettingsSnapshot {
        settings,
        agents,
        env_override: std::env::var("ACP_AGENT_COMMAND").ok(),
        codex_acp: codex_acp_settings_status(paths),
        claude_woa: claude_woa_settings_status(paths),
    }
}

pub async fn detect_ioa_environment() -> IoaEnvironmentStatus {
    let started = Instant::now();
    crate::startup_perf::mark("settings/ioa_env_detect/start", IOA_ENV_DETECT_URL);
    let client = match reqwest::Client::builder()
        .timeout(IOA_ENV_DETECT_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            crate::startup_perf::mark(
                "settings/ioa_env_detect/client_error",
                format!(
                    "duration_ms={} error={error}",
                    started.elapsed().as_millis()
                ),
            );
            return external_ioa_environment_status(Some(format!(
                "Failed to create IOA environment detector: {error}"
            )));
        }
    };

    detect_ioa_environment_with_client(
        &client,
        IOA_ENV_DETECT_URL,
        started,
        IOA_ENV_DETECT_MAX_ATTEMPTS,
        IOA_ENV_DETECT_RETRY_DELAY,
    )
    .await
}

async fn detect_ioa_environment_with_client(
    client: &reqwest::Client,
    url: &str,
    started: Instant,
    max_attempts: usize,
    retry_delay: Duration,
) -> IoaEnvironmentStatus {
    let attempts = max_attempts.max(1);
    let mut last_retryable_error = None;

    for attempt in 1..=attempts {
        match detect_ioa_environment_once(client, url, started, attempt).await {
            IoaDetectAttempt::Detected(status) => return status,
            IoaDetectAttempt::RetryableFailure(message) => {
                if attempt >= attempts {
                    return external_ioa_environment_status(Some(format!(
                        "{message} after {attempts} attempts"
                    )));
                }
                last_retryable_error = Some(message);
                crate::startup_perf::mark(
                    "settings/ioa_env_detect/retry",
                    format!(
                        "attempt={} next_attempt={} delay_ms={} duration_ms={}",
                        attempt,
                        attempt + 1,
                        retry_delay.as_millis(),
                        started.elapsed().as_millis()
                    ),
                );
                if !retry_delay.is_zero() {
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
    }

    external_ioa_environment_status(last_retryable_error)
}

enum IoaDetectAttempt {
    Detected(IoaEnvironmentStatus),
    RetryableFailure(String),
}

async fn detect_ioa_environment_once(
    client: &reqwest::Client,
    url: &str,
    started: Instant,
    attempt: usize,
) -> IoaDetectAttempt {
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            match response.text().await {
                Ok(body) => {
                    if !status.is_success() {
                        crate::startup_perf::mark(
                            "settings/ioa_env_detect/http_error",
                            format!(
                                "attempt={} http_status={} duration_ms={} body_prefix={}",
                                attempt,
                                status.as_u16(),
                                started.elapsed().as_millis(),
                                compact_log_prefix(&body),
                            ),
                        );
                        return IoaDetectAttempt::RetryableFailure(format!(
                            "IOA environment detector returned HTTP {}",
                            status.as_u16()
                        ));
                    }
                    match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(payload) => {
                            let detected_status = ioa_environment_status_from_value(&payload, true);
                            crate::startup_perf::mark(
                                "settings/ioa_env_detect/end",
                                format!(
                                    "attempt={} http_status={} duration_ms={} detected={} company_environment={} is_company_export_ip={} is_internal={} recommended_setup={:?}",
                                    attempt,
                                    status.as_u16(),
                                    started.elapsed().as_millis(),
                                    detected_status.detected,
                                    detected_status.company_environment,
                                    detected_status.is_company_export_ip,
                                    detected_status.is_internal,
                                    detected_status.recommended_setup,
                                ),
                            );
                            IoaDetectAttempt::Detected(detected_status)
                        }
                        Err(error) => {
                            crate::startup_perf::mark(
                                "settings/ioa_env_detect/parse_error",
                                format!(
                                    "attempt={} http_status={} duration_ms={} error={} body_prefix={}",
                                    attempt,
                                    status.as_u16(),
                                    started.elapsed().as_millis(),
                                    error,
                                    compact_log_prefix(&body),
                                ),
                            );
                            IoaDetectAttempt::RetryableFailure(format!(
                                "Failed to parse IOA environment response: {error}"
                            ))
                        }
                    }
                }
                Err(error) => {
                    crate::startup_perf::mark(
                        "settings/ioa_env_detect/body_error",
                        format!(
                            "attempt={} http_status={} duration_ms={} error={error}",
                            attempt,
                            status.as_u16(),
                            started.elapsed().as_millis(),
                        ),
                    );
                    IoaDetectAttempt::RetryableFailure(format!(
                        "Failed to read IOA environment response: {error}"
                    ))
                }
            }
        }
        Err(error) => {
            crate::startup_perf::mark(
                "settings/ioa_env_detect/request_error",
                format!(
                    "attempt={} duration_ms={} error={error}",
                    attempt,
                    started.elapsed().as_millis()
                ),
            );
            IoaDetectAttempt::RetryableFailure(format!("Failed to detect IOA environment: {error}"))
        }
    }
}

fn ioa_environment_status_from_value(
    payload: &serde_json::Value,
    detected: bool,
) -> IoaEnvironmentStatus {
    let data = payload.get("data").unwrap_or(payload);
    let is_company_export_ip = truthy_json_bool(data.get("isCompanyExportIP"));
    let is_internal = truthy_json_bool(data.get("isInternal"));
    let login_method_is_ioa = data
        .get("loginMethod")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|login_method| login_method.eq_ignore_ascii_case("ioa"));
    let company_environment = is_company_export_ip || is_internal || login_method_is_ioa;

    IoaEnvironmentStatus {
        is_company_export_ip,
        is_internal,
        company_environment,
        recommended_setup: if company_environment {
            InitialSetupRecommendation::Woa
        } else {
            InitialSetupRecommendation::CodexByok
        },
        detected,
        timestamp_ms: now_ms(),
        message: None,
    }
}

fn external_ioa_environment_status(message: Option<String>) -> IoaEnvironmentStatus {
    IoaEnvironmentStatus {
        is_company_export_ip: false,
        is_internal: false,
        company_environment: false,
        recommended_setup: InitialSetupRecommendation::CodexByok,
        detected: false,
        timestamp_ms: now_ms(),
        message,
    }
}

fn compact_log_prefix(value: &str) -> String {
    value
        .chars()
        .take(180)
        .collect::<String>()
        .replace('\r', " ")
        .replace('\n', " ")
}

fn truthy_json_bool(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => {
            value.eq_ignore_ascii_case("true") || value == "1"
        }
        Some(serde_json::Value::Number(value)) => value.as_i64() == Some(1),
        _ => false,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
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
        claude_woa: existing.claude_woa,
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
    if codex_active_provider(&config_path) == CODEX_WOA_PROVIDER_ID {
        return codex_woa_agent_env(paths).unwrap_or_default();
    }
    let provider_keys = codex_provider_keys(&config_path);
    if provider_keys.is_empty() {
        codex_active_provider_key(&config_path)
            .map(|(env_key, api_key)| vec![(env_key, api_key)])
            .unwrap_or_default()
    } else {
        provider_keys
    }
}

pub fn ensure_agent_ready_for_command(command: &str, paths: &AppPaths) -> Result<()> {
    let settings = load_app_settings(paths);
    if is_codex_acp_command(command)
        && settings.codex_connection_mode != CodexConnectionMode::Default
        && selected_codex_provider_profile_id(paths, &settings) == CODEX_WOA_PROVIDER_ID
    {
        ensure_woa_token_for_settings(paths, &settings)?;
        return Ok(());
    }
    if !is_claude_agent_acp_command(command) {
        return Ok(());
    }
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    if selected_profile_id != CLAUDE_WOA_PROVIDER_ID {
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
    ensure_woa_token_for_settings(paths, &settings).map(|_| ())
}

fn ensure_woa_token_for_settings(
    paths: &AppPaths,
    settings: &AppSettings,
) -> Result<crate::claude_woa::WoaToken> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create WOA token runtime")?
        .block_on(crate::claude_woa::ensure_token_ready(
            &managed_claude_woa_settings(paths, &settings.claude_woa),
        ))
}

fn codex_woa_agent_env(paths: &AppPaths) -> Result<Vec<(String, String)>> {
    let settings = load_app_settings(paths);
    let token = ensure_woa_token_for_settings(paths, &settings)?;
    let access_token = token.access_token;
    Ok(vec![
        (CODEX_WOA_API_KEY_ENV.to_string(), access_token.clone()),
        (
            CODEX_WOA_LEGACY_API_KEY_ENV.to_string(),
            access_token.clone(),
        ),
        (
            CODEX_WOA_OPENAI_API_KEY_ENV.to_string(),
            access_token.clone(),
        ),
        (CODEX_WOA_CODEX_API_KEY_ENV.to_string(), access_token),
        (
            "openai_base_url".to_string(),
            CODEX_WOA_BASE_URL.trim_end_matches('/').to_string(),
        ),
        (
            CODEX_WOA_APP_VERSION_ENV.to_string(),
            CODEX_WOA_APP_VERSION.to_string(),
        ),
        (
            CODEX_WOA_USER_AGENT_ENV.to_string(),
            format!("Codex-Internal/{CODEX_WOA_APP_VERSION}"),
        ),
        (
            CODEX_WOA_CONVERSATION_ID_ENV.to_string(),
            uuid::Uuid::new_v4().to_string(),
        ),
    ])
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
    if selected_profile_id == CLAUDE_WOA_PROVIDER_ID {
        let status =
            crate::claude_woa::status(&managed_claude_woa_settings(paths, &settings.claude_woa));
        return status.token.exists && !status.token.malformed;
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
    if selected_profile_id == CODEX_WOA_PROVIDER_ID {
        let status =
            crate::claude_woa::status(&managed_claude_woa_settings(paths, &settings.claude_woa));
        return status.token.exists && !status.token.malformed;
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

pub fn claude_woa_token_path(paths: &AppPaths) -> PathBuf {
    paths.root().join("claude-woa-token.json")
}

pub fn claude_woa_settings_status(paths: &AppPaths) -> ClaudeWoaSettingsStatus {
    let settings = load_app_settings(paths);
    let mut status =
        crate::claude_woa::status(&managed_claude_woa_settings(paths, &settings.claude_woa));
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    status.selected_profile_id = selected_profile_id.clone();
    status.profiles = provider_profiles(paths, AgentProviderFamily::Claude, &selected_profile_id);
    status
}

pub fn save_claude_woa_config(
    paths: &AppPaths,
    input: ClaudeWoaConfigInput,
) -> Result<AgentSettingsSnapshot> {
    let mut settings = load_app_settings(paths);
    settings.claude_woa = ClaudeWoaSettings {
        channel: input.channel,
        token_path: None,
        available_models: sanitize_claude_available_models(input.available_models),
    };
    save_app_settings(paths, &settings)?;
    Ok(settings_snapshot(paths))
}

pub fn refresh_claude_woa_token(paths: &AppPaths) -> Result<AgentSettingsSnapshot> {
    let settings = load_app_settings(paths);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create WOA token runtime")?
        .block_on(crate::claude_woa::refresh_and_save(
            &managed_claude_woa_settings(paths, &settings.claude_woa),
        ))?;
    Ok(settings_snapshot(paths))
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
        venus_key_configured: codex_venus_key_configured(&config_path),
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
        .unwrap_or(CLAUDE_WOA_PROVIDER_ID);
    if claude_is_byok_source(candidate) {
        return BYOK_PROVIDER_ID.to_string();
    }
    if profile_definition(AgentProviderFamily::Claude, candidate).is_some() {
        candidate.to_string()
    } else {
        CLAUDE_WOA_PROVIDER_ID.to_string()
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
            if definition.id == CODEX_WOA_PROVIDER_ID {
                write_codex_woa_channel_config(paths)?;
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
            if definition.id == CLAUDE_WOA_PROVIDER_ID
                && settings.claude_woa.available_models.is_empty()
            {
                settings.claude_woa.available_models = default_claude_woa_available_models();
            }
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
            if definition.id == VENUS_PROVIDER_ID || definition.id == TIMIAI_PROVIDER_ID {
                save_provider_secret(paths, family, definition.id, secret)?;
                write_codex_acp_provider_config(paths, definition.id, secret)?;
                save_codex_managed_mode_with_profile(paths, definition.id)?;
            } else {
                save_codex_byok_source_secret(paths, definition.id, secret)?;
            }
        }
        AgentProviderFamily::Claude => {
            save_provider_secret(paths, family, definition.id, secret)?;
        }
    }
    Ok(settings_snapshot(paths))
}

pub fn save_codex_acp_venus_key(
    paths: &AppPaths,
    venus_key: &str,
) -> Result<AgentSettingsSnapshot> {
    save_agent_provider_secret(
        paths,
        AgentProviderFamily::Codex,
        VENUS_PROVIDER_ID,
        venus_key,
    )
}

pub fn write_codex_acp_venus_config(paths: &AppPaths, venus_key: &str) -> Result<()> {
    write_codex_acp_provider_config(paths, VENUS_PROVIDER_ID, venus_key)
}

pub fn save_codex_acp_provider_key(
    paths: &AppPaths,
    provider: &str,
    api_key: &str,
) -> Result<AgentSettingsSnapshot> {
    let provider = normalize_codex_provider(provider)?;
    if provider == TIMIAI_PROVIDER_ID {
        save_provider_secret(paths, AgentProviderFamily::Codex, provider, api_key)?;
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

    write_codex_acp_provider_config(paths, provider, &api_key)?;
    save_codex_managed_mode_with_profile(paths, provider)?;
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

    let default_model = configured_codex_byok_models(paths)
        .into_iter()
        .next()
        .unwrap_or_else(|| KIMI_MODEL.to_string());
    doc["model"] = value(byok_model_slug(&default_model));
    doc["model_provider"] = value(BYOK_PROVIDER_ID);
    doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
    doc["model_context_window"] = value(model_context_window(&default_model));
    doc["model_max_output_tokens"] = value(model_max_output_tokens(&default_model));
    doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
    doc["model_catalog_json"] = value(
        codex_model_catalog_path(paths)
            .to_string_lossy()
            .to_string(),
    );
    write_codex_byok_provider_table(&mut doc);
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    write_codex_acp_model_catalog(paths, BYOK_PROVIDER_ID)
}

fn write_codex_woa_channel_config(paths: &AppPaths) -> Result<()> {
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

    let default_model = default_model_for_provider(CODEX_WOA_PROVIDER_ID);
    doc["model"] = value(default_model);
    doc["model_provider"] = value(CODEX_WOA_PROVIDER_ID);
    doc["chatgpt_base_url"] = value(CODEX_WOA_BASE_URL);
    doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
    doc["model_context_window"] = value(model_context_window(default_model));
    doc["model_max_output_tokens"] = value(model_max_output_tokens(default_model));
    doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
    doc["model_catalog_json"] = value(
        codex_model_catalog_path(paths)
            .to_string_lossy()
            .to_string(),
    );
    write_codex_woa_provider_table(&mut doc);
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    write_codex_acp_model_catalog(paths, CODEX_WOA_PROVIDER_ID)
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
    let active_provider = codex_channel_provider_for_source(provider);
    doc["model_provider"] = value(active_provider);
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
    if active_provider == BYOK_PROVIDER_ID {
        write_codex_byok_provider_table(&mut doc);
    }

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write Codex config {}", path.display()))?;
    write_codex_acp_model_catalog(paths, active_provider)
}

fn save_codex_byok_source_secret(paths: &AppPaths, provider: &str, api_key: &str) -> Result<()> {
    let provider = normalize_codex_provider(provider)?;
    if provider == VENUS_PROVIDER_ID
        || provider == BYOK_PROVIDER_ID
        || provider == TIMIAI_PROVIDER_ID
    {
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
        .unwrap_or(VENUS_PROVIDER_ID);

    write_codex_provider_table(&mut doc, provider, key);
    write_codex_byok_provider_table(&mut doc);

    if active_provider == BYOK_PROVIDER_ID || codex_is_byok_source(active_provider) {
        doc["model_provider"] = value(BYOK_PROVIDER_ID);
        let active_model = doc
            .get("model")
            .and_then(|item| item.as_str())
            .unwrap_or_else(|| default_model_for_provider(provider))
            .to_string();
        let active_model_slug = byok_model_slug(&active_model);
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug);
        }
        if doc.get("preferred_auth_method").is_none() {
            doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
        }
        if doc.get("model_context_window").is_none() {
            doc["model_context_window"] = value(model_context_window(&active_model));
        }
        if doc.get("model_max_output_tokens").is_none() {
            doc["model_max_output_tokens"] = value(model_max_output_tokens(&active_model));
        }
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
    }
    Ok(())
}

fn codex_channel_provider_for_source(provider: &str) -> &'static str {
    match provider {
        VENUS_PROVIDER_ID => VENUS_PROVIDER_ID,
        CODEX_WOA_PROVIDER_ID => CODEX_WOA_PROVIDER_ID,
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_ID,
        _ => BYOK_PROVIDER_ID,
    }
}

fn codex_is_byok_source(provider: &str) -> bool {
    BYOK_SOURCE_PROVIDER_IDS.contains(&provider)
}

fn claude_is_byok_source(provider: &str) -> bool {
    BYOK_SOURCE_PROVIDER_IDS.contains(&provider)
}

fn configured_codex_byok_models(paths: &AppPaths) -> Vec<String> {
    let mut models = Vec::new();
    let config_path = codex_config_path(paths);
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if codex_provider_key(&config_path, provider)
            .filter(|key| !key.trim().is_empty())
            .is_none()
        {
            continue;
        }
        for model in catalog_models_for_provider(provider) {
            let model = (*model).to_string();
            if !models.contains(&model) {
                models.push(model);
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

fn codex_provider_keys(path: &Path) -> Vec<(String, String)> {
    let mut providers = Vec::new();
    if codex_active_provider(path) == BYOK_PROVIDER_ID {
        providers.push(BYOK_PROVIDER_ID);
    }
    providers.extend([
        VENUS_PROVIDER_ID,
        TIMIAI_PROVIDER_ID,
        DEEPSEEK_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        MIMO_PROVIDER_ID,
    ]);
    providers
        .into_iter()
        .filter_map(|provider| {
            codex_provider_key(path, provider)
                .filter(|key| !key.trim().is_empty())
                .map(|key| (env_key_for_provider(provider).to_string(), key))
        })
        .collect()
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

    let provider = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .and_then(|provider| normalize_codex_provider(provider).ok())
        .unwrap_or(VENUS_PROVIDER_ID);
    if provider == CODEX_WOA_PROVIDER_ID {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        let paths = AppPaths::from_root(parent);
        let settings = load_app_settings(&paths);
        ensure_woa_token_for_settings(&paths, &settings)?;
        let before = doc.to_string();
        let active_model = doc
            .get("model")
            .and_then(|item| item.as_str())
            .unwrap_or_else(|| default_model_for_provider(provider))
            .to_string();
        write_codex_woa_provider_table(&mut doc);
        doc["chatgpt_base_url"] = value(CODEX_WOA_BASE_URL);
        if doc.get("preferred_auth_method").is_none() {
            doc["preferred_auth_method"] = value(CODEX_AUTH_METHOD_API_KEY);
        }
        if doc.get("model_context_window").is_none() {
            doc["model_context_window"] = value(model_context_window(&active_model));
        }
        if doc.get("model_max_output_tokens").is_none() {
            doc["model_max_output_tokens"] = value(model_max_output_tokens(&active_model));
        }
        if doc.get("model_reasoning_effort").is_none() {
            doc["model_reasoning_effort"] = value(CODEX_REASONING_EFFORT_NONE);
        }
        let catalog_path_string = codex_model_catalog_path(&paths)
            .to_string_lossy()
            .to_string();
        if doc.get("model_catalog_json").and_then(|item| item.as_str())
            != Some(catalog_path_string.as_str())
        {
            doc["model_catalog_json"] = value(catalog_path_string);
        }
        let _ = write_codex_acp_model_catalog(&paths, provider);
        if doc.to_string() != before {
            std::fs::write(path, doc.to_string())
                .with_context(|| format!("failed to write Codex config {}", path.display()))?;
        }
        return Ok(());
    }
    if provider == BYOK_PROVIDER_ID || codex_is_byok_source(provider) {
        let mut changed = false;
        if provider != BYOK_PROVIDER_ID {
            doc["model_provider"] = value(BYOK_PROVIDER_ID);
            changed = true;
        }
        write_codex_byok_provider_table(&mut doc);
        let active_model = doc
            .get("model")
            .and_then(|item| item.as_str())
            .unwrap_or_else(|| default_model_for_provider(provider))
            .to_string();
        let active_model_slug = byok_model_slug(&active_model);
        if active_model_slug != active_model {
            doc["model"] = value(active_model_slug.clone());
            changed = true;
        }
        if !provider_field_eq(&doc, provider, "base_url", &venus_base_url()) {
            doc["model_providers"][provider]["base_url"] = value(venus_base_url());
            changed = true;
        }
        if doc.get("model_context_window").is_none() {
            doc["model_context_window"] = value(model_context_window(&active_model_slug));
            changed = true;
        }
        if doc.get("model_max_output_tokens").is_none() {
            doc["model_max_output_tokens"] = value(model_max_output_tokens(&active_model_slug));
            changed = true;
        }
        if let Some(parent) = path.parent() {
            let paths = AppPaths::from_root(parent);
            let _ = write_codex_acp_model_catalog(&paths, BYOK_PROVIDER_ID);
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
    if provider == CODEX_WOA_PROVIDER_ID {
        write_codex_woa_provider_table(doc);
        return;
    }
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

fn write_codex_woa_provider_table(doc: &mut DocumentMut) {
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
    providers.insert(CODEX_WOA_PROVIDER_ID, Item::Table(Table::new()));
    let provider_table = providers
        .get_mut(CODEX_WOA_PROVIDER_ID)
        .and_then(|item| item.as_table_mut())
        .expect("WOA provider should be a table");
    provider_table.insert("name", value(CODEX_WOA_PROVIDER_NAME));
    provider_table.insert("base_url", value(CODEX_WOA_BASE_URL));
    provider_table.insert("chatgpt_base_url", value(CODEX_WOA_BASE_URL));
    provider_table.insert("wire_api", value(VENUS_WIRE_API));
    provider_table.insert("requires_openai_auth", value(false));

    provider_table.insert("http_headers", Item::Table(Table::new()));
    let headers = provider_table
        .get_mut("http_headers")
        .and_then(|item| item.as_table_mut())
        .expect("http_headers should be a table");
    headers.insert("x-app-name", value("codex-internal"));
    headers.insert("x-request-platform", value("codex-internal"));
    headers.insert("x-scene-name", value("common_chat"));
    headers.insert("x-channel", value("codex-internal"));

    provider_table.insert("env_http_headers", Item::Table(Table::new()));
    let env_headers = provider_table
        .get_mut("env_http_headers")
        .and_then(|item| item.as_table_mut())
        .expect("env_http_headers should be a table");
    env_headers.insert("x-api-key", value(CODEX_WOA_API_KEY_ENV));
    env_headers.insert("x-knot-api-key", value(CODEX_WOA_KNOT_API_KEY_ENV));
    env_headers.insert("x-conversation-id", value(CODEX_WOA_CONVERSATION_ID_ENV));
    env_headers.insert("x-app-version", value(CODEX_WOA_APP_VERSION_ENV));
    env_headers.insert("x-git-repos", value(CODEX_WOA_GIT_REPOS_ENV));
    env_headers.insert("x-yolo-mode", value(CODEX_WOA_YOLO_MODE_ENV));
    env_headers.insert("x-anydev-mode", value(CODEX_WOA_ANYDEV_MODE_ENV));
    env_headers.insert("x-specify-model", value(CODEX_WOA_SPECIFY_MODEL_ENV));
    env_headers.insert("user-agent", value(CODEX_WOA_USER_AGENT_ENV));
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

fn venus_base_url() -> String {
    acp_core::codex_api_proxy_base_url()
}

fn normalize_codex_provider(provider: &str) -> Result<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        BYOK_PROVIDER_ID => Ok(BYOK_PROVIDER_ID),
        CODEX_WOA_PROVIDER_ID => Ok(CODEX_WOA_PROVIDER_ID),
        TIMIAI_PROVIDER_ID | "timi" | "timi-ai" | "timi_ai" => Ok(TIMIAI_PROVIDER_ID),
        VENUS_PROVIDER_ID => Ok(VENUS_PROVIDER_ID),
        DEEPSEEK_PROVIDER_ID => Ok(DEEPSEEK_PROVIDER_ID),
        KIMI_PROVIDER_ID | "kimi" | "kimi-code" => Ok(KIMI_PROVIDER_ID),
        MIMO_PROVIDER_ID | "mimo" | "xiaomi-mimo" => Ok(MIMO_PROVIDER_ID),
        other => anyhow::bail!("Unsupported Codex provider: {other}"),
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => model_slug_for_provider(VENUS_MODEL, VENUS_PROVIDER_ID),
        CODEX_WOA_PROVIDER_ID => model_slug_for_provider(CODEX_WOA_MODEL, CODEX_WOA_PROVIDER_ID),
        TIMIAI_PROVIDER_ID => model_slug_for_provider(TIMIAI_CODEX_MODEL, TIMIAI_PROVIDER_ID),
        DEEPSEEK_PROVIDER_ID => model_slug_for_provider(DEEPSEEK_MODEL, DEEPSEEK_PROVIDER_ID),
        KIMI_PROVIDER_ID => model_slug_for_provider(KIMI_MODEL, KIMI_PROVIDER_ID),
        MIMO_PROVIDER_ID => model_slug_for_provider(MIMO_MODEL, MIMO_PROVIDER_ID),
        _ => model_slug_for_provider(VENUS_MODEL, VENUS_PROVIDER_ID),
    }
}

fn provider_name(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => BYOK_PROVIDER_NAME,
        CODEX_WOA_PROVIDER_ID => CODEX_WOA_PROVIDER_NAME,
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_NAME,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_PROVIDER_NAME,
        KIMI_PROVIDER_ID => KIMI_PROVIDER_NAME,
        MIMO_PROVIDER_ID => MIMO_PROVIDER_NAME,
        _ => VENUS_PROVIDER_NAME,
    }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => "BYOK",
        CODEX_WOA_PROVIDER_ID => "WOA",
        TIMIAI_PROVIDER_ID => TIMIAI_PROVIDER_NAME,
        DEEPSEEK_PROVIDER_ID => "DeepSeek",
        KIMI_PROVIDER_ID => "Kimi Code",
        MIMO_PROVIDER_ID => MIMO_PROVIDER_NAME,
        _ => "Venus",
    }
}

fn wire_api_for_provider(_provider: &str) -> &'static str {
    VENUS_WIRE_API
}

fn env_key_for_provider(provider: &str) -> &'static str {
    match provider {
        BYOK_PROVIDER_ID => BYOK_API_KEY_ENV,
        CODEX_WOA_PROVIDER_ID => CODEX_WOA_API_KEY_ENV,
        TIMIAI_PROVIDER_ID => TIMIAI_API_KEY_ENV,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_API_KEY_ENV,
        KIMI_PROVIDER_ID => KIMI_API_KEY_ENV,
        MIMO_PROVIDER_ID => MIMO_API_KEY_ENV,
        _ => VENUS_API_KEY_ENV,
    }
}

fn base_url_for_provider(provider: &str) -> String {
    match provider {
        CODEX_WOA_PROVIDER_ID => CODEX_WOA_BASE_URL.to_string(),
        BYOK_PROVIDER_ID => venus_base_url(),
        TIMIAI_PROVIDER_ID => venus_base_url(),
        DEEPSEEK_PROVIDER_ID => venus_base_url(),
        _ => venus_base_url(),
    }
}

fn codex_model_catalog_path(paths: &AppPaths) -> PathBuf {
    paths.root().join("model_catalog.json")
}

fn write_codex_acp_model_catalog(paths: &AppPaths, provider: &str) -> Result<()> {
    let path = codex_model_catalog_path(paths);
    let catalog_models = catalog_models_for_provider_with_paths(paths, provider);
    let models = catalog_models
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

fn catalog_models_for_provider_with_paths(paths: &AppPaths, provider: &str) -> Vec<&'static str> {
    if provider != BYOK_PROVIDER_ID {
        return catalog_models_for_provider(provider).to_vec();
    }
    let config_path = codex_config_path(paths);
    let mut models = Vec::new();
    for provider in BYOK_SOURCE_PROVIDER_IDS {
        if codex_provider_key(&config_path, provider)
            .filter(|key| !key.trim().is_empty())
            .is_some()
        {
            models.extend(catalog_models_for_provider(provider));
        }
    }
    if models.is_empty() {
        models.push(VENUS_MODEL);
    }
    models
}

fn catalog_models_for_provider(provider: &str) -> &'static [&'static str] {
    match provider {
        CODEX_WOA_PROVIDER_ID => CODEX_WOA_CATALOG_MODELS,
        TIMIAI_PROVIDER_ID => TIMIAI_CATALOG_MODELS,
        DEEPSEEK_PROVIDER_ID => DEEPSEEK_CATALOG_MODELS,
        KIMI_PROVIDER_ID => KIMI_CATALOG_MODELS,
        MIMO_PROVIDER_ID => MIMO_CATALOG_MODELS,
        _ => VENUS_CATALOG_MODELS,
    }
}

fn codex_acp_model_catalog_entry(
    model: &str,
    provider: &str,
    priority: usize,
) -> serde_json::Value {
    let slug = if provider == BYOK_PROVIDER_ID {
        byok_model_slug(model)
    } else {
        model_slug_for_provider(model, provider).to_string()
    };
    let context_window = model_context_window(model);
    let max_output_tokens = model_max_output_tokens(model);
    let is_deepseek = provider == DEEPSEEK_PROVIDER_ID || model.contains("deepseek");
    let apply_patch_tool_type = apply_patch_tool_type_for_provider(provider);
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
        "apply_patch_tool_type": apply_patch_tool_type,
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

fn apply_patch_tool_type_for_provider(_provider: &str) -> &'static str {
    "function"
}

/// Resolve a display model name with the Venus slug mapping.
#[cfg(test)]
fn model_slug(display_name: &str) -> &str {
    model_slug_for_provider(display_name, VENUS_PROVIDER_ID)
}

fn model_slug_for_provider<'a>(display_name: &'a str, provider: &str) -> &'a str {
    match provider {
        VENUS_PROVIDER_ID => lookup_model_slug(display_name, VENUS_MODEL_SLUG_MAP),
        MIMO_PROVIDER_ID => lookup_model_slug(display_name, MIMO_MODEL_SLUG_MAP),
        _ => display_name,
    }
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
    if let Some(display_name) = legacy_deepseek_venus_slug_display_name(model) {
        return display_name.to_string();
    }
    let provider = byok_source_provider_for_model(model);
    model_slug_for_provider(model, provider).to_string()
}

fn byok_source_provider_for_model(model: &str) -> &'static str {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.contains("deepseek") {
        DEEPSEEK_PROVIDER_ID
    } else if normalized.contains("kimi") {
        KIMI_PROVIDER_ID
    } else if normalized.contains("mimo") {
        MIMO_PROVIDER_ID
    } else {
        VENUS_PROVIDER_ID
    }
}

fn legacy_deepseek_venus_slug_display_name(model: &str) -> Option<&'static str> {
    match model {
        "deepseek-v4-pro-external" => Some("deepseek-v4-pro"),
        "deepseek-v4-flash-external" => Some("deepseek-v4-flash"),
        _ => None,
    }
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
            (*candidate == model
                || model_slug_for_provider(candidate, VENUS_PROVIDER_ID) == model
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
    let settings = load_app_settings(paths);
    let binary = claude_agent_acp_detected_path(paths)
        .map(|path| shell_quote_path(&path))
        .unwrap_or_else(|| binary_name("claude-agent-acp"));
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    if selected_profile_id != CLAUDE_WOA_PROVIDER_ID {
        return binary;
    }
    let token_path = claude_woa_token_path(paths);
    format!(
        "{} --woa --woa-channel {} --woa-token-path {}",
        binary,
        crate::claude_woa::channel_arg(settings.claude_woa.channel),
        shell_words::quote(&token_path.to_string_lossy())
    )
}

fn claude_agent_acp_env(paths: &AppPaths) -> Vec<(String, String)> {
    let settings = load_app_settings(paths);
    let selected_profile_id = selected_claude_provider_profile_id(&settings);
    let selected_profile = profile_definition(AgentProviderFamily::Claude, &selected_profile_id);
    let available_models = if selected_profile_id == CLAUDE_WOA_PROVIDER_ID {
        sanitize_claude_available_models(settings.claude_woa.available_models)
    } else if selected_profile_id == BYOK_PROVIDER_ID {
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

    if selected_profile_id == CLAUDE_WOA_PROVIDER_ID {
        return env;
    }

    if selected_profile_id == BYOK_PROVIDER_ID {
        let configured_sources = configured_claude_byok_source_keys(paths);
        for (provider, key) in &configured_sources {
            acp_core::ensure_codex_api_proxy(provider, key);
        }
        if let Some(default_model) = available_models.first() {
            env.push(("ANTHROPIC_MODEL".to_string(), default_model.clone()));
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
    if profile.id == TIMIAI_PROVIDER_ID {
        if let Some(secret) = provider_secret(paths, AgentProviderFamily::Claude, profile.id) {
            acp_core::ensure_codex_api_proxy(TIMIAI_PROVIDER_ID, &secret);
            env.push((
                "ANTHROPIC_API_KEY".to_string(),
                TIMIAI_PROVIDER_ID.to_string(),
            ));
            env.push((
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                TIMIAI_PROVIDER_ID.to_string(),
            ));
            env.push(("AUTH_TOKEN".to_string(), TIMIAI_PROVIDER_ID.to_string()));
        }
        env.push((
            "ANTHROPIC_BASE_URL".to_string(),
            claude_provider_proxy_base_url(),
        ));
        if let Some(model) = profile.default_model {
            env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
        }
        env.push((
            "CLAUDE_PROVIDER_PROXY_KIND".to_string(),
            claude_proxy_kind_env(profile.proxy_kind).to_string(),
        ));
        return env;
    }
    if let Some(secret) = provider_secret(paths, AgentProviderFamily::Claude, profile.id) {
        env.push(("ANTHROPIC_API_KEY".to_string(), secret.clone()));
        env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), secret.clone()));
        env.push(("AUTH_TOKEN".to_string(), secret));
    }
    if let Some(base_url) = profile.base_url {
        env.push(("ANTHROPIC_BASE_URL".to_string(), base_url.to_string()));
    }
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
    for profile in CLAUDE_PROVIDER_PROFILES.iter().filter(|profile| {
        profile.id != CLAUDE_WOA_PROVIDER_ID
            && profile.id != VENUS_PROVIDER_ID
            && profile.id != TIMIAI_PROVIDER_ID
            && profile.id != BYOK_PROVIDER_ID
            && profile.requires_credential
    }) {
        if byok_source_secret(paths, AgentProviderFamily::Claude, profile.id).is_none() {
            continue;
        }
        for model in profile.models {
            let model = (*model).to_string();
            if !models.contains(&model) {
                models.push(model);
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

fn claude_proxy_kind_env(proxy_kind: AgentProviderProxyKind) -> &'static str {
    match proxy_kind {
        AgentProviderProxyKind::ClaudeNative => "claude_native",
        AgentProviderProxyKind::CompletionToClaude => "completion_to_claude",
        AgentProviderProxyKind::ClaudeWoa => "claude_woa",
        _ => "unsupported",
    }
}

fn sanitize_claude_available_models(models: Vec<String>) -> Vec<String> {
    let mut sanitized = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || sanitized.iter().any(|existing| existing == model) {
            continue;
        }
        sanitized.push(model.to_string());
    }
    sanitized
}

fn managed_claude_woa_settings(
    paths: &AppPaths,
    settings: &ClaudeWoaSettings,
) -> ClaudeWoaSettings {
    ClaudeWoaSettings {
        channel: settings.channel,
        token_path: Some(claude_woa_token_path(paths)),
        available_models: settings.available_models.clone(),
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
    use std::sync::Mutex;
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

    #[test]
    fn ioa_environment_detects_company_export_ip() {
        let status = ioa_environment_status_from_value(
            &serde_json::json!({
                "data": {
                    "isCompanyExportIP": true,
                    "isInternal": false
                }
            }),
            true,
        );

        assert!(status.company_environment);
        assert_eq!(status.recommended_setup, InitialSetupRecommendation::Woa);
        assert!(status.detected);
    }

    #[test]
    fn ioa_environment_detects_login_method_ioa() {
        let status = ioa_environment_status_from_value(
            &serde_json::json!({
                "data": {
                    "isCompanyExportIP": "false",
                    "isInternal": false,
                    "loginMethod": "ioa"
                }
            }),
            true,
        );

        assert!(status.company_environment);
        assert_eq!(status.recommended_setup, InitialSetupRecommendation::Woa);
        assert!(status.detected);
    }

    #[test]
    fn ioa_environment_falls_back_to_codex_byok_for_external_network() {
        let status = ioa_environment_status_from_value(
            &serde_json::json!({
                "data": {
                    "isCompanyExportIP": false,
                    "isInternal": false
                }
            }),
            true,
        );

        assert!(!status.company_environment);
        assert_eq!(
            status.recommended_setup,
            InitialSetupRecommendation::CodexByok
        );
        assert!(status.detected);
    }

    #[test]
    fn ioa_environment_detection_failure_falls_back_to_codex_byok() {
        let status = external_ioa_environment_status(Some("request timed out".to_string()));

        assert!(!status.company_environment);
        assert!(!status.detected);
        assert_eq!(
            status.recommended_setup,
            InitialSetupRecommendation::CodexByok
        );
        assert_eq!(status.message.as_deref(), Some("request timed out"));
    }

    #[tokio::test]
    async fn ioa_environment_retries_untrusted_detector_responses() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                server_request_count.fetch_add(1, Ordering::SeqCst);
                let (status, body) = match index {
                    0 => ("502 Bad Gateway", "blocked by endpoint protection"),
                    1 => ("200 OK", "<html>blocked</html>"),
                    _ => (
                        "200 OK",
                        r#"{"data":{"isCompanyExportIP":true,"isInternal":false}}"#,
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let status =
            detect_ioa_environment_with_client(&client, &url, Instant::now(), 3, Duration::ZERO)
                .await;

        assert!(
            status.detected,
            "status={status:?} count={}",
            request_count.load(Ordering::SeqCst)
        );
        assert!(status.company_environment);
        assert_eq!(status.recommended_setup, InitialSetupRecommendation::Woa);
        assert_eq!(request_count.load(Ordering::SeqCst), 3);
        server.join().unwrap();
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
        assert_eq!(
            settings.claude_woa.channel,
            workspace_model::ClaudeWoaChannel::Default
        );
        assert!(settings.claude_woa.token_path.is_none());
        assert!(settings.claude_woa.available_models.is_empty());
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
            selected_codex_provider_profile_id: Some(VENUS_PROVIDER_ID.to_string()),
            selected_claude_provider_profile_id: Some(CLAUDE_WOA_PROVIDER_ID.to_string()),
            claude_woa: ClaudeWoaSettings {
                available_models: default_claude_woa_available_models(),
                ..ClaudeWoaSettings::default()
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
            claude_woa: ClaudeWoaSettings::default(),
        };

        save_app_settings(&paths, &settings).unwrap();
        let loaded = load_app_settings(&paths);

        assert_eq!(loaded.selected_agent, AgentCliId::Codebuddy);
        assert_eq!(
            loaded.selected_codex_provider_profile_id.as_deref(),
            Some(VENUS_PROVIDER_ID)
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
        assert_eq!(snapshot.claude_woa.selected_profile_id, BYOK_PROVIDER_ID);
        assert_eq!(snapshot.codex_acp.profiles.len(), 8);
        assert_eq!(snapshot.claude_woa.profiles.len(), 7);
        assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
            profile.id == TIMIAI_PROVIDER_ID
                && profile.label == TIMIAI_PROVIDER_NAME
                && profile.proxy_kind == AgentProviderProxyKind::Responses
                && profile.base_url.as_deref() == Some(TIMIAI_BASE_URL)
        }));
        assert!(snapshot.claude_woa.profiles.iter().any(|profile| {
            profile.id == TIMIAI_PROVIDER_ID
                && profile.label == TIMIAI_PROVIDER_NAME
                && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
                && profile.base_url.as_deref() == Some(TIMIAI_BASE_URL)
        }));
        assert!(snapshot.codex_acp.profiles.iter().any(|profile| {
            profile.id == CODEX_WOA_PROVIDER_ID
                && profile.label == CODEX_WOA_PROVIDER_NAME
                && profile.proxy_kind == AgentProviderProxyKind::Responses
                && profile.base_url.as_deref() == Some(CODEX_WOA_BASE_URL)
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
        assert!(snapshot.claude_woa.profiles.iter().any(|profile| {
            profile.id == KIMI_PROVIDER_ID
                && profile.proxy_kind == AgentProviderProxyKind::ClaudeNative
                && profile.base_url.as_deref() == Some(KIMI_CODE_ANTHROPIC_BASE_URL)
        }));
        assert!(snapshot.claude_woa.profiles.iter().any(|profile| {
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
    fn default_agent_for_new_work_prefers_configured_codex_woa_over_unconfigured_claude() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let binary_path = codex_acp_binary_path(&paths);
        std::fs::create_dir_all(binary_path.parent().unwrap()).unwrap();
        std::fs::write(&binary_path, "fake").unwrap();

        select_agent_provider_profile(&paths, AgentProviderFamily::Codex, CODEX_WOA_PROVIDER_ID)
            .unwrap();
        crate::claude_woa::save_token(
            &claude_woa_token_path(&paths),
            &crate::claude_woa::WoaToken {
                access_token: "access-secret".into(),
                refresh_token: Some("refresh-secret".into()),
                expires_at: crate::claude_woa::now_ms() + 600_000,
            },
        )
        .unwrap();

        assert_eq!(default_agent_for_new_work(&paths), AgentCliId::CodexAcp);
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
    fn claude_woa_config_persists_channel_and_uses_managed_token_path() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let token_path = dir.path().join("woa-token.json");

        let snapshot = save_claude_woa_config(
            &paths,
            workspace_model::ClaudeWoaConfigInput {
                channel: workspace_model::ClaudeWoaChannel::Offline,
                token_path: Some(token_path.display().to_string()),
                available_models: vec![
                    " claude-sonnet-4-6[1m] ".into(),
                    "claude-sonnet-4-6[1m]".into(),
                    String::new(),
                    "claude-opus-4-7[1m]".into(),
                ],
            },
        )
        .unwrap();

        assert_eq!(
            snapshot.settings.claude_woa.channel,
            workspace_model::ClaudeWoaChannel::Offline
        );
        assert_eq!(snapshot.settings.claude_woa.token_path, None);
        assert_eq!(
            snapshot.settings.claude_woa.available_models,
            vec!["claude-sonnet-4-6[1m]", "claude-opus-4-7[1m]"]
        );
        assert_eq!(
            snapshot.claude_woa.token_path,
            claude_woa_token_path(&paths)
        );
        assert_ne!(snapshot.claude_woa.token_path, token_path);
    }

    #[test]
    fn selecting_claude_woa_fills_default_models() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let snapshot = select_agent_provider_profile(
            &paths,
            AgentProviderFamily::Claude,
            CLAUDE_WOA_PROVIDER_ID,
        )
        .unwrap();

        assert_eq!(
            snapshot.settings.claude_woa.available_models,
            vec!["claude-opus-4-7[1m]", "claude-opus-4-6[1m]"]
        );
    }

    #[test]
    fn claude_woa_snapshot_does_not_expose_token_secrets() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let token_path = claude_woa_token_path(&paths);
        save_claude_woa_config(
            &paths,
            workspace_model::ClaudeWoaConfigInput {
                channel: workspace_model::ClaudeWoaChannel::Default,
                token_path: Some(dir.path().join("ignored.json").display().to_string()),
                available_models: Vec::new(),
            },
        )
        .unwrap();
        crate::claude_woa::save_token(
            &token_path,
            &crate::claude_woa::WoaToken {
                access_token: "access-secret-value".into(),
                refresh_token: Some("refresh-secret-value".into()),
                expires_at: crate::claude_woa::now_ms() + 600_000,
            },
        )
        .unwrap();

        let serialized = serde_json::to_string(&settings_snapshot(&paths)).unwrap();

        assert!(!serialized.contains("access-secret-value"));
        assert!(!serialized.contains("refresh-secret-value"));
        assert!(serialized.contains("acce...alue"));
    }

    #[test]
    fn claude_agent_acp_command_uses_woa_args() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        let token_path = claude_woa_token_path(&paths);
        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, CLAUDE_WOA_PROVIDER_ID)
            .unwrap();
        save_claude_woa_config(
            &paths,
            workspace_model::ClaudeWoaConfigInput {
                channel: workspace_model::ClaudeWoaChannel::Offline,
                token_path: Some(dir.path().join("ignored.json").display().to_string()),
                available_models: Vec::new(),
            },
        )
        .unwrap();

        let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();

        assert!(command.contains("claude-agent-acp"));
        assert!(command.contains("--woa"));
        assert!(command.contains("--woa-channel offline"));
        assert!(command.contains("--woa-token-path"));
        assert!(command.contains(&token_path.to_string_lossy().to_string()));
    }

    #[test]
    fn claude_agent_acp_env_includes_custom_model_list() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, CLAUDE_WOA_PROVIDER_ID)
            .unwrap();
        save_claude_woa_config(
            &paths,
            workspace_model::ClaudeWoaConfigInput {
                channel: workspace_model::ClaudeWoaChannel::Default,
                token_path: None,
                available_models: vec![
                    "claude-sonnet-4-6[1m]".into(),
                    "claude-opus-4-7[1m]".into(),
                ],
            },
        )
        .unwrap();

        let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);

        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CLAUDE_MODEL_CONFIG");
        assert!(env[0].1.contains("claude-sonnet-4-6[1m]"));
        assert!(env[0].1.contains("claude-opus-4-7[1m]"));
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
                selected_codex_provider_profile_id: Some(VENUS_PROVIDER_ID.to_string()),
                selected_claude_provider_profile_id: Some(CLAUDE_WOA_PROVIDER_ID.to_string()),
                claude_woa: ClaudeWoaSettings::default(),
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
        assert!(
            doc["model_providers"][VENUS_PROVIDER_ID]["base_url"]
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
    fn selecting_codex_woa_creates_direct_gateway_config() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        let snapshot = select_agent_provider_profile(
            &paths,
            AgentProviderFamily::Codex,
            CODEX_WOA_PROVIDER_ID,
        )
        .unwrap();

        assert_eq!(
            snapshot.codex_acp.selected_profile_id,
            CODEX_WOA_PROVIDER_ID
        );
        assert!(snapshot.codex_acp.profiles.iter().any(|profile| profile.id
            == CODEX_WOA_PROVIDER_ID
            && profile.label == CODEX_WOA_PROVIDER_NAME
            && profile.selected
            && profile.configured));

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        assert!(content.contains("[model_providers.woa]"));
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(CODEX_WOA_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(CODEX_WOA_PROVIDER_ID));
        assert_eq!(
            doc["preferred_auth_method"].as_str(),
            Some(CODEX_AUTH_METHOD_API_KEY)
        );
        assert_eq!(
            doc["model_context_window"].as_integer(),
            Some(model_context_window(CODEX_WOA_MODEL))
        );
        assert_eq!(
            doc["model_max_output_tokens"].as_integer(),
            Some(model_max_output_tokens(CODEX_WOA_MODEL))
        );
        assert_eq!(doc["chatgpt_base_url"].as_str(), Some(CODEX_WOA_BASE_URL));
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["base_url"].as_str(),
            Some(CODEX_WOA_BASE_URL)
        );
        assert!(
            !doc["model_providers"][CODEX_WOA_PROVIDER_ID]["base_url"]
                .as_str()
                .unwrap_or_default()
                .contains("127.0.0.1")
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["chatgpt_base_url"].as_str(),
            Some(CODEX_WOA_BASE_URL)
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["wire_api"].as_str(),
            Some(VENUS_WIRE_API)
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["http_headers"]["x-request-platform"]
                .as_str(),
            Some("codex-internal")
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["env_http_headers"]["x-api-key"].as_str(),
            Some(CODEX_WOA_API_KEY_ENV)
        );
        assert!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["env_http_headers"]
                .get("oauth-token")
                .is_none()
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["env_http_headers"]["x-knot-api-key"]
                .as_str(),
            Some(CODEX_WOA_KNOT_API_KEY_ENV)
        );
        assert_eq!(
            doc["model_providers"][CODEX_WOA_PROVIDER_ID]["env_http_headers"]["x-git-repos"]
                .as_str(),
            Some(CODEX_WOA_GIT_REPOS_ENV)
        );
        let woa_provider_table = doc["model_providers"][CODEX_WOA_PROVIDER_ID]
            .as_table()
            .unwrap();
        assert!(woa_provider_table.get("api_key").is_none());
        assert!(woa_provider_table.get("env_key").is_none());

        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let models = catalog["models"].as_array().unwrap();
        let slugs = models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            slugs,
            CODEX_WOA_CATALOG_MODELS
                .iter()
                .map(|model| model_slug_for_provider(model, CODEX_WOA_PROVIDER_ID))
                .collect::<Vec<_>>()
        );
        assert!(slugs.contains(&"gpt-5.4"));
        assert!(!slugs.contains(&"gpt-5.5-codex-max"));
        assert!(!slugs.contains(&"gpt-5.5-codex-mini"));
        assert!(!slugs.contains(&"gpt-5.5"));
        assert!(models.iter().all(|model| {
            model["input_modalities"].as_array().unwrap()
                == &vec![
                    serde_json::Value::String("text".to_string()),
                    serde_json::Value::String("image".to_string()),
                ]
        }));
        assert!(
            models
                .iter()
                .all(|model| { model["apply_patch_tool_type"].as_str() == Some("function") })
        );

        crate::claude_woa::save_token(
            &claude_woa_token_path(&paths),
            &crate::claude_woa::WoaToken {
                access_token: "access-secret".into(),
                refresh_token: Some("refresh-secret".into()),
                expires_at: crate::claude_woa::now_ms() + 600_000,
            },
        )
        .unwrap();
        let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);
        assert!(env.contains(&(
            CODEX_WOA_API_KEY_ENV.to_string(),
            "access-secret".to_string()
        )));
        assert!(env.contains(&(
            CODEX_WOA_APP_VERSION_ENV.to_string(),
            CODEX_WOA_APP_VERSION.to_string()
        )));
        assert!(env.contains(&(
            CODEX_WOA_USER_AGENT_ENV.to_string(),
            format!("Codex-Internal/{CODEX_WOA_APP_VERSION}")
        )));
        assert!(env.iter().any(|(name, value)| {
            name == CODEX_WOA_CONVERSATION_ID_ENV && uuid::Uuid::parse_str(value).is_ok()
        }));
    }

    #[test]
    fn codex_acp_deepseek_config_creates_provider_config() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        write_codex_acp_provider_config(&paths, DEEPSEEK_PROVIDER_ID, "deepseek-secret").unwrap();

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        assert!(content.contains("[model_providers.deepseek]"));
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model"].as_str(),
            Some(byok_model_slug(DEEPSEEK_MODEL).as_str())
        );
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
        assert!(!status.venus_key_configured);
        assert!(status.deepseek_key_configured);
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

        for (provider, env_key, model) in [
            (
                VENUS_PROVIDER_ID,
                VENUS_API_KEY_ENV,
                model_slug_for_provider(VENUS_MODEL, VENUS_PROVIDER_ID),
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
            if provider == VENUS_PROVIDER_ID {
                assert_eq!(snapshot.codex_acp.selected_profile_id, provider);
            } else {
                assert_ne!(snapshot.codex_acp.selected_profile_id, provider);
                let snapshot =
                    select_agent_provider_profile(&paths, AgentProviderFamily::Codex, provider)
                        .unwrap();
                assert_eq!(snapshot.codex_acp.selected_profile_id, BYOK_PROVIDER_ID);
            }

            let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
            let doc = content.parse::<DocumentMut>().unwrap();
            if provider == VENUS_PROVIDER_ID {
                assert_eq!(doc["model"].as_str(), Some(model));
            } else {
                let configured_models = configured_codex_byok_models(&paths)
                    .into_iter()
                    .map(|model| byok_model_slug(&model))
                    .collect::<Vec<_>>();
                assert!(configured_models.contains(&doc["model"].as_str().unwrap().to_string()));
            }
            let expected_channel_provider = codex_channel_provider_for_source(provider);
            assert_eq!(
                doc["model_provider"].as_str(),
                Some(expected_channel_provider)
            );
            if expected_channel_provider == BYOK_PROVIDER_ID {
                assert_eq!(
                    doc["model_providers"][BYOK_PROVIDER_ID]["base_url"].as_str(),
                    Some(venus_base_url().as_str())
                );
            }
            assert_eq!(
                doc["model_providers"][provider]["env_key"].as_str(),
                Some(env_key)
            );
            assert_eq!(
                doc["model_providers"][provider]["wire_api"].as_str(),
                Some(VENUS_WIRE_API)
            );
            assert_eq!(
                doc["model_providers"][provider]["api_key"].as_str(),
                Some(secret.as_str())
            );
            if provider == KIMI_PROVIDER_ID && doc["model"].as_str() == Some(KIMI_MODEL) {
                assert_eq!(doc["model"].as_str(), Some(KIMI_MODEL));
                assert_eq!(
                    doc["model_context_window"].as_integer(),
                    Some(KIMI_MODEL_CONTEXT_WINDOW)
                );
                assert_eq!(
                    doc["model_max_output_tokens"].as_integer(),
                    Some(KIMI_MODEL_MAX_OUTPUT_TOKENS)
                );
            }
            let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
            let env = agent_env_for_command(&command, &paths);
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
                .claude_woa
                .profiles
                .iter()
                .any(|profile| profile.id == TIMIAI_PROVIDER_ID && profile.configured)
        );

        let snapshot =
            select_agent_provider_profile(&paths, AgentProviderFamily::Codex, TIMIAI_PROVIDER_ID)
                .unwrap();
        assert_eq!(snapshot.codex_acp.selected_profile_id, TIMIAI_PROVIDER_ID);

        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(TIMIAI_CODEX_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(TIMIAI_PROVIDER_ID));
        assert_eq!(
            doc["model_providers"][TIMIAI_PROVIDER_ID]["env_key"].as_str(),
            Some(TIMIAI_API_KEY_ENV)
        );
        assert_eq!(
            doc["model_providers"][TIMIAI_PROVIDER_ID]["api_key"].as_str(),
            Some("timiai-secret")
        );
        assert!(
            doc["model_providers"][TIMIAI_PROVIDER_ID]["base_url"]
                .as_str()
                .unwrap_or_default()
                .starts_with("http://127.0.0.1:")
        );

        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let slugs = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(slugs.contains(&TIMIAI_CODEX_MODEL));
        assert!(slugs.contains(&TIMIAI_CLAUDE_MODEL));
        assert!(slugs.contains(&DEEPSEEK_MODEL));
        assert!(!slugs.contains(&model_slug(DEEPSEEK_MODEL)));

        let command = command_for_agent_with_paths(AgentCliId::CodexAcp, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);
        assert!(env.contains(&(TIMIAI_API_KEY_ENV.to_string(), "timiai-secret".to_string())));
    }

    #[test]
    fn claude_timiai_channel_uses_shared_key_and_local_proxy() {
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

        assert_eq!(snapshot.claude_woa.selected_profile_id, TIMIAI_PROVIDER_ID);
        assert!(snapshot.claude_woa.profiles.iter().any(|profile| {
            profile.id == TIMIAI_PROVIDER_ID
                && profile.configured
                && profile.models.contains(&TIMIAI_CODEX_MODEL.to_string())
                && profile.models.contains(&TIMIAI_CLAUDE_MODEL.to_string())
        }));

        let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
        assert!(!command.contains("--woa"));
        ensure_agent_ready_for_command(&command, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);

        assert!(env.contains(&(
            "ANTHROPIC_API_KEY".to_string(),
            TIMIAI_PROVIDER_ID.to_string()
        )));
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
        assert!(model_config.get("modelOverrides").is_none());
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
                selected_claude_provider_profile_id: Some(CLAUDE_WOA_PROVIDER_ID.to_string()),
                claude_woa: ClaudeWoaSettings::default(),
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

        assert!(env.contains(&(BYOK_API_KEY_ENV.to_string(), "byok".to_string())));
        assert!(env.contains(&(
            DEEPSEEK_API_KEY_ENV.to_string(),
            "deepseek-secret".to_string()
        )));
        assert!(env.contains(&(KIMI_API_KEY_ENV.to_string(), "kimi-secret".to_string())));
        assert!(env.contains(&(MIMO_API_KEY_ENV.to_string(), "mimo-secret".to_string())));
        let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
        let doc = content.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some(KIMI_MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some(BYOK_PROVIDER_ID));

        let catalog = std::fs::read_to_string(codex_model_catalog_path(&paths)).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let models = catalog["models"].as_array().unwrap();
        let slugs = models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(slugs.contains(&byok_model_slug(DEEPSEEK_MODEL).as_str()));
        assert!(!slugs.contains(&model_slug(DEEPSEEK_MODEL)));
        assert!(slugs.contains(&byok_model_slug(KIMI_MODEL).as_str()));
        assert!(slugs.contains(&byok_model_slug(MIMO_MODEL).as_str()));
        assert!(models.iter().any(|model| {
            model["display_name"].as_str() == Some(MIMO_MODEL)
                && model["slug"].as_str() == Some("mimo-v2.5-pro")
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
            model_slug_for_provider(DEEPSEEK_MODEL, VENUS_PROVIDER_ID),
            "deepseek-v4-pro-external"
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
            model_slug_for_provider("MiMo-V2.5-Pro", VENUS_PROVIDER_ID),
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
                .claude_woa
                .profiles
                .iter()
                .any(|profile| profile.id == DEEPSEEK_PROVIDER_ID && profile.configured)
        );
        assert!(!serialized.contains("deepseek-secret"));
    }

    #[test]
    fn claude_woa_channel_does_not_include_byok_models() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        select_agent_provider_profile(&paths, AgentProviderFamily::Claude, CLAUDE_WOA_PROVIDER_ID)
            .unwrap();
        let woa_command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
        assert!(woa_command.contains("--woa"));

        for provider in [
            VENUS_PROVIDER_ID,
            DEEPSEEK_PROVIDER_ID,
            KIMI_PROVIDER_ID,
            MIMO_PROVIDER_ID,
        ] {
            let secret = format!("{provider}-secret");
            save_agent_provider_secret(&paths, AgentProviderFamily::Claude, provider, &secret)
                .unwrap();
        }

        let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
        assert!(command.contains("claude-agent-acp"));
        assert!(command.contains("--woa"));
        assert!(!command.contains("secret"));

        let env = agent_env_for_command(&command, &paths);
        assert!(!env.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY"));
        assert!(
            !env.iter()
                .any(|(name, _)| name == "CLAUDE_PROVIDER_PROXY_KIND")
        );
        let model_config = env
            .iter()
            .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
            .map(|(_, value)| value)
            .expect("WOA should use its default model list");
        assert!(model_config.contains("claude-opus-4-7[1m]"));
        assert!(model_config.contains("claude-opus-4-6[1m]"));
        assert!(!model_config.contains(DEEPSEEK_MODEL));
        assert!(!model_config.contains(KIMI_MODEL));
        assert!(!model_config.contains(MIMO_MODEL));
    }

    #[test]
    fn claude_byok_channel_uses_shared_model_pool_and_local_proxy() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));

        for provider in [DEEPSEEK_PROVIDER_ID, KIMI_PROVIDER_ID, MIMO_PROVIDER_ID] {
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
        assert_eq!(snapshot.claude_woa.selected_profile_id, BYOK_PROVIDER_ID);
        assert!(snapshot.claude_woa.profiles.iter().any(|profile| {
            profile.id == BYOK_PROVIDER_ID
                && profile.configured
                && profile.models.contains(&DEEPSEEK_MODEL.to_string())
                && profile.models.contains(&KIMI_MODEL.to_string())
                && profile.models.contains(&MIMO_MODEL.to_string())
        }));

        let command = command_for_agent_with_paths(AgentCliId::ClaudeAgentAcp, &paths).unwrap();
        assert!(!command.contains("--woa"));
        ensure_agent_ready_for_command(&command, &paths).unwrap();
        let env = agent_env_for_command(&command, &paths);

        assert!(env.contains(&("ANTHROPIC_API_KEY".to_string(), "byok".to_string())));
        assert!(
            env.iter().any(|(name, value)| name == "ANTHROPIC_BASE_URL"
                && value.starts_with("http://127.0.0.1:"))
        );
        assert!(
            env.iter()
                .any(|(name, value)| name == "ANTHROPIC_MODEL" && value == DEEPSEEK_MODEL)
        );
        let (_, model_config) = env
            .iter()
            .find(|(name, _)| name == "CLAUDE_MODEL_CONFIG")
            .unwrap();
        let model_config: serde_json::Value = serde_json::from_str(model_config).unwrap();
        let available_models = model_config["availableModels"].as_array().unwrap();
        assert!(available_models.contains(&serde_json::Value::String(DEEPSEEK_MODEL.to_string())));
        assert!(available_models.contains(&serde_json::Value::String(KIMI_MODEL.to_string())));
        assert!(available_models.contains(&serde_json::Value::String(MIMO_MODEL.to_string())));
        assert_eq!(
            model_config["modelOverrides"][MIMO_MODEL].as_str(),
            Some(model_slug_for_provider(MIMO_MODEL, MIMO_PROVIDER_ID))
        );
        assert_eq!(model_config["preserveDefaultModel"].as_bool(), Some(false));
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
            vec![
                (BYOK_API_KEY_ENV.to_string(), "byok".to_string()),
                (
                    DEEPSEEK_API_KEY_ENV.to_string(),
                    "deepseek-secret".to_string()
                )
            ]
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
                selected_codex_provider_profile_id: Some(VENUS_PROVIDER_ID.to_string()),
                selected_claude_provider_profile_id: Some(CLAUDE_WOA_PROVIDER_ID.to_string()),
                claude_woa: ClaudeWoaSettings::default(),
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
