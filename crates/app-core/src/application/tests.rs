use super::diff_utils::{
    canonical_text_diff, edit_input_after_text, edit_input_before_text,
    expand_tool_diff_fragment_from_disk, is_file_write_tool_identity,
    is_trustworthy_review_change_text, looks_like_fragment_to_full_file_text,
    looks_like_whole_file_addition_hunks, sanitize_session_file_changes, tool_diff_hunks,
    tool_diff_hunks_for_tracker_change, tool_event_hint_paths, tool_hunks_for_tracker_update,
};
use super::inline_think::InlineThinkFilter;
use super::titles::is_placeholder_session_title;
use super::{Application, current_timestamp, turn_finished_notice};
use acp_core::{ClientEvent, diff_to_hunks};
use std::{collections::HashMap, fs};
use workspace_model::{
    ChangeSetSource, ChangeSetStatus, ChatMessage, DiffHunk, DiffLine, DiffLineKind, DiffQuality,
    FileChangeType, GetChangeSetFileDiffRequest, ListChangeSetFilesRequest, ListChangeSetsRequest,
    MessageRole, SessionFileChange, TimelineItem, ToolInvocation, ToolStatus, TurnFileChanges,
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
