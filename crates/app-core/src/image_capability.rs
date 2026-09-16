//! Per-session native image capability resolution.
//!
//! Determines whether the active model/provider natively supports image
//! understanding (`native_view`), generation (`native_generate`), and editing
//! (`native_edit`). When a native capability is missing, `app-core` injects the
//! unified `kodex-image` MCP server to back the corresponding fallback tool
//! (`view_image` / `generate_image` / `edit_image`).
//!
//! `native_view` is derived from a static keyword table (the same signal the
//! model catalog uses to emit `input_modalities`) plus BYOK/harness slug
//! decoding. The unknown-model default is channel-specific: image-capable for
//! codex-acp/kodex-claude (so multimodal models are never forced through the
//! tool path), but text-only for the dsh harness (whose own catalog defaults
//! undeclared models to text-only and rejects native images with
//! `attachment-error`, surfacing as a failed prompt). `native_generate` is
//! true only for the codex-acp channel under the `default` (ChatGPT login)
//! provider, because codex-acp's native `ImageGenerationBegin/End` protocol
//! events only fire there. `native_edit` is always false: Kodex has no native
//! image-editing capability, so editing is always delivered through the MCP
//! `edit_image` tool.

use workspace_model::ImageCapabilities;

use crate::AppPaths;
use crate::settings::{
    is_claude_agent_acp_command, is_codex_acp_command, is_deepseek_harness_command,
};

/// Model name substrings that indicate a text-only model (no image input).
/// Plain GLM-5.x text models are text-only; only GLM-*V* (vision) variants
/// accept images (matched by the `glm-v` multimodal keyword below).
const TEXT_ONLY_MODEL_KEYWORDS: &[&str] = &["deepseek", "glm-5"];

