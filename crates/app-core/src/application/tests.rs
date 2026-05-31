use super::diff_utils::{
    canonical_text_diff, edit_input_after_text, edit_input_before_text,
    expand_tool_diff_fragment_from_disk, is_file_write_tool_identity,
    is_trustworthy_review_change_text, looks_like_fragment_to_full_file_text,
    looks_like_whole_file_addition_hunks, reverse_apply_unified_diff,
    sanitize_session_file_changes, tool_diff_hunks, tool_diff_hunks_for_tracker_change,
    tool_event_hint_paths, tool_hunks_for_tracker_update,
};
use super::inline_think::InlineThinkFilter;
use super::titles::is_placeholder_session_title;
use super::{Application, current_timestamp, humanize_acp_disconnect_reason, turn_finished_notice};
use acp_core::{ClientEvent, diff_to_hunks};
use std::{collections::HashMap, fs, path::PathBuf, time::Duration};
use workspace_model::{
    ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind, DiffQuality,
    FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest, ListChangeSetsRequest,
    MessageRole, SessionAttentionState, SessionConfigCategory, SessionConfigChoice,
    SessionConfigControl, SessionConfigSource, SessionConfigState, SessionFileChange,
    SessionRuntimeStatus, TimelineItem, ToolDiffPreview, ToolInvocation, ToolStatus,
    TurnFileChanges,
};

mod change_set_tests;
mod diff_tests;
mod prompt_tests;

fn init_test_git_repo(path: &std::path::Path) -> git2::Repository {
    let repo = git2::Repository::init(path).unwrap();
    fs::write(path.join(".gitignore"), "home/\n").unwrap();
    repo
}

fn commit_paths(repo: &git2::Repository, paths: &[&str]) {
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(std::path::Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let parent_oid = repo.head().ok().and_then(|head| head.target());
    let parent_commit = parent_oid.and_then(|oid| repo.find_commit(oid).ok());
    let parents = parent_commit.into_iter().collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parent_refs)
        .unwrap();
}

fn test_app(dir: &tempfile::TempDir) -> Application {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("app-core should live under crates/app-core")
        .join("Cargo.toml");
    let manifest = manifest.display().to_string().replace('\\', "/");
    Application::bootstrap_with_app_paths(
        dir.path(),
        format!(
            "cargo run --manifest-path {} -p mock-acp-agent --quiet --",
            manifest
        ),
        crate::paths::AppPaths::from_root(dir.path().join("home").join(".kodex")),
    )
    .unwrap()
}

#[test]
fn stale_model_config_refresh_preserves_user_selected_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);

    app.ui.session_config = model_config_state(
        SessionConfigSource::ConfigOption,
        "gpt-5.4",
        &["gpt-5.4", "gpt-5.5"],
    );
    app.ui.session.model = "gpt-5.4".into();
    app.authoritative_model_selection = Some("gpt-5.4".into());

    app.apply_event_and_restore_model(ClientEvent::SessionConfigUpdated {
        state: model_config_state(
            SessionConfigSource::ConfigOption,
            "gpt-5.5",
            &["gpt-5.4", "gpt-5.5"],
        ),
    });

    assert_eq!(app.ui.session.model, "gpt-5.4");
    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("model control should exist");
    assert_eq!(model_control.current_value_id, "gpt-5.4");
}

#[test]
fn live_runtime_reuse_avoids_session_load_when_switching_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let first_session_id = app.ui.session.id.to_string();

    app.session_create(None).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);
    let second_session_id = app.ui.session.id.to_string();

    app.session_switch(&first_session_id).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);

    assert_eq!(app.ui.session.id.to_string(), first_session_id);
    assert_ne!(first_session_id, second_session_id);
    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("live runtime should retain session/new model state");
    assert!(
        model_control
            .choices
            .iter()
            .all(|choice| choice.id != "mock-loaded"),
        "reusing the live runtime must not run session/load"
    );
}

#[test]
fn switched_away_in_flight_prompt_completes_under_original_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("finish while hidden").unwrap();
    app.session_create(None).unwrap();
    let visible_session_id = app.ui.session.id.to_string();

    let running = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        running.runtime_status,
        SessionRuntimeStatus::BackgroundRunning
    );

    wait_for_session_attention(
        &mut app,
        &background_session_id,
        SessionAttentionState::CompletedUnviewed,
    );

    assert_eq!(app.ui.session.id.to_string(), visible_session_id);
    let (messages, tools, _) = app.store.load_session(&background_session_id).unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::User
                && message.body.contains("finish while hidden"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant
                && message.body.contains("Real ACP session connected"))
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.status == ToolStatus::Succeeded)
    );
}

