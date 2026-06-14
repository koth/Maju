use super::{load_app_settings, save_app_settings};
use crate::AppPaths;
use anyhow::Result;
use workspace_model::{AppSettings, LspServerConfigInput, LspServerSettings};

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
