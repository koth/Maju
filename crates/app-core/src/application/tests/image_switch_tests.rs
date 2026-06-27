//! Tests for `Application::reapply_image_capabilities` — the model-switch
//! glue that re-resolves native image capabilities and propagates the result
//! to `ui.image_capabilities`, the `prompt_capabilities.image` gate, and the
//! running image MCP server's offered tool set (without restarting it).

use super::*;

use workspace_model::ImageCapabilities;

fn text_only_caps() -> ImageCapabilities {
    ImageCapabilities {
        native_view: false,
        native_generate: false,
        native_edit: false,
        view_fallback: true,
    }
}

fn attach_text_only_image_mcp(app: &mut Application, workspace_root: std::path::PathBuf) {
    let service = crate::image_mcp::ImageMcpService::new(
        text_only_caps(),
        crate::image_mcp::ImageMcpConfig {
            workspace_root,
            settings: workspace_model::ImageSettings::default(),
            view_api_key: None,
            generate_api_key: None,
        },
    );
    let handle = crate::image_mcp::start_image_mcp_server(service).unwrap();
    app.image_mcp = Some(handle);
    app.ui.prompt_capabilities.image = false;
}

#[test]
fn reapply_image_capabilities_updates_prompt_gate_and_handle() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    attach_text_only_image_mcp(&mut app, dir.path().to_path_buf());

    // Switch to a multimodal model: native_view becomes true.
    app.reapply_image_capabilities("gpt-5.4", Some("timiai"));
    assert!(app.ui.image_capabilities.native_view);
    assert!(app.ui.prompt_capabilities.image);
    assert!(
        app.image_mcp
            .as_ref()
            .unwrap()
            .capabilities()
            .native_view
    );

    // Switch back to a text-only model: native_view becomes false, but a
    // `view_image` fallback is still attached (view_fallback=true), so the
    // prompt gate stays open — image attachments are degraded through
    // `view_image` instead of being rejected (Bug 3).
    app.reapply_image_capabilities("deepseek-v4-pro", Some("deepseek"));
    assert!(!app.ui.image_capabilities.native_view);
    assert!(app.ui.image_capabilities.view_fallback);
    assert!(app.ui.prompt_capabilities.image);
    assert!(
        !app.image_mcp
            .as_ref()
            .unwrap()
            .capabilities()
            .native_view
    );

    app.session.shutdown();
}

#[test]
fn reapply_resolves_native_view_without_fallback_attached() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    // No image MCP attached (fallback disabled). `reapply` must still resolve
    // `native_view` from the model name (Bug 1): a text-only model gates image
    // attachments closed, and a multimodal model keeps them open.
    app.ui.image_capabilities = ImageCapabilities::assumed_native();
    app.ui.prompt_capabilities.image = true;

    // Text-only model: native_view=false, view_fallback=false -> gate closed.
    app.reapply_image_capabilities("deepseek-v4-pro", Some("deepseek"));
    assert!(!app.ui.image_capabilities.native_view);
    assert!(!app.ui.image_capabilities.view_fallback);
    assert!(!app.ui.prompt_capabilities.image);

    // Multimodal model: native_view=true -> gate open even without fallback.
    app.reapply_image_capabilities("gpt-5.4", Some("timiai"));
    assert!(app.ui.image_capabilities.native_view);
    assert!(!app.ui.image_capabilities.view_fallback);
    assert!(app.ui.prompt_capabilities.image);

    app.session.shutdown();
}
