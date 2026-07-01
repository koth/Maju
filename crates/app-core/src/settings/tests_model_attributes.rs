use super::*;
use serde_json::json;
use tempfile::tempdir;
use workspace_model::AgentProviderFamily;

// ============================================================================
// Tests for `add-model-attributes` change (provider model catalog v1 -> v2)
// ============================================================================

#[test]
fn provider_models_v1_file_is_upgraded_to_v2_on_read() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    // Write a v1 `provider-models.json` with bare `Vec<String>` models.
    std::fs::write(
        provider_models_path(&paths),
        r#"{
  "version": 1,
  "providers": {
    "timiai": {
      "models": ["gpt-5.4", "claude-opus-4.8"]
    }
  }
}"#,
    )
    .unwrap();

    let catalog = load_provider_models_catalog(&paths);
    assert_eq!(catalog.version, PROVIDER_MODELS_VERSION);
    let timiai = catalog.providers.get(TIMIAI_PROVIDER_ID).unwrap();
    let slugs: Vec<String> = timiai.models.iter().map(|e| e.slug.clone()).collect();
    assert_eq!(slugs, vec!["gpt-5.4", "claude-opus-4.8"]);
    // No custom attributes were present in v1.
    for entry in &timiai.models {
        assert!(entry.display_name.is_none());
        assert!(entry.context_window.is_none());
        assert!(entry.max_output_tokens.is_none());
        assert!(entry.supports_image_input.is_none());
        assert!(entry.reasoning_effort.is_none());
    }
}

#[test]
fn provider_models_v2_round_trip_preserves_rich_attributes() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));

    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![
            workspace_model::ModelAttributesInput {
                slug: "gpt-5.4".to_string(),
                display_name: Some("GPT-5.4".to_string()),
                context_window: Some(400_000),
                max_output_tokens: Some(64_000),
                supports_image_input: Some(false),
                reasoning_effort: Some(workspace_model::ReasoningEffort::Medium),
            },
            workspace_model::ModelAttributesInput::from_slug("claude-opus-4.8"),
        ],
    )
    .unwrap();

    let raw = std::fs::read_to_string(provider_models_path(&paths)).unwrap();
    let raw_json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(raw_json["version"].as_u64().unwrap(), 2);
    let timiai_models = raw_json["providers"][TIMIAI_PROVIDER_ID]["models"]
        .as_array()
        .unwrap();
    assert_eq!(timiai_models.len(), 2);
    let gpt = &timiai_models[0];
    assert_eq!(gpt["slug"], "gpt-5.4");
    assert_eq!(gpt["context_window"], 400_000);
    assert_eq!(gpt["max_output_tokens"], 64_000);
    assert_eq!(gpt["supports_image_input"], false);
    assert_eq!(gpt["reasoning_effort"], "medium");

    // Slug-only entries serialize as just `{ "slug": "..." }`.
    let claude = &timiai_models[1];
    assert_eq!(claude.as_object().unwrap().len(), 1);
    assert_eq!(claude["slug"], "claude-opus-4.8");
}

#[test]
fn resolve_model_attributes_precedence() {
    // Static table baseline (gpt-5.5 is in MODEL_MAX_OUTPUT_TOKENS at 128_000).
    let baseline = resolve_model_attributes(None, "gpt-5.5", TIMIAI_PROVIDER_ID);
    assert_eq!(baseline.max_output_tokens, 128_000);
    assert_eq!(baseline.reasoning_effort, workspace_model::ReasoningEffort::None);

    // Custom max_output_tokens overrides the static table.
    let custom = resolve_model_attributes(
        Some(&workspace_model::ModelCatalogEntry {
            slug: "gpt-5.5".to_string(),
            display_name: None,
            context_window: None,
            max_output_tokens: Some(32_000),
            supports_image_input: None,
            reasoning_effort: Some(workspace_model::ReasoningEffort::High),
        }),
        "gpt-5.5",
        TIMIAI_PROVIDER_ID,
    );
    assert_eq!(custom.max_output_tokens, 32_000);
    assert_eq!(custom.reasoning_effort, workspace_model::ReasoningEffort::High);
}

#[test]
fn resolve_model_attributes_falls_back_to_static_when_attributes_missing() {
    let entry = workspace_model::ModelCatalogEntry {
        slug: "gpt-5.5".to_string(),
        display_name: Some("GPT-5.5".to_string()),
        context_window: None,
        max_output_tokens: None,
        supports_image_input: None,
        reasoning_effort: None,
    };
    let resolved = resolve_model_attributes(Some(&entry), "gpt-5.5", TIMIAI_PROVIDER_ID);
    // Static table values: gpt-5.5 context window is 1_050_000 and max output is 128_000.
    assert_eq!(resolved.context_window, 1_050_000);
    assert_eq!(resolved.max_output_tokens, 128_000);
    // Reasoning defaults to None when neither the custom entry nor the static
    // table supplies it.
    assert_eq!(resolved.reasoning_effort, workspace_model::ReasoningEffort::None);
}

#[test]
fn resolve_model_attributes_multimodal_override_beats_heuristic() {
    // `deepseek-v4-pro` is text-only by heuristic; the custom override must win.
    let entry = workspace_model::ModelCatalogEntry {
        slug: "deepseek-v4-pro".to_string(),
        display_name: None,
        context_window: None,
        max_output_tokens: None,
        supports_image_input: Some(true),
        reasoning_effort: None,
    };
    let resolved =
        resolve_model_attributes(Some(&entry), "deepseek-v4-pro", DEEPSEEK_PROVIDER_ID);
    assert!(resolved.supports_image_input);
}

