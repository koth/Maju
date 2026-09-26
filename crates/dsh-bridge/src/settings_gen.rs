//! Generate/merge the dsh `settings.yaml` document from Kodex's BYOK provider
//! catalog before spawning `dsh web`.
//!
//! Kodex owns the `llm-pi-ai` settings section (the LLM provider routes) plus
//! the conditional `web-search-deepseek` / `llm-deepseek` sections, and
//! rewrites them on each bring-up from its current provider catalog. Other
//! sections (`ui-onboarding`, anything the user hand-edited under other
//! namespaces) are preserved by a YAML round-trip that replaces only those
//! keys.
//!
//! The default model and default agent preset travel in the `--patch` overlay
//! instead (`render_harness_patch`): dsh 0.1.7 moved both out of the settings
//! document into cordis plugin config (`agent-default-model` and
//! `agent-preset-registry` rows), so a `settings.yaml` section of those names
//! is no longer read by the harness.
//!
//! See `design-dsh-settings.md` for the full rationale.

use anyhow::{Context, anyhow};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use url::Url;

/// The env-var name Kodex injects for a provider's API key. dsh reads it via
/// the `apiKeyEnv` credential ref in `settings.yaml`.
pub fn key_env_for_provider(provider_id: &str) -> String {
    format!(
        "KODEX_DSH_{}_KEY",
        provider_id.to_ascii_uppercase().replace('-', "_")
    )
}

/// One LLM provider route for dsh's `llm-pi-ai.providers` dict.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DshProviderRoute {
    /// Route key (also the dsh provider id). Kodex provider id, e.g.
    /// `deepseek`, `kimi`, `mimo`, `commandcode`, `timiai`, custom ids.
    pub id: String,
    /// `apiKeyEnv` — env var name Kodex injects at spawn. Never the secret.
    pub api_key_env: String,
    /// Wire protocol: `openai-completions` | `openai-responses` |
    /// `anthropic-messages`.
    pub api: String,
    /// Upstream provider endpoint.
    pub base_url: String,
    /// Models this route serves.
    pub models: Vec<DshModelEntry>,
    /// Optional display name.
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DshModelEntry {
    pub id: String,
    pub name: String,
    pub context_window: i64,
    /// Declared request modalities (`["text"]` or `["text", "image"]`).
    ///
    /// dsh resolves a model's image support from its own catalog and defaults
    /// an undeclared model to text-only, which makes it reject every image with
    /// `attachment-error`. Declaring the modality here is the only channel
    /// Kodex has into that decision, so the harness and Kodex must agree on it
    /// (`image_capability::harness_model_input`).
    pub input: Vec<String>,
    // Deliberately no `maxTokens`: dsh's llm-pi-ai treats a configured model
    // `maxTokens` as a per-request *default* (adapterDefaults.maxTokens), on
    // top of the model capability pi-ai already passes, and the upstream
    // (litellm) then fails with a duplicate `max_tokens` argument. The
    // capability value is a model attribute, not a per-route deployment cap,
    // so it stays out of the generated settings entirely.
}

/// The default model selection for new dsh sessions.
///
/// Carried to the harness through the `agent-default-model` row of the patch
/// overlay (dsh ≥ 0.1.7); the settings.yaml section of the same name is no
/// longer read.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DshDefaultModel {
    pub provider: String,
    pub model: String,
}

/// The full settings document Kodex writes/merges. The `llm-pi-ai` section is
/// owned by Kodex and replaced wholesale on each bring-up; `web-search-deepseek`
/// is written when `web_search_api_key_env` is set and removed when it is
/// `None`, so a revoked key never leaves a dangling credential reference
/// behind. `default_model` / `default_preset` do NOT land in this document —
/// dsh ≥ 0.1.7 reads them from the patch overlay's plugin rows
/// (`render_harness_patch`); they ride this struct only so the bring-up can
/// forward them there.
#[derive(Debug, Clone, Default)]
pub struct DshSettingsConfig {
    pub providers: Vec<DshProviderRoute>,
    pub default_model: DshDefaultModel,
    /// Default agent preset for new sessions. Forwarded to the patch overlay
    /// (`agent-preset-registry` row); not written to settings.yaml. `None`
    /// leaves the harness's shipped default (`standard`) untouched.
    pub default_preset: Option<String>,
    /// Credential reference for dsh's `web-search-deepseek` plugin. Set when
    /// the DeepSeek BYOK provider is configured: Kodex injects the secret into
    /// the spawned process under `key_env_for_provider("deepseek")`, and this
    /// section points the search plugin at that same env var instead of its
    /// `DEEPSEEK_API_KEY` default.
    pub web_search_api_key_env: Option<String>,
    /// Whether the DeepSeek BYOK provider is configured. Drives the
    /// `llm-deepseek` section: with a key, dsh's native `deepseek-official`
    /// adapter must advertise an empty model catalog (`models: []`), or it
    /// resolves the same injected key and lists a second DeepSeek group next
    /// to the `llm-pi-ai` route (the duplicate picker entries go straight to
    /// api.deepseek.com, bypassing Kodex's codex_api_proxy). Catalog
    /// membership is advisory in dsh, so an empty list hides the group without
    /// breaking sessions already routed to `deepseek-official`. Without a key
    /// the section is removed entirely, restoring the adapter's own
    /// `DEEPSEEK_API_KEY` behavior.
    pub deepseek_byok_configured: bool,
}

