//! Generated-image injection into the conversation timeline.
//!
//! `generate_image` / `edit_image` results are more than a tool card: the
//! assistant message carries the picture inline (`![…](data:image/…;base64,…)`)
//! so it renders in the timeline and survives the file being deleted. These
//! tests cover *where* that result text arrives from, because the two agent
//! channels disagree: assistant channels hand it over as `raw_output`, while the
//! DeepSeek Harness renders a generic tool card and parks the same JSON in
//! `terminal_output`. Reading only `raw_output` is why a dsh session could
//! generate an image and show nothing.

use super::*;

/// A `ToolCompleted` carrying a `generate_image` result in one result channel.
fn generated_image_completion(
    dir: &std::path::Path,
    carry_in_raw_output: bool,
    carry_in_terminal_output: bool,
) -> ClientEvent {
    let path = dir.join("generated.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n generated").unwrap();
    let payload = format!(
        "{{\"images\":[{{\"saved_path\":{:?},\"mime_type\":\"image/png\"}}],\"saved_dir\":{:?}}}",
        path.to_string_lossy(),
        dir.to_string_lossy()
    );
    ClientEvent::ToolCompleted {
        id: "call-generated".into(),
        name: Some("mcp__kodex_image__generate_image".into()),
        outcome: "completed".into(),
        raw_output: carry_in_raw_output.then(|| payload.clone()),
        terminal_output: carry_in_terminal_output.then(|| workspace_model::TerminalOutput {
            exit_code: Some(0),
            output: payload,
        }),
    }
}

fn injected_images(app: &Application) -> Vec<&str> {
    app.ui
        .messages
        .iter()
        .filter(|message| message.body.contains("data:image/png;base64,"))
        .map(|message| message.body.as_str())
        .collect()
}

#[test]
fn raw_output_image_result_is_rendered_in_the_timeline() {
    // Assistant channels (codex-acp / kodex-claude) put the MCP JSON here.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&generated_image_completion(dir.path(), true, false));

    let injected = injected_images(&app);
    assert_eq!(injected.len(), 1, "the picture must reach the conversation");
    assert!(injected[0].starts_with("![生成的图片](data:image/png;base64,"));
    app.session.shutdown();
}

#[test]
fn terminal_output_image_result_is_rendered_in_the_timeline() {
    // The dsh bridge maps a generic tool card's text into `terminal_output`
    // (`ToolResultView::Terminal`), leaving `raw_output` empty.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.apply_event_with_dirty_tracking(&generated_image_completion(dir.path(), false, true));

    let injected = injected_images(&app);
    assert_eq!(
        injected.len(),
        1,
        "dsh image results must reach the conversation too"
    );
    assert!(injected[0].starts_with("![生成的图片](data:image/png;base64,"));
    app.session.shutdown();
}

#[test]
fn a_replayed_completion_does_not_stack_a_second_picture() {
    // dsh re-baselines the follow stream and can re-deliver a completion.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let event = generated_image_completion(dir.path(), false, true);

    app.apply_event_with_dirty_tracking(&event);
    app.apply_event_with_dirty_tracking(&event);

    assert_eq!(injected_images(&app).len(), 1);
    app.session.shutdown();
}

#[test]
fn non_image_tool_output_injects_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    let event = ClientEvent::ToolCompleted {
        id: "call-plain".into(),
        name: Some("read".into()),
        outcome: "completed".into(),
        raw_output: Some("{\"files\":[\"a.rs\"]}".into()),
        terminal_output: Some(workspace_model::TerminalOutput {
            exit_code: Some(0),
            output: "ok".into(),
        }),
    };
    app.apply_event_with_dirty_tracking(&event);

    assert!(injected_images(&app).is_empty());
    app.session.shutdown();
}