#[test]
fn background_idle_runtime_retires_and_reopens_with_session_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.set_runtime_clock_now(std::time::Instant::now());
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("retire after completion")
        .unwrap();
    app.session_create(None).unwrap();
    wait_for_session_attention(
        &mut app,
        &background_session_id,
        SessionAttentionState::CompletedUnviewed,
    );

    app.advance_runtime_clock(Duration::from_secs(10 * 60 + 1));
    app.poll_prompt_progress();

    assert!(
        !app.runtime_registry
            .entries
            .contains_key(&background_session_id),
        "idle background runtime should be reclaimed"
    );
    let retired = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(retired.runtime_status, SessionRuntimeStatus::None);
    assert_eq!(
        retired.attention_state,
        SessionAttentionState::CompletedUnviewed
    );

    app.session_switch(&background_session_id).unwrap();
    wait_for_control(&mut app, SessionConfigCategory::Model);

    let model_control = app
        .ui
        .session_config
        .controls
        .iter()
        .find(|control| control.category == SessionConfigCategory::Model)
        .expect("session/load should hydrate model state after retirement");
    assert!(
        model_control
            .choices
            .iter()
            .any(|choice| choice.id == "mock-loaded"),
        "retired runtime should restore through session/load"
    );
    let reopened = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(reopened.attention_state, SessionAttentionState::None);
}

#[test]
fn cancel_visible_session_does_not_cancel_background_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.send_prompt_background("keep running").unwrap();
    app.session_create(None).unwrap();
    app.cancel_prompt().unwrap();

    let background = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        background.runtime_status,
        SessionRuntimeStatus::BackgroundRunning
    );
}

#[test]
fn background_permission_marks_only_owning_session_as_needing_attention() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let background_session_id = app.ui.session.id.to_string();

    app.session_create(None).unwrap();
    let visible_session_id = app.ui.session.id.to_string();
    let mut runtime = app
        .runtime_registry
        .remove(&background_session_id)
        .expect("previous session should be backgrounded");

    app.swap_visible_state_with_runtime(&mut runtime);
    app.apply_event_with_dirty_tracking(&ClientEvent::ToolPermissionRequest {
        id: "permission-1".into(),
        name: "Read".into(),
        options: vec![],
        details: None,
    });
    let needs_attention = app.runtime_needs_attention();
    app.swap_visible_state_with_runtime(&mut runtime);
    runtime.attention_state = if needs_attention {
        SessionAttentionState::NeedsAttention
    } else {
        SessionAttentionState::None
    };
    runtime.runtime_status = SessionRuntimeStatus::BackgroundRunning;
    app.runtime_registry.insert(runtime);

    assert_eq!(app.ui.session.id.to_string(), visible_session_id);
    assert!(
        app.ui
            .tools
            .iter()
            .all(|tool| tool.call_id != "permission-1")
    );
    let background = app
        .session_list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == background_session_id)
        .unwrap();
    assert_eq!(
        background.attention_state,
        SessionAttentionState::NeedsAttention
    );
}

fn model_config_state(
    source: SessionConfigSource,
    current: &str,
    choices: &[&str],
) -> SessionConfigState {
    SessionConfigState {
        hydrated: true,
        controls: vec![SessionConfigControl {
            id: "model".into(),
            label: "Model".into(),
            description: None,
            category: SessionConfigCategory::Model,
            source,
            current_value_id: current.into(),
            current_value_label: current.into(),
            choices: choices
                .iter()
                .map(|choice| SessionConfigChoice {
                    id: (*choice).into(),
                    label: (*choice).into(),
                    description: None,
                })
                .collect(),
            enabled: true,
        }],
    }
}

fn wait_for_control(app: &mut Application, category: SessionConfigCategory) {
    for _ in 0..100 {
        app.poll_prompt_progress();
        if app
            .ui
            .session_config
            .controls
            .iter()
            .any(|control| control.category == category)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn wait_for_session_attention(
    app: &mut Application,
    session_id: &str,
    expected: SessionAttentionState,
) {
    for _ in 0..100 {
        app.poll_prompt_progress();
        if app
            .session_list()
            .unwrap()
            .into_iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.attention_state == expected)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("session {session_id} did not reach attention state {expected:?}");
}