/// Read the existing `settings.yaml` (if any) as a JSON value, so we can
/// round-trip it while replacing only the two Kodex-owned sections. Returns
/// an empty mapping when the file does not exist.
fn read_existing(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read dsh settings {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let yaml: Value = serde_yaml::from_str(&text).context("failed to parse dsh settings.yaml")?;
    Ok(yaml)
}

/// Build the `llm-pi-ai` section value from the provider routes.
fn build_llm_section(providers: &[DshProviderRoute]) -> Value {
    let mut routes = serde_json::Map::new();
    for route in providers {
        let mut entry = serde_json::Map::new();
        entry.insert("apiKeyEnv".into(), Value::String(route.api_key_env.clone()));
        entry.insert("api".into(), Value::String(route.api.clone()));
        entry.insert("baseURL".into(), Value::String(route.base_url.clone()));
        if let Some(name) = &route.display_name {
            entry.insert("displayName".into(), Value::String(name.clone()));
        }
        let mut models = Vec::with_capacity(route.models.len());
        for model in &route.models {
            let mut m = serde_json::Map::new();
            m.insert("id".into(), Value::String(model.id.clone()));
            m.insert("name".into(), Value::String(model.name.clone()));
            m.insert(
                "contextWindow".into(),
                Value::Number(model.context_window.into()),
            );
            if !model.input.is_empty() {
                m.insert(
                    "input".into(),
                    Value::Array(
                        model
                            .input
                            .iter()
                            .map(|modality| Value::String(modality.clone()))
                            .collect(),
                    ),
                );
            }
            models.push(Value::Object(m));
        }
        entry.insert("models".into(), Value::Array(models));
        routes.insert(route.id.clone(), Value::Object(entry));
    }
    let mut llm = serde_json::Map::new();
    llm.insert("providers".into(), Value::Object(routes));
    Value::Object(llm)
}

/// Build the `web-search-deepseek` section value.
fn build_web_search_section(api_key_env: &str) -> Value {
    let mut section = serde_json::Map::new();
    section.insert("apiKeyEnv".into(), Value::String(api_key_env.to_string()));
    Value::Object(section)
}

/// Build the `llm-deepseek` section value that empties the native
/// `deepseek-official` adapter's model catalog (Kodex's `llm-pi-ai` deepseek
/// route replaces it in the picker).
fn build_llm_deepseek_disabled_section() -> Value {
    let mut section = serde_json::Map::new();
    section.insert("models".into(), Value::Array(Vec::new()));
    Value::Object(section)
}

/// Write the merged `settings.yaml` to `path`, replacing the Kodex-owned
/// sections and preserving all other top-level keys. The parent directory is
/// created if missing.
pub fn write_settings(path: &Path, config: &DshSettingsConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create dsh settings dir {}", parent.display()))?;
    }
    let mut doc = read_existing(path)?;
    if !doc.is_object() {
        // A non-object root (e.g. a stray scalar) is replaced wholesale; the
        // document is expected to be a mapping.
        doc = Value::Object(serde_json::Map::new());
    }
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("dsh settings.yaml root is not a mapping"))?;
    obj.insert("llm-pi-ai".into(), build_llm_section(&config.providers));
    // dsh ≥ 0.1.7 reads the default model and the default preset from cordis
    // plugin config (the `agent-default-model` / `agent-preset-registry` rows
    // of the patch overlay), not from these settings sections. Drop the
    // sections Kodex wrote for older harnesses so the file carries no dead
    // keys — including `agent-preset-registry`, whose stale `selectedDefault`
    // would otherwise override the patched default preset.
    obj.remove("agent-default-model");
    obj.remove("agent-presets");
    obj.remove("agent-preset-registry");
    match &config.web_search_api_key_env {
        Some(api_key_env) => {
            obj.insert(
                "web-search-deepseek".into(),
                build_web_search_section(api_key_env),
            );
        }
        None => {
            obj.remove("web-search-deepseek");
        }
    }
    if config.deepseek_byok_configured {
        obj.insert("llm-deepseek".into(), build_llm_deepseek_disabled_section());
    } else {
        obj.remove("llm-deepseek");
    }

    let yaml = serde_yaml::to_string(&doc).context("failed to serialize dsh settings.yaml")?;
    std::fs::write(path, yaml)
        .with_context(|| format!("failed to write dsh settings {}", path.display()))?;
    Ok(())
}

/// Resolve the dsh settings path for a Kodex data root. Convenience wrapper
/// mirroring `AppPaths::dsh_settings_path` (kept here so `dsh-bridge` tests
/// can exercise the generator without `app-core`).
pub fn settings_path_for_root(root: &Path) -> PathBuf {
    root.join("dsh").join("settings.yaml")
}

/// Resolve the Kodex-owned dsh patch-overlay path for a Kodex data root.
/// Mirrors `AppPaths::dsh_patch_path`.
pub fn patch_path_for_root(root: &Path) -> PathBuf {
    root.join("dsh").join("kodex.patch.yml")
}