/// Model name substrings that indicate a multimodal model (image input).
/// Mirrors the models present in the BYOK catalogs that carry image
/// `input_modalities` in the generated codex-acp model catalog.
///
/// Note: only GLM-*V* (vision) variants accept image input; plain GLM-5.x
/// text models do not, so `"glm-v"` (not `"glm-5"`) is the multimodal signal.
const MULTIMODAL_MODEL_KEYWORDS: &[&str] = &[
    "gpt-5",
    "gpt-4o",
    "claude-opus",
    "claude-sonnet",
    "gemini",
    "glm-v",
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
///
/// Without `AppPaths` the user's per-model declaration cannot be consulted;
/// use [`resolve_image_capabilities_for_paths`] on the session path.
pub fn resolve_image_capabilities(
    model: &str,
    provider: Option<&str>,
    agent_command: &str,
) -> ImageCapabilities {
    let (decoded_model, decoded_provider) = decode_byok_identifier(model, provider);
    resolve_with_declared(
        &decoded_model,
        decoded_provider.as_deref(),
        agent_command,
        None,
    )
}

/// [`resolve_image_capabilities`] plus the user's per-model image-input
/// declaration from Settings → 模型.
///
/// The declaration is authoritative — the same precedence
/// `settings::resolve_model_attributes` applies when it writes the codex model
/// catalog — because the keyword table cannot know a model whose name does not
/// reveal its modality. That gap is not cosmetic: with `native_view` wrongly
/// false the model is told it cannot see pictures (the degradation notice says
/// so and `view_image` is mounted), so a vision model refuses to look at an
/// image and asks for the tool instead.
pub fn resolve_image_capabilities_for_paths(
    paths: &AppPaths,
    model: &str,
    provider: Option<&str>,
    agent_command: &str,
) -> ImageCapabilities {
    let (decoded_model, decoded_provider) = decode_byok_identifier(model, provider);
    let declared = declared_image_input(paths, &decoded_model, decoded_provider.as_deref());
    resolve_with_declared(
        &decoded_model,
        decoded_provider.as_deref(),
        agent_command,
        declared,
    )
}

/// The request modalities to declare for one harness model route.
///
/// dsh's `llm-pi-ai` catalog takes an explicit per-model `input` list, and its
/// documentation is blunt about the contract: declaring images is what makes a
/// hand-declared vision model usable, while declaring text alone corrects a
/// model the shipped catalog marks image-capable but whose gateway is not.
/// Passing Kodex's resolution through here keeps the harness and Kodex agreeing
/// on one answer instead of the harness defaulting to text-only and rejecting
/// every attachment with `attachment-error`.
pub fn harness_model_input(supports_image_input: bool) -> Vec<String> {
    if supports_image_input {
        vec!["text".to_string(), "image".to_string()]
    } else {
        vec!["text".to_string()]
    }
}

/// The user's per-model image-input declaration, when they authored one.
fn declared_image_input(
    paths: &AppPaths,
    decoded_model: &str,
    decoded_provider: Option<&str>,
) -> Option<bool> {
    let provider = decoded_provider?;
    crate::settings::lookup_model_attributes_for_active(paths, decoded_model, provider)
        .and_then(|entry| entry.supports_image_input)
}

fn resolve_with_declared(
    decoded_model: &str,
    decoded_provider: Option<&str>,
    agent_command: &str,
    declared: Option<bool>,
) -> ImageCapabilities {
    let is_codex = is_codex_acp_command(agent_command);
    let is_claude = is_claude_agent_acp_command(agent_command);
    let is_harness = is_deepseek_harness_command(agent_command);

    // The dsh harness determines per-model image support from its own model
    // catalog (`input` modality), which it does NOT expose in `session.models`
    // (the response carries only id/name/description/reasoning). A model that
    // does not declare `input: [text, image]` is text-only -- and the harness
    // defaults undeclared models to `["text"]`. Kodex writes that declaration
    // itself (`build_dsh_settings_config`), so for the harness channel an
    // *unknown* model (one neither the user nor the keyword table classifies)
    // must default to text-only and degrade image attachments through the
    // `view_image` fallback. Defaulting to capable -- as the codex-acp/claude
    // channels do -- forwards the raw image natively and the harness rejects it
    // with `attachment-error: Model "X" does not support image input`, failing
    // the prompt (perceived as a disconnect).
    let native_view = declared
        .or_else(|| classify_image_input(decoded_model))
        .unwrap_or(!is_harness);
    let native_generate =
        is_codex && decoded_provider == Some(DEFAULT_PROVIDER_ID) && !is_claude;
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
    classify_image_input(model).unwrap_or(true)
}

/// Tri-state image-input classification: `Some(true)` for a model the keyword
/// table recognizes as multimodal, `Some(false)` for one it recognizes as
/// text-only, and `None` when no keyword matches (unknown). Callers that know
/// the channel can pick a channel-specific default for the unknown case;
/// see [`resolve_image_capabilities`] (the dsh harness defaults unknown
/// models to text-only, mirroring the harness's own `["text"]` modality
/// default, because it does not expose per-model `input_modalities` in
/// `session.models`).
fn classify_image_input(model: &str) -> Option<bool> {
    let lower = model.to_ascii_lowercase();
    // Multimodal (vision) variants are checked first so a name like `GLM-5V`
    // is recognized as image-capable even though plain `glm-5` is text-only.
    if MULTIMODAL_MODEL_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        return Some(true);
    }
    if TEXT_ONLY_MODEL_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
    {
        return Some(false);
    }
    None
}

