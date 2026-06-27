//! Per-session native image capability resolution.
//!
//! Determines whether the active model/provider natively supports image
//! understanding (`native_view`), generation (`native_generate`), and editing
//! (`native_edit`). When a native capability is missing, `app-core` injects the
//! unified `kodex-image` MCP server to back the corresponding fallback tool
//! (`view_image` / `generate_image` / `edit_image`).
//!
//! `native_view` is derived from a static keyword table (the same signal the
//! model catalog uses to emit `input_modalities`) plus BYOK slug decoding,
//! defaulting to capable for unknown models so multimodal models are never
//! forced through the tool path. `native_generate` is true only for the
//! codex-acp channel under the `default` (ChatGPT login) provider, because
//! codex-acp's native `ImageGenerationBegin/End` protocol events only fire
//! there. `native_edit` is always false: Kodex has no native image-editing
//! capability, so editing is always delivered through the MCP `edit_image` tool.

use workspace_model::ImageCapabilities;

use crate::settings::{is_claude_agent_acp_command, is_codex_acp_command};

/// Model name substrings that indicate a text-only model (no image input).
const TEXT_ONLY_MODEL_KEYWORDS: &[&str] = &["deepseek"];

/// Model name substrings that indicate a multimodal model (image input).
/// Mirrors the models present in the BYOK catalogs that carry image
/// `input_modalities` in the generated codex-acp model catalog.
const MULTIMODAL_MODEL_KEYWORDS: &[&str] = &[
    "gpt-5",
    "gpt-4o",
    "claude-opus",
    "claude-sonnet",
    "gemini",
    "glm-5",
    "kimi",
    "mimo",
];

/// The codex-acp provider id that backs native image generation
/// (ChatGPT login state). Native `ImageGenerationBegin/End` only fires here.
const DEFAULT_PROVIDER_ID: &str = "default";

/// Resolve native image capabilities for a session.
///
/// `provider` is the active codex provider id for the codex-acp channel
/// (e.g. `"default"`, `"timiai"`, `"deepseek"`); it may be `None` for the
/// kodex-claude channel. `agent_command` selects the channel.
pub fn resolve_image_capabilities(
    model: &str,
    provider: Option<&str>,
    agent_command: &str,
) -> ImageCapabilities {
    let (decoded_model, decoded_provider) = decode_byok_identifier(model, provider);
    let is_codex = is_codex_acp_command(agent_command);
    let is_claude = is_claude_agent_acp_command(agent_command);

    let native_view = model_supports_image_input(&decoded_model);
    let native_generate = is_codex
        && decoded_provider.as_deref() == Some(DEFAULT_PROVIDER_ID)
        && !is_claude;
    // kodex-claude has no native generation path; BYOK codex providers go
    // through Responses→Completions conversion and never emit generation events.
    let native_edit = false;

    ImageCapabilities {
        native_view,
        native_generate,
        native_edit,
        // `view_fallback` is resolved by the caller (session runtime) from
        // whether the `kodex-image` MCP server is actually attached, not from
        // the model name, so it always starts `false` here.
        view_fallback: false,
    }
}

/// Whether a model accepts image input, mirroring the `input_modalities`
/// signal emitted by the codex-acp model catalog (`codex_acp_model_catalog_entry`).
///
/// The catalog currently derives `input_modalities` from the same text-only
/// check used here; when the catalog is completed this can consult it as the
/// authoritative source. Unknown models default to image-capable (true) so
/// multimodal models are never误降级 (mis-degraded) through the tool path.
pub fn model_supports_image_input(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    if TEXT_ONLY_MODEL_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        return false;
    }
    if MULTIMODAL_MODEL_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        return true;
    }
    // Unknown model: default to capable to avoid forcing multimodal models
    // through the fallback tool path (D12).
    true
}