/// Which model generates a dsh session title.
///
/// dsh titles a session with a small auxiliary LLM request. Kodex mounts a
/// self-contained provider beside its patch overlay: DSH handles the first
/// prompt, and the provider explicitly refreshes after each completed turn.
/// By default each request inherits the exact route logged for that turn.
/// That can fail on models which cannot answer inside the title output budget,
/// leaving the previous title (or, initially, the deterministic first-prompt
/// fallback). Pinning a pair here routes every title check to a model known to
/// work.
///
/// One external MCP server Kodex exposes to the harness.
///
/// The ACP channels receive Kodex's local MCP servers through
/// `SessionConfig.mcp_servers`; `dsh web` has no such seam (it ignores ACP MCP
/// config), so the same servers are mounted as `@deepseek-ai/dsh-mcp-client`
/// rows in the Kodex-owned `--patch` overlay instead. The harness then calls
/// them as native tools named `mcp__<server_name>__<tool>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessMcpServer {
    /// Loader row id; must be unique across the composed tree.
    pub id: String,
    /// Tool namespace: `[A-Za-z0-9_-]{1,32}`, unique inside one scope.
    pub server_name: String,
    /// Streamable-HTTP endpoint of a Kodex-owned loopback MCP server.
    pub url: String,
    /// Auth header the local server requires, and its per-server token.
    pub header_name: String,
    pub header_value: String,
    /// Per-tool-call timeout (`toolCallTimeoutMs`) for `dsh-mcp-client`, in
    /// milliseconds. `0` omits the key so the client keeps its own default
    /// (60s) — far too short for image generation, which is why a configured
    /// "300s" still died at ~2 minutes: the client deadline cut the call
    /// before the HTTP timeout could. The image server row carries the
    /// configured generation timeout here instead.
    pub tool_call_timeout_ms: u64,
}

/// `None` = keep dsh's default (inherit the current turn's route). dsh rejects
/// a half-configured pair, so both fields are always present or both absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessPatchConfig {
    pub title_provider: Option<String>,
    pub title_model: Option<String>,
    /// Absolute `file://` URL of Kodex's self-contained title provider.
    /// `None` keeps the legacy renderer shape for callers that only need a
    /// config preview; `write_harness_patch` always fills this in.
    pub title_plugin_url: Option<String>,
    /// Default model for new sessions — the `agent-default-model` row's
    /// `config` (dsh ≥ 0.1.7 reads it from plugin config, not from the
    /// settings.yaml section of the same name). `None` (or a pair with a
    /// blank half) keeps the shipped deployment default.
    pub default_model: Option<DshDefaultModel>,
    /// Default agent preset for new sessions — the `agent-preset-registry`
    /// row's `config.default`. `None` leaves the shipped default
    /// (`standard`) untouched.
    pub default_preset: Option<String>,
    /// Local MCP servers to mount for every harness session. Empty = mount
    /// none (dsh's own built-in tools are unaffected either way).
    pub mcp_servers: Vec<HarnessMcpServer>,
}

impl HarnessPatchConfig {
    /// Build from the settings' optional `(provider, model)` pair, dropping a
    /// half-configured pair or one with a blank half.
    pub fn with_title_route(route: Option<(String, String)>) -> Self {
        let (provider, model) = match route {
            Some((provider, model)) => (provider.trim().to_string(), model.trim().to_string()),
            None => (String::new(), String::new()),
        };
        if provider.is_empty() || model.is_empty() {
            return Self::default();
        }
        Self {
            title_provider: Some(provider),
            title_model: Some(model),
            ..Self::default()
        }
    }

    fn with_title_plugin_url(mut self, url: String) -> Self {
        self.title_plugin_url = Some(url);
        self
    }

    /// Pin the default model new sessions boot with. A blank provider or
    /// model drops the override (dsh rejects a half-configured pair).
    pub fn with_default_model(mut self, default: &DshDefaultModel) -> Self {
        let provider = default.provider.trim();
        let model = default.model.trim();
        if !provider.is_empty() && !model.is_empty() {
            self.default_model = Some(DshDefaultModel {
                provider: provider.to_string(),
                model: model.to_string(),
            });
        }
        self
    }

    /// Pin the default agent preset for new sessions. Blank keeps the
    /// harness's own default.
    pub fn with_default_preset(mut self, preset: Option<&str>) -> Self {
        if let Some(preset) = preset.map(str::trim).filter(|p| !p.is_empty()) {
            self.default_preset = Some(preset.to_string());
        }
        self
    }

    /// Attach the local MCP servers the harness should mount.
    pub fn with_mcp_servers(mut self, servers: Vec<HarnessMcpServer>) -> Self {
        self.mcp_servers = servers;
        self
    }
}

/// Shipped `session-title-llm` budget. Raised from dsh's 64: a reasoning model
/// spends part of the budget on its `reasoning_content` preamble and can
/// otherwise return an empty `content` with `finish_reason: length`. 8192
/// leaves enough room for that preamble plus the title on high-effort models.
const TITLE_MAX_OUTPUT_TOKENS: u32 = 8192;

/// Bounded aggregate user-message budget for the turn-end title provider.
/// The old 4 KiB first-prompt cap stopped title evaluation after only a few
/// turns; 64 KiB keeps long sessions revisable while still bounding each
/// auxiliary request. DSH retains the previous title when this cap is hit.
const TITLE_MAX_INPUT_BYTES: u32 = 65_536;

/// File written beside the Kodex-owned patch and loaded by DSH through a
/// `file://` module entry. Keeping the provider in the repository avoids
/// modifying the user's installed DSH npm package.
const KODEX_SESSION_TITLE_PLUGIN_FILE: &str = "kodex-session-title-all-prompts.mjs";
const KODEX_SESSION_TITLE_PLUGIN_SOURCE: &str =
    include_str!("../assets/kodex-session-title-all-prompts.mjs");

