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
use std::{collections::HashMap, fs, path::PathBuf};
use workspace_model::{
    ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind, DiffQuality,
    FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest, ListChangeSetsRequest,
    MessageRole, SessionConfigCategory, SessionConfigChoice, SessionConfigControl,
    SessionConfigSource, SessionConfigState, SessionFileChange, TimelineItem, ToolDiffPreview,
    ToolInvocation, ToolStatus, TurnFileChanges,
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