/// Decode an encoded BYOK provider/model identifier into its parts.
///
/// BYOK providers encode the selection as a slug such as
/// `kodex-provider/byok/timiai/gpt-5.4`; the trailing segment is the model
/// and the segment before it (when prefixed with `byok/`) is the provider.
/// Plain model names pass through unchanged.
pub fn decode_byok_identifier(
    model: &str,
    provider: Option<&str>,
) -> (String, Option<String>) {
    let lower = model.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("kodex-provider/byok/") {
        let mut parts = rest.split('/');
        let provider_part = parts.next().map(str::to_string);
        let model_part = parts.next().map(str::to_string);
        if let (Some(provider_part), Some(model_part)) = (provider_part, model_part) {
            return (model_part, Some(provider_part));
        }
    }
    (model.to_string(), provider.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX_CMD: &str = "codex-acp";
    const CLAUDE_CMD: &str = "claude-agent-acp";

    #[test]
    fn deepseek_is_text_only_under_byok() {
        let caps = resolve_image_capabilities("deepseek-v4-pro", Some("deepseek"), CODEX_CMD);
        assert!(!caps.native_view);
        assert!(!caps.native_generate);
        assert!(!caps.native_edit);
    }

    #[test]
    fn multimodal_under_default_provider_has_native_generation() {
        let caps = resolve_image_capabilities("gpt-5.4", Some("default"), CODEX_CMD);
        assert!(caps.native_view);
        assert!(caps.native_generate);
        assert!(!caps.native_edit);
    }

    #[test]
    fn multimodal_under_byok_lacks_native_generation() {
        let caps = resolve_image_capabilities("gpt-5.4", Some("timiai"), CODEX_CMD);
        assert!(caps.native_view);
        assert!(!caps.native_generate);
    }

    #[test]
    fn claude_channel_never_has_native_generation() {
        let caps = resolve_image_capabilities("claude-opus-4.8", None, CLAUDE_CMD);
        assert!(caps.native_view);
        assert!(!caps.native_generate);
        assert!(!caps.native_edit);
    }

    #[test]
    fn unknown_model_defaults_to_image_capable() {
        let caps = resolve_image_capabilities("some-new-model-9", Some("default"), CODEX_CMD);
        assert!(caps.native_view);
    }

    #[test]
    fn byok_slug_is_decoded_before_matching() {
        let caps = resolve_image_capabilities(
            "kodex-provider/byok/timiai/gpt-5.4",
            Some("byok"),
            CODEX_CMD,
        );
        assert!(caps.native_view);
        assert!(!caps.native_generate);
    }

    #[test]
    fn byok_slug_deepseek_decodes_to_text_only() {
        let caps = resolve_image_capabilities(
            "kodex-provider/byok/deepseek/deepseek-v4-pro",
            Some("byok"),
            CODEX_CMD,
        );
        assert!(!caps.native_view);
        assert!(!caps.native_generate);
    }

    #[test]
    fn glm_kimi_mimo_are_multimodal() {
        for model in &["glm-5.2", "kimi-for-coding", "MiMo-V2.5-Pro"] {
            assert!(
                model_supports_image_input(model),
                "{model} should be image-capable"
            );
        }
    }

    #[test]
    fn all_native_only_when_view_and_generate_present() {
        let caps = resolve_image_capabilities("gpt-5.4", Some("default"), CODEX_CMD);
        // native_edit is always false, so all_native is never true today.
        assert!(!caps.all_native());
    }

    #[test]
    fn validate_rejects_text_only_view_model() {
        use workspace_model::{ImageSettings, ImageViewSettings};
        let mut settings = ImageSettings::default();
        settings.enabled = true;
        settings.view = ImageViewSettings {
            provider: "deepseek".into(),
            model: "deepseek-v4-pro".into(),
        };
        assert!(crate::settings::validate_image_settings(&settings).is_err());
    }

    #[test]
    fn validate_accepts_multimodal_view_model() {
        use workspace_model::{ImageSettings, ImageViewSettings};
        let mut settings = ImageSettings::default();
        settings.enabled = true;
        settings.view = ImageViewSettings {
            provider: "timiai".into(),
            model: "claude-sonnet-4-6".into(),
        };
        assert!(crate::settings::validate_image_settings(&settings).is_ok());
    }

    #[test]
    fn validate_skips_when_disabled() {
        use workspace_model::ImageSettings;
        let mut settings = ImageSettings::default();
        settings.enabled = false;
        settings.view.model = "deepseek-v4-pro".into();
        assert!(crate::settings::validate_image_settings(&settings).is_ok());
    }
}