/// Quote a scalar for a double-quoted YAML flow scalar.
fn yaml_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Render the Kodex-owned dsh patch overlay, applied with `--patch` AFTER the
/// profile layer (bundle patches + the user's own `cordis.patch.yml`).
///
/// Kodex disables DSH's shipped `session-title-llm` row and mounts its
/// self-contained title provider from a generated `file://` module. The
/// overlay therefore remains self-contained and never edits the installed npm
/// package. DSH treats a truthy `name` in an id-targeted patch as an assertion
/// rather than a provider rename.
///
/// `settings.yaml` carries only the provider-route sections (`llm-pi-ai` and
/// friends); plugin configuration lives in the cordis patch stack instead, so
/// anything Kodex needs to change inside a bundle row — the session-title
/// budget and route, the default model, the default preset, the mounted MCP
/// servers — has to travel as an overlay like this one.
///
/// **An id-targeted entry replaces the targeted row's whole `config`, it does
/// not merge.** Every field the row declares must therefore be restated here in
/// full — dropping one silently reverts it to "unset" and the plugin's schema
/// rejects the boot.
///
/// `- insert:` entries add rows instead of overriding one, which is how Kodex
/// mounts the local MCP servers (`kodex-web-tools`, `kodex-image`) for every
/// harness session: `dsh-mcp-client` registers their tools on the host tool
/// registry, so each session inherits them as `mcp__<server>__<tool>`.
pub fn render_harness_patch(config: &HarnessPatchConfig) -> String {
    // One array entry per line rather than one `\`-continued literal: a missing
    // continuation silently leaks the source indentation into the output, and
    // an indented YAML comment is still a *valid* comment, so no parse test
    // catches it.
    let header = [
        "# Managed by Kodex — regenerated on every bring-up. Edit Kodex, not this file.",
        "#",
        "# An id-targeted entry REPLACES the row's whole `config` (it does not merge), so",
        "# every field of an overridden row is restated here in full.",
        "#",
        "# session-title-llm: Kodex raises the shipped 64-token output budget because",
        "# reasoning models can spend all of it on the reasoning preamble and leave an",
        "# empty `content`. The provider is also configured with a bounded turn-end",
        "# input so it can re-evaluate the title after later completed turns without",
        "# growing an auxiliary request without limit. The optional provider/model pair",
        "# pins every title check to a route configured in Settings → DeepSeek Harness →",
        "# 会话标题模型; without it the provider inherits the current turn's route.",
        "#",
        "# The trailing `insert:` block mounts Kodex's own local MCP servers so harness",
        "# sessions get the tools configured in Kodex Settings: the web tools provider",
        "# (kodex-web-tools) and the image capability server (kodex-image). The ACP",
        "# channels receive these through `SessionConfig.mcpServers`; `dsh web` ignores",
        "# that config, so the rows below are the only channel it has.",
    ];

    let mut out = String::with_capacity(512);
    for line in header {
        out.push_str(line);
        out.push('\n');
    }
    let title_config_indent = if config.title_plugin_url.is_some() {
        "      "
    } else {
        "    "
    };
    if let Some(plugin_url) = &config.title_plugin_url {
        // Disable DSH's shipped first-prompt row and mount Kodex's
        // self-contained turn-end title provider from the generated data root.
        out.push_str("- id: session-title-llm\n");
        out.push_str("  disabled: true\n");
        out.push_str("- insert:\n");
        out.push_str("  - id: kodex-session-title-all-prompts\n");
        out.push_str(&format!("    name: {}\n", yaml_quote(plugin_url)));
        out.push_str("    config:\n");
    } else {
        out.push_str("- id: session-title-llm\n");
        out.push_str("  config:\n");
    }
    out.push_str(&format!("{title_config_indent}targetWords: 5\n"));
    out.push_str(&format!("{title_config_indent}targetCjkCharacters: 10\n"));
    out.push_str(&format!(
        "{title_config_indent}maxInputBytes: {TITLE_MAX_INPUT_BYTES}\n"
    ));
    out.push_str(&format!(
        "{title_config_indent}maxOutputTokens: {TITLE_MAX_OUTPUT_TOKENS}\n"
    ));
    out.push_str(&format!("{title_config_indent}timeoutMs: 60000\n"));
    if let (Some(provider), Some(model)) = (&config.title_provider, &config.title_model) {
        out.push_str(&format!(
            "{title_config_indent}provider: {}\n",
            yaml_quote(provider)
        ));
        out.push_str(&format!(
            "{title_config_indent}model: {}\n",
            yaml_quote(model)
        ));
    }

    // dsh ≥ 0.1.7 plugin-config rows (see the module docs): the default model
    // and the default preset are cordis row config now, not settings.yaml
    // sections. Both rows ship with exactly the keys restated here, so an
    // id-targeted override carries the row's whole config.
    if let Some(default) = &config.default_model {
        out.push_str("- id: agent-default-model\n");
        out.push_str("  config:\n");
        out.push_str(&format!("    provider: {}\n", yaml_quote(&default.provider)));
        out.push_str(&format!("    model: {}\n", yaml_quote(&default.model)));
    }
    if let Some(preset) = &config.default_preset {
        out.push_str("- id: agent-preset-registry\n");
        out.push_str("  config:\n");
        out.push_str(&format!("    default: {}\n", yaml_quote(preset)));
    }

    if !config.mcp_servers.is_empty() {
        out.push_str("- insert:\n");
        for server in &config.mcp_servers {
            out.push_str(&format!("    - id: {}\n", yaml_quote(&server.id)));
            out.push_str("      name: '@deepseek-ai/dsh-mcp-client'\n");
            out.push_str("      config:\n");
            out.push_str(&format!(
                "        serverName: {}\n",
                yaml_quote(&server.server_name)
            ));
            out.push_str("        transport: streamable-http\n");
            out.push_str(&format!("        url: {}\n", yaml_quote(&server.url)));
            out.push_str("        headers:\n");
            out.push_str(&format!(
                "          {}: {}\n",
                server.header_name,
                yaml_quote(&server.header_value)
            ));
            if server.tool_call_timeout_ms > 0 {
                out.push_str(&format!(
                    "        toolCallTimeoutMs: {}\n",
                    server.tool_call_timeout_ms
                ));
            }
        }
    }
    out
}