#[test]
fn codex_acp_model_catalog_entry_reflects_custom_attributes() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![workspace_model::ModelAttributesInput {
            slug: "gpt-5.5".to_string(),
            display_name: Some("GPT-5.5 (Kodex)".to_string()),
            context_window: Some(500_000),
            max_output_tokens: Some(96_000),
            supports_image_input: Some(false),
            reasoning_effort: Some(workspace_model::ReasoningEffort::High),
        }],
    )
    .unwrap();

    let catalog = codex_acp_model_catalog_content(&paths, TIMIAI_PROVIDER_ID).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let entry = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == "gpt-5.5")
        .expect("gpt-5.5 should be in catalog");
    assert_eq!(entry["context_window"].as_i64(), Some(500_000));
    assert_eq!(entry["max_context_window"].as_i64(), Some(500_000));
    assert_eq!(entry["max_output_tokens"].as_i64(), Some(96_000));
    // Multimodal override: deepseek-style text-only is overridden to false.
    assert_eq!(entry["supports_image_detail_original"].as_bool(), Some(false));
    assert_eq!(entry["input_modalities"], json!(["text"]));
    assert_eq!(entry["default_reasoning_level"], "high");
    let supported = entry["supported_reasoning_levels"].as_array().unwrap();
    assert_eq!(supported.len(), 1);
    assert_eq!(supported[0]["effort"], "high");
}

#[test]
fn codex_acp_model_catalog_entry_defaults_remain_when_attributes_missing() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![workspace_model::ModelAttributesInput::from_slug("gpt-5.5")],
    )
    .unwrap();

    let catalog = codex_acp_model_catalog_content(&paths, TIMIAI_PROVIDER_ID).unwrap();
    let catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let entry = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == "gpt-5.5")
        .unwrap();
    // Custom attribute absent: defaults to the static table.
    assert_eq!(
        entry["context_window"].as_i64(),
        Some(model_context_window_for_provider("gpt-5.5", TIMIAI_PROVIDER_ID))
    );
    assert_eq!(
        entry["max_output_tokens"].as_i64(),
        Some(model_max_output_tokens_for_provider("gpt-5.5", TIMIAI_PROVIDER_ID))
    );
    // No reasoning attribute: catalog stays on the historical `none` shape.
    assert_eq!(entry["default_reasoning_level"], "none");
    let supported = entry["supported_reasoning_levels"].as_array().unwrap();
    assert_eq!(supported.len(), 1);
    assert_eq!(supported[0]["effort"], "none");
}

#[test]
fn write_codex_byok_channel_config_writes_custom_max_output_and_reasoning() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    paths.ensure_root().unwrap();
    // Configure a BYOK source with custom attributes.
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![workspace_model::ModelAttributesInput {
            slug: TIMIAI_CODEX_MODEL.to_string(),
            display_name: None,
            context_window: Some(400_000),
            max_output_tokens: Some(24_000),
            supports_image_input: None,
            reasoning_effort: Some(workspace_model::ReasoningEffort::Medium),
        }],
    )
    .unwrap();
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();

    write_codex_byok_channel_config(&paths).unwrap();
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert!(doc.get("model_context_window").is_none());
    assert_eq!(doc["model_max_output_tokens"].as_integer(), Some(24_000));
    assert_eq!(doc["model_reasoning_effort"].as_str(), Some("medium"));
}

#[test]
fn write_codex_byok_channel_config_falls_back_to_static_when_no_custom_attributes() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    paths.ensure_root().unwrap();
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![workspace_model::ModelAttributesInput::from_slug(TIMIAI_CODEX_MODEL)],
    )
    .unwrap();
    save_agent_provider_secret(
        &paths,
        AgentProviderFamily::Codex,
        TIMIAI_PROVIDER_ID,
        "timiai-secret",
    )
    .unwrap();

    write_codex_byok_channel_config(&paths).unwrap();
    let content = std::fs::read_to_string(codex_config_path(&paths)).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    assert_eq!(
        doc["model_max_output_tokens"].as_integer(),
        Some(model_max_output_tokens_for_provider(
            TIMIAI_CODEX_MODEL,
            TIMIAI_PROVIDER_ID
        ))
    );
    assert_eq!(doc["model_reasoning_effort"].as_str(), Some("none"));
}

#[test]
fn settings_snapshot_surfaces_model_entries_with_attributes() {
    let dir = tempdir().unwrap();
    let paths = AppPaths::from_root(dir.path().join(".kodex"));
    save_provider_models(
        &paths,
        TIMIAI_PROVIDER_ID,
        vec![workspace_model::ModelAttributesInput {
            slug: "gpt-5.4".to_string(),
            display_name: Some("GPT-5.4".to_string()),
            context_window: Some(400_000),
            max_output_tokens: Some(16_000),
            supports_image_input: Some(true),
            reasoning_effort: Some(workspace_model::ReasoningEffort::Low),
        }],
    )
    .unwrap();
    let snapshot = settings_snapshot(&paths);
    let parsed = serde_json::to_value(&snapshot).unwrap();
    let timiai = parsed["codex_acp"]["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == TIMIAI_PROVIDER_ID)
        .unwrap();
    let entries = timiai["model_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["slug"], "gpt-5.4");
    assert_eq!(entries[0]["context_window"], 400_000);
    assert_eq!(entries[0]["max_output_tokens"], 16_000);
    assert_eq!(entries[0]["supports_image_input"], true);
    assert_eq!(entries[0]["reasoning_effort"], "low");
}