/// Decode an encoded BYOK provider/model identifier into its parts.
///
/// Two encodings share the `kodex-provider/` prefix:
///   - `kodex-provider/byok/{source_provider}/{model_slug}` (codex BYOK)
///   - `kodex-provider/{provider}/{model_slug}`              (dsh harness /
///     non-BYOK providers)
/// The first segment after the prefix is the provider (or `byok`); the
/// remainder is the model slug, which may itself contain slashes (e.g.
/// `zai-org/GLM-5.2` or `cline-pass/glm-5.2`), so it is split once and kept
/// verbatim. Plain model names (no `kodex-provider/` prefix) pass through
/// unchanged.
pub fn decode_byok_identifier(model: &str, provider: Option<&str>) -> (String, Option<String>) {
    let lower = model.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("kodex-provider/") {
        if let Some((first, model_part)) = rest.split_once('/') {
            let model_part = model_part.trim();
            if !first.is_empty() && !model_part.is_empty() {
                if first == "byok" {
                    // `kodex-provider/byok/{source_provider}/{model_slug}`:
                    // the segment after `byok` is the source provider and the
                    // remainder is the model slug.
                    if let Some((source_provider, slug)) = model_part.split_once('/') {
                        let slug = slug.trim();
                        if !source_provider.is_empty() && !slug.is_empty() {
                            return (slug.to_string(), Some(source_provider.to_string()));
                        }
                    }
                } else {
                    // `kodex-provider/{provider}/{model_slug}` (dsh harness and
                    // other non-BYOK providers): decode so the bare model slug
                    // is classified against the keyword table, not the encoded
                    // string (whose provider segment could false-positive,
                    // e.g. `kimi_code` matching the `kimi` multimodal keyword).
                    return (model_part.to_string(), Some(first.to_string()));
                }
            }
        }
    }
    (model.to_string(), provider.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX_CMD: &str = "codex-acp";
    const CLAUDE_CMD: &str = "claude-agent-acp";

    /// Paths whose catalog declares one model's image support — the per-model
    /// attributes the user edits in Settings → 模型.
    fn paths_with_declared_image_input(
        provider: &str,
        slug: &str,
        supports: bool,
    ) -> (tempfile::TempDir, AppPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".kodex"));
        crate::settings::save_provider_models(
            &paths,
            provider,
            vec![workspace_model::ModelAttributesInput {
                slug: slug.to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                supports_image_input: Some(supports),
                reasoning_effort: None,
            }],
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn declared_image_input_beats_the_text_only_keyword_table() {
        // The reported bug: a vision model the keyword table cannot classify
        // correctly (here `deepseek-flash`, text-only by name) was judged
        // text-only for the harness channel, so the degradation notice told it
        // it cannot see images and `view_image` was mounted — it then refused to
        // look at a picture itself. The user's per-model declaration is
        // authoritative, exactly as it is for the codex catalog.
        const DSH_CMD: &str = "dsh";
        let (_dir, paths) = paths_with_declared_image_input("custom_tencent", "deepseek-flash", true);

        let caps = resolve_image_capabilities_for_paths(
            &paths,
            "kodex-provider/byok/custom_tencent/deepseek-flash",
            Some("custom_tencent"),
            DSH_CMD,
        );
        assert!(
            caps.native_view,
            "a model the user declared image-capable must not be degraded"
        );
        // The harness is told the same thing, so it accepts the image instead
        // of rejecting it with `attachment-error`.
        assert_eq!(
            harness_model_input(caps.native_view),
            vec!["text".to_string(), "image".to_string()]
        );
    }

    #[test]
    fn declared_text_only_beats_a_multimodal_keyword() {
        // The other direction: the user's model page can correct a model the
        // name suggests is multimodal, and the harness declaration follows.
        const DSH_CMD: &str = "dsh";
        let (_dir, paths) = paths_with_declared_image_input("timiai", "gpt-5.5-pro", false);

        let caps = resolve_image_capabilities_for_paths(
            &paths,
            "kodex-provider/byok/timiai/gpt-5.5-pro",
            Some("timiai"),
            DSH_CMD,
        );
        assert!(!caps.native_view);
        assert_eq!(harness_model_input(caps.native_view), vec!["text".to_string()]);
    }

    #[test]
    fn undeclared_harness_model_still_defaults_to_text_only() {
        // No declaration, no keyword match: the harness default is unchanged,
        // because dsh would reject a raw image for an undeclared model.
        const DSH_CMD: &str = "dsh";
        let (_dir, paths) = paths_with_declared_image_input("custom_tencent", "unrelated-model", true);

        let caps = resolve_image_capabilities_for_paths(
            &paths,
            "kodex-provider/byok/custom_tencent/mystery-model",
            Some("custom_tencent"),
            DSH_CMD,
        );
        assert!(!caps.native_view);
    }

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
    fn harness_unknown_model_defaults_to_text_only() {
        // The dsh harness does not expose per-model `input_modalities` in
        // `session.models`, and its own catalog defaults undeclared models to
        // `["text"]`. An unknown harness model id like `k3` (kimi_code) must
        // therefore default to text-only so image attachments degrade through
        // the `view_image` fallback instead of being forwarded natively and
        // rejected with `attachment-error: Model "k3" does not support image
        // input` (which surfaces as a failed prompt / "disconnect").
        const DSH_CMD: &str = "dsh";
        // Bare model id (as stored in `ui.session.model` at session create).
        let caps = resolve_image_capabilities("k3", Some("kimi_code"), DSH_CMD);
        assert!(
            !caps.native_view,
            "unknown harness model `k3` must be text-only, not native image-capable"
        );
        // Encoded form (as sent to `reapply_image_capabilities` on model switch):
        // must decode to `k3` and NOT false-positive on the `kimi_code` provider
        // segment matching the `kimi` multimodal keyword.
        let caps_encoded = resolve_image_capabilities(
            "kodex-provider/kimi_code/k3",
            Some("kimi_code"),
            DSH_CMD,
        );
        assert!(
            !caps_encoded.native_view,
            "encoded `kodex-provider/kimi_code/k3` must decode to `k3` and be text-only"
        );
        // The codex-acp channel keeps the capable default for the same id so
        // its catalog's authoritative per-model override still wins upstream.
        let caps_codex =
            resolve_image_capabilities("k3", Some("kimi_code"), CODEX_CMD);
        assert!(
            caps_codex.native_view,
            "codex-acp unknown model must keep the image-capable default"
        );
    }

    #[test]
    fn harness_encoded_model_with_slash_decodes_to_bare_slug() {
        // A harness model slug that contains a slash (e.g. `cline-pass/glm-5.2`)
        // must decode keeping the full slug so the keyword table sees the model
        // name, not the truncated provider segment.
        let (model, provider) =
            decode_byok_identifier("kodex-provider/custom_cline/cline-pass/glm-5.2", None);
        assert_eq!(model, "cline-pass/glm-5.2");
        assert_eq!(provider.as_deref(), Some("custom_cline"));
    }

    #[test]
    fn harness_known_multimodal_still_native() {
        // A harness model the keyword table recognizes as multimodal (e.g.
        // gpt-5.x) keeps native image forwarding; only the *unknown* default
        // changes for the harness channel.
        const DSH_CMD: &str = "dsh";
        let caps =
            resolve_image_capabilities("kodex-provider/timiai/gpt-5.5", Some("timiai"), DSH_CMD);
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
    fn byok_slug_commandcode_glm_with_slash_decodes_to_text_only() {
        // The commandcode model slug `zai-org/GLM-5.2` contains a slash; the
        // decoder must keep the full model id and not truncate it to `zai-org`
        // (which would fall through to the unknown-model default and wrongly
        // appear image-capable).
        let (model, provider) = decode_byok_identifier(
            "kodex-provider/byok/commandcode/zai-org/GLM-5.2",
            Some("byok"),
        );
        assert_eq!(model, "zai-org/glm-5.2");
        assert_eq!(provider.as_deref(), Some("commandcode"));
        let caps = resolve_image_capabilities(
            "kodex-provider/byok/commandcode/zai-org/GLM-5.2",
            Some("byok"),
            CODEX_CMD,
        );
        assert!(
            !caps.native_view,
            "GLM-5.2 via commandcode must be text-only"
        );
        assert!(!caps.native_generate);
    }

    #[test]
    fn glm_v_is_multimodal_but_plain_glm_is_text_only() {
        // Only GLM-*V* (vision) variants accept image input; plain GLM-5.x
        // text models do not.
        assert!(model_supports_image_input("glm-v4-plus"));
        assert!(model_supports_image_input("glm-v4"));
        assert!(
            !model_supports_image_input("glm-5.2"),
            "plain GLM-5.2 must not be treated as image-capable"
        );
        assert!(!model_supports_image_input("zai-org/GLM-5.1"));
    }

    #[test]
    fn kimi_and_mimo_are_multimodal() {
        for model in &["kimi-for-coding", "MiMo-V2.5-Pro"] {
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