/// Write the Kodex-owned dsh patch overlay to `path`, together with the
/// self-contained title provider module it references. The parent directory is
/// created if missing, and unchanged files are left alone so repeated bring-ups
/// do not rewrite them.
pub fn write_harness_patch(path: &Path, config: &HarnessPatchConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create dsh patch dir {}", parent.display()))?;
    }

    let plugin_path = path.with_file_name(KODEX_SESSION_TITLE_PLUGIN_FILE);
    let existing_plugin = std::fs::read_to_string(&plugin_path).ok();
    if existing_plugin.as_deref() != Some(KODEX_SESSION_TITLE_PLUGIN_SOURCE) {
        std::fs::write(&plugin_path, KODEX_SESSION_TITLE_PLUGIN_SOURCE).with_context(|| {
            format!(
                "failed to write Kodex DSH title provider {}",
                plugin_path.display()
            )
        })?;
    }
    let plugin_path = std::fs::canonicalize(&plugin_path).with_context(|| {
        format!(
            "failed to resolve Kodex DSH title provider {}",
            plugin_path.display()
        )
    })?;
    let plugin_url = Url::from_file_path(&plugin_path)
        .map_err(|_| anyhow!("failed to build file URL for DSH title provider"))?
        .to_string();
    let rendered = render_harness_patch(&config.clone().with_title_plugin_url(plugin_url));

    let existing = std::fs::read_to_string(path).ok();
    if existing.as_deref() == Some(rendered.as_str()) {
        return Ok(());
    }
    std::fs::write(path, &rendered)
        .with_context(|| format!("failed to write dsh patch overlay {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route() -> DshProviderRoute {
        DshProviderRoute {
            id: "deepseek".into(),
            api_key_env: "KODEX_DSH_DEEPSEEK_KEY".into(),
            api: "openai-completions".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            display_name: Some("DeepSeek".into()),
            models: vec![DshModelEntry {
                id: "deepseek-v4-pro".into(),
                name: "DeepSeek V4 Pro".into(),
                context_window: 1000000,
                input: vec!["text".into()],
            }],
        }
    }

    #[test]
    fn key_env_name_is_uppercase_underscored() {
        assert_eq!(key_env_for_provider("deepseek"), "KODEX_DSH_DEEPSEEK_KEY");
        assert_eq!(key_env_for_provider("kimi-code"), "KODEX_DSH_KIMI_CODE_KEY");
    }

    #[test]
    fn declared_model_modalities_reach_the_generated_settings() {
        // dsh resolves a model's image support from its own catalog and defaults
        // an undeclared model to text-only (`attachment-error` on every image),
        // so the `input` list is the only channel Kodex has into that decision.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let mut route = sample_route();
        route.models = vec![
            DshModelEntry {
                id: "vision-model".into(),
                name: "Vision".into(),
                context_window: 100000,
                input: vec!["text".into(), "image".into()],
            },
            DshModelEntry {
                id: "text-model".into(),
                name: "Text".into(),
                context_window: 100000,
                input: vec!["text".into()],
            },
        ];
        write_settings(
            &path,
            &DshSettingsConfig {
                providers: vec![route],
                ..Default::default()
            },
        )
        .unwrap();

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let models = written["llm-pi-ai"]["providers"]["deepseek"]["models"]
            .as_sequence()
            .expect("models list");
        assert_eq!(
            models[0]["input"],
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::String("text".into()),
                serde_yaml::Value::String("image".into()),
            ])
        );
        assert_eq!(
            models[1]["input"],
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("text".into())])
        );
    }

    #[test]
    fn write_creates_file_with_provider_routes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: None,
            deepseek_byok_configured: false,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("llm-pi-ai:"));
        assert!(text.contains("apiKeyEnv: KODEX_DSH_DEEPSEEK_KEY"));
        assert!(text.contains("baseURL: https://api.deepseek.com/v1"));
        // The default model rides the patch overlay (dsh ≥ 0.1.7), not this
        // document.
        assert!(!text.contains("agent-default-model:"));
        assert!(!text.contains("web-search-deepseek:"));
        assert!(!text.contains("llm-deepseek:"));
    }

    #[test]
    fn write_disables_native_deepseek_catalog_when_byok_configured() {
        // With a DeepSeek BYOK key injected, dsh's native `deepseek-official`
        // adapter would resolve the same key and list a second DeepSeek group
        // next to the llm-pi-ai route. Empting its advisory catalog hides the
        // duplicate without breaking sessions already routed to it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: Some("KODEX_DSH_DEEPSEEK_KEY".into()),
            deepseek_byok_configured: true,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("llm-deepseek:"));
        assert!(text.contains("models: []"));
    }

    #[test]
    fn write_removes_llm_deepseek_section_when_byok_not_configured() {
        // Without a Kodex DeepSeek key, no override must linger: the native
        // adapter falls back to its own DEEPSEEK_API_KEY behavior.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: None,
            deepseek_byok_configured: true,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("llm-deepseek:")
        );
        let cfg2 = DshSettingsConfig {
            deepseek_byok_configured: false,
            ..cfg
        };
        write_settings(&path, &cfg2).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("llm-deepseek:")
        );
    }

    #[test]
    fn write_web_search_section_points_at_injected_key_env() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: Some("KODEX_DSH_DEEPSEEK_KEY".into()),
            deepseek_byok_configured: true,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("web-search-deepseek:"));
        assert!(text.contains("apiKeyEnv: KODEX_DSH_DEEPSEEK_KEY"));
    }

    #[test]
    fn write_removes_web_search_section_when_key_env_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: Some("KODEX_DSH_DEEPSEEK_KEY".into()),
            deepseek_byok_configured: true,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("web-search-deepseek:")
        );
        // Key revoked: the section must disappear rather than leave a dangling
        // reference to an env var nothing injects anymore.
        let cfg2 = DshSettingsConfig {
            web_search_api_key_env: None,
            ..cfg
        };
        write_settings(&path, &cfg2).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("web-search-deepseek:")
        );
    }

    #[test]
    fn model_entries_omit_max_tokens() {
        // Regression: a configured model `maxTokens` becomes dsh's per-request
        // default (adapterDefaults.maxTokens) on top of the model capability
        // pi-ai already sends, and the upstream litellm rejects the duplicate
        // `max_tokens` argument. Capability stays out of generated settings.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: None,
            deepseek_byok_configured: false,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("maxTokens"));
        assert!(text.contains("contextWindow: 1000000"));
    }

    #[test]
    fn write_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        std::fs::write(
            &path,
            "ui-onboarding:\n  welcomeNoticeVersion: '1'\nagent-presets:\n  default: code\nagent-default-model:\n  provider: deepseek\n  model: deepseek-v4-pro\n",
        )
        .unwrap();
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: None,
            deepseek_byok_configured: false,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("ui-onboarding:"));
        assert!(text.contains("welcomeNoticeVersion"));
        assert!(text.contains("llm-pi-ai:"));
        // dsh ≥ 0.1.7 reads the default model/preset from the patch overlay's
        // plugin rows; the settings sections are dead keys and must be dropped
        // so a stale `agent-preset-registry.selectedDefault` (or an old
        // `agent-presets.default`) cannot override the patched default.
        assert!(!text.contains("agent-presets:"));
        assert!(!text.contains("agent-default-model:"));
        assert!(!text.contains("agent-preset-registry:"));
    }

    #[test]
    fn write_replaces_owned_sections_on_second_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.yaml");
        let cfg = DshSettingsConfig {
            providers: vec![sample_route()],
            default_model: DshDefaultModel {
                provider: "deepseek".into(),
                model: "deepseek-v4-pro".into(),
            },
            web_search_api_key_env: None,
            deepseek_byok_configured: false,
            ..Default::default()
        };
        write_settings(&path, &cfg).unwrap();
        // Second run with a different provider set.
        let cfg2 = DshSettingsConfig {
            providers: vec![DshProviderRoute {
                id: "kimi".into(),
                api_key_env: "KODEX_DSH_KIMI_KEY".into(),
                api: "openai-completions".into(),
                base_url: "https://api.kimi.com/v1".into(),
                display_name: None,
                models: vec![],
            }],
            default_model: DshDefaultModel {
                provider: "kimi".into(),
                model: "kimi-k3".into(),
            },
            ..Default::default()
        };
        write_settings(&path, &cfg2).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("kimi:"));
        assert!(text.contains("KODEX_DSH_KIMI_KEY"));
        // The old deepseek route is gone (the section is replaced wholesale).
        assert!(!text.contains("KODEX_DSH_DEEPSEEK_KEY"));
        // The default model rides the patch overlay, not settings.yaml.
        assert!(!text.contains("kimi-k3"));
    }

    // ---- harness patch overlay ----

    /// Parse the rendered overlay and return the `session-title-llm` entry's
    /// `config` mapping. Parsing rather than string-matching proves the YAML is
    /// loadable by dsh's loader, which is the failure that matters.
    fn parsed_title_config(config: &HarnessPatchConfig) -> serde_yaml::Value {
        let text = render_harness_patch(config);
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .unwrap_or_else(|e| panic!("rendered overlay is not valid YAML: {e}\n{text}"));
        let entries = doc.as_sequence().expect("overlay must be a top-level array");
        assert_eq!(entries.len(), 1, "exactly one patch entry");
        let entry = &entries[0];
        assert_eq!(
            entry.get("id").and_then(|v| v.as_str()),
            Some("session-title-llm")
        );
        entry
            .get("config")
            .cloned()
            .expect("the entry must carry a config")
    }

    #[test]
    fn harness_patch_restates_the_whole_title_config() {
        let config = parsed_title_config(&HarnessPatchConfig::default());
        // An id-targeted patch REPLACES the row's config, so every field the
        // bundle row declares must be present or dsh's schema rejects the boot.
        for key in [
            "targetWords",
            "targetCjkCharacters",
            "maxInputBytes",
            "maxOutputTokens",
            "timeoutMs",
        ] {
            assert!(config.get(key).is_some(), "missing restated field {key}");
        }
        assert_eq!(
            config.get("maxOutputTokens").and_then(|v| v.as_u64()),
            Some(u64::from(TITLE_MAX_OUTPUT_TOKENS))
        );
        assert_eq!(
            config.get("maxInputBytes").and_then(|v| v.as_u64()),
            Some(u64::from(TITLE_MAX_INPUT_BYTES))
        );
        assert!(
            TITLE_MAX_OUTPUT_TOKENS > 64,
            "must exceed dsh's shipped 64-token budget"
        );
        assert!(
            TITLE_MAX_INPUT_BYTES >= 16_384,
            "turn-end checks need more than the old 4 KiB first-prompt cap"
        );
        // No route configured => the provider inherits the current turn's route.
        assert!(config.get("provider").is_none());
        assert!(config.get("model").is_none());
    }

    #[test]
    fn harness_patch_pins_the_configured_title_route() {
        let config = parsed_title_config(&HarnessPatchConfig {
            title_provider: Some("kimi_code".into()),
            // A model id with a slash must survive YAML round-tripping.
            title_model: Some("cline-pass/deepseek-v4.1-flash".into()),
            ..Default::default()
        });
        assert_eq!(
            config.get("provider").and_then(|v| v.as_str()),
            Some("kimi_code")
        );
        assert_eq!(
            config.get("model").and_then(|v| v.as_str()),
            Some("cline-pass/deepseek-v4.1-flash")
        );
    }

    #[test]
    fn harness_patch_quotes_scalars_that_would_break_yaml() {
        // dsh rejects a half-configured pair, so a crafted id must not be able
        // to inject structure either.
        let config = parsed_title_config(&HarnessPatchConfig {
            title_provider: Some("weird: provider".into()),
            title_model: Some("mo\"del #x".into()),
            ..Default::default()
        });
        assert_eq!(
            config.get("provider").and_then(|v| v.as_str()),
            Some("weird: provider")
        );
        assert_eq!(
            config.get("model").and_then(|v| v.as_str()),
            Some("mo\"del #x")
        );
    }

    /// Parse the rendered overlay and return the entry whose `id` matches.
    fn parsed_entry(config: &HarnessPatchConfig, id: &str) -> Option<serde_yaml::Value> {
        let text = render_harness_patch(config);
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .unwrap_or_else(|e| panic!("rendered overlay is not valid YAML: {e}\n{text}"));
        doc.as_sequence()
            .expect("overlay must be a top-level array")
            .iter()
            .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(id))
            .cloned()
    }

    #[test]
    fn harness_patch_pins_default_model_and_preset_as_plugin_rows() {
        // dsh ≥ 0.1.7 reads both from cordis plugin config, not settings.yaml.
        let config = HarnessPatchConfig::default()
            .with_default_model(&DshDefaultModel {
                provider: "kimi_code".into(),
                model: "k3".into(),
            })
            .with_default_preset(Some("code"));
        let model_row = parsed_entry(&config, "agent-default-model")
            .expect("agent-default-model row missing");
        let model_config = model_row.get("config").expect("row must carry config");
        assert_eq!(
            model_config.get("provider").and_then(|v| v.as_str()),
            Some("kimi_code")
        );
        assert_eq!(model_config.get("model").and_then(|v| v.as_str()), Some("k3"));

        let preset_row = parsed_entry(&config, "agent-preset-registry")
            .expect("agent-preset-registry row missing");
        assert_eq!(
            preset_row
                .get("config")
                .and_then(|c| c.get("default"))
                .and_then(|v| v.as_str()),
            Some("code")
        );
    }

    #[test]
    fn harness_patch_omits_blank_default_model_and_preset() {
        let config = HarnessPatchConfig::default()
            .with_default_model(&DshDefaultModel::default())
            .with_default_preset(None)
            .with_default_preset(Some("  "));
        assert!(parsed_entry(&config, "agent-default-model").is_none());
        assert!(parsed_entry(&config, "agent-preset-registry").is_none());
    }

    #[test]
    fn harness_patch_mounts_the_configured_mcp_servers() {
        let config = HarnessPatchConfig::default().with_mcp_servers(vec![HarnessMcpServer {
            id: "kodex-web-tools-mcp".into(),
            server_name: "kodex_web_tools".into(),
            url: "http://127.0.0.1:54321/mcp".into(),
            header_name: "x-kodex-web-tools-token".into(),
            header_value: "tok:en #1".into(),
            tool_call_timeout_ms: 315_000,
        }]);
        let text = render_harness_patch(&config);
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .unwrap_or_else(|e| panic!("rendered overlay is not valid YAML: {e}\n{text}"));
        let entries = doc.as_sequence().expect("overlay must be a top-level array");
        assert_eq!(entries.len(), 2, "title override + one insert block");

        // The insert block is what dsh's loader turns into mcp-client rows.
        let inserted = entries[1]
            .get("insert")
            .and_then(|v| v.as_sequence())
            .expect("second entry must be an insert list");
        assert_eq!(inserted.len(), 1);
        let row = &inserted[0];
        assert_eq!(
            row.get("name").and_then(|v| v.as_str()),
            Some("@deepseek-ai/dsh-mcp-client")
        );
        let row_config = row.get("config").expect("row config");
        assert_eq!(
            row_config.get("serverName").and_then(|v| v.as_str()),
            Some("kodex_web_tools")
        );
        assert_eq!(
            row_config.get("transport").and_then(|v| v.as_str()),
            Some("streamable-http")
        );
        assert_eq!(
            row_config.get("url").and_then(|v| v.as_str()),
            Some("http://127.0.0.1:54321/mcp")
        );
        // A crafted token must not be able to inject YAML structure.
        assert_eq!(
            row_config
                .get("headers")
                .and_then(|v| v.get("x-kodex-web-tools-token"))
                .and_then(|v| v.as_str()),
            Some("tok:en #1")
        );
        // The per-call timeout must reach the mcp-client row — its own 60s
        // default is what killed long generate_image calls before the HTTP
        // timeout could.
        assert_eq!(
            row_config.get("toolCallTimeoutMs").and_then(|v| v.as_u64()),
            Some(315_000)
        );
    }

    #[test]
    fn harness_patch_omits_tool_timeout_when_unset() {
        let config = HarnessPatchConfig::default().with_mcp_servers(vec![HarnessMcpServer {
            id: "kodex-web-tools-mcp".into(),
            server_name: "kodex_web_tools".into(),
            url: "http://127.0.0.1:54321/mcp".into(),
            header_name: "x-kodex-web-tools-token".into(),
            header_value: "tok".into(),
            tool_call_timeout_ms: 0,
        }]);
        let text = render_harness_patch(&config);
        assert!(!text.contains("toolCallTimeoutMs"), "unset must keep dsh default:\n{text}");
    }

    #[test]
    fn harness_patch_omits_the_insert_block_without_mcp_servers() {
        // dsh loads the overlay on every boot; with nothing to mount there must
        // be no second entry at all (the header merely documents the block).
        let text = render_harness_patch(&HarnessPatchConfig::default());
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        let entries = doc.as_sequence().expect("overlay must be a top-level array");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].get("id").is_some());
        assert!(entries[0].get("insert").is_none());
    }

    #[test]
    fn with_title_route_drops_half_configured_pairs() {
        assert_eq!(
            HarnessPatchConfig::with_title_route(Some(("kimi_code".into(), "k3".into()))),
            HarnessPatchConfig {
                title_provider: Some("kimi_code".into()),
                title_model: Some("k3".into()),
                mcp_servers: Vec::new(),
                ..HarnessPatchConfig::default()
            }
        );
        for half in [
            None,
            Some((String::new(), "k3".into())),
            Some(("kimi_code".into(), String::new())),
            Some(("  ".into(), "k3".into())),
        ] {
            assert_eq!(
                HarnessPatchConfig::with_title_route(half),
                HarnessPatchConfig::default(),
                "a half-configured route must be dropped, never half-written"
            );
        }
    }

    #[test]
    fn harness_patch_comment_lines_are_not_indented() {
        // Regression: a missing `\` continuation in the literal leaked the
        // Rust source indentation into an output comment. YAML accepts an
        // indented comment, so the parse-based tests above cannot see it.
        let text = render_harness_patch(&HarnessPatchConfig::default());
        for line in text.lines() {
            assert!(
                !(line.starts_with(' ') && line.trim_start().starts_with('#')),
                "indented comment line — broken string continuation: {line:?}"
            );
        }
    }

    #[test]
    fn write_harness_patch_creates_parent_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("kodex.patch.yml");
        let config = HarnessPatchConfig::with_title_route(Some((
            "kimi_code".into(),
            "k3".into(),
        )));
        write_harness_patch(&path, &config).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("maxOutputTokens: 8192"));
        assert!(first.contains("disabled: true"));
        assert!(first.contains("kodex-session-title-all-prompts"));
        let entries = serde_yaml::from_str::<serde_yaml::Value>(&first)
            .unwrap()
            .as_sequence()
            .cloned()
            .expect("generated patch must be a YAML sequence");
        assert_eq!(entries.len(), 2, "disabled base row + local provider row");
        assert_eq!(
            entries[0].get("disabled").and_then(|value| value.as_bool()),
            Some(true)
        );
        let plugin_row = entries[1]
            .get("insert")
            .and_then(|value| value.as_sequence())
            .and_then(|rows| rows.first())
            .expect("local provider row must be inserted");
        assert_eq!(
            plugin_row.get("id").and_then(|value| value.as_str()),
            Some("kodex-session-title-all-prompts")
        );
        let plugin_config = plugin_row
            .get("config")
            .expect("local provider config must be nested under config");
        assert_eq!(
            plugin_config.get("maxOutputTokens").and_then(|value| value.as_u64()),
            Some(8192)
        );
        assert_eq!(
            plugin_config.get("provider").and_then(|value| value.as_str()),
            Some("kimi_code")
        );
        assert_eq!(
            plugin_config.get("model").and_then(|value| value.as_str()),
            Some("k3")
        );
        assert!(plugin_row.get("targetWords").is_none());
        let plugin_path = path.with_file_name(KODEX_SESSION_TITLE_PLUGIN_FILE);
        assert_eq!(
            std::fs::read_to_string(&plugin_path).unwrap(),
            KODEX_SESSION_TITLE_PLUGIN_SOURCE
        );

        // Regenerating is a no-op, and a changed route rewrites the file.
        write_harness_patch(&path, &config).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        let cleared = HarnessPatchConfig::default();
        write_harness_patch(&path, &cleared).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_ne!(second, first);
        assert!(!second.contains("provider:"));
    }
}
