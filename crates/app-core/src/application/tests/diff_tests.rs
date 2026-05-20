use super::*;

#[test]
fn tool_diff_uses_previous_session_new_text_for_repeated_file_edits() {
    let hunks = tool_diff_hunks(Some("one\ntwo\n"), Some("one\n"), "one\nthree\n");
    let added = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    let removed = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();

    assert_eq!(added, vec!["three"]);
    assert_eq!(removed, vec!["two"]);
}

#[test]
fn tracker_tool_diff_prefers_tool_start_baseline_over_session_new_text() {
    let hunks =
        tool_diff_hunks_for_tracker_change(Some("one\ntwo\n"), Some("one\ntwo\n"), "one\nthree\n");
    let added = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    let removed = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();

    assert_eq!(added, vec!["three"]);
    assert_eq!(removed, vec!["two"]);
}

#[test]
fn tracker_tool_diff_preserves_existing_acp_preview() {
    let existing = vec![DiffHunk {
        heading: "@@ -1,1 +1,1 @@".into(),
        lines: vec![DiffLine {
            kind: DiffLineKind::Added,
            content: "'react-refresh/only-export-components': [".into(),
        }],
    }];
    let tracker_full_file = vec![DiffHunk {
        heading: "@@ -0,0 +1,27 @@".into(),
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                content: "module.exports = {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "  root: true,".into(),
            },
        ],
    }];

    let hunks = tool_hunks_for_tracker_update(
        false,
        None,
        Some(existing.clone()),
        None,
        None,
        "module.exports = {\n  root: true,\n",
        &tracker_full_file,
    );

    assert_eq!(hunks, existing);
}

#[test]
fn tracker_tool_diff_prefers_codebuddy_old_new_string_exact_hunks() {
    let input = serde_json::json!({
        "file_path": "D:/work/InfiniteCanvasOL/smokeTest/tests/app-smoke.spec.ts",
        "old_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}",
        "new_string": "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await page.waitForFunction(() => Boolean(document.querySelector('[data-testid=\"prompt-shell\"]')), undefined, { timeout: 10_000 });\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}"
    });
    let exact = diff_to_hunks(
        edit_input_before_text(&input),
        edit_input_after_text(&input).unwrap(),
    );
    let tracker_full_file = vec![DiffHunk {
        heading: "@@ -0,0 +1,847 @@".into(),
        lines: vec![DiffLine {
            kind: DiffLineKind::Added,
            content: "import { test, expect, Page, TestInfo } from '@playwright/test';".into(),
        }],
    }];

    let hunks = tool_hunks_for_tracker_update(
        false,
        Some(exact.clone()),
        None,
        None,
        None,
        "full file content",
        &tracker_full_file,
    );

    assert_eq!(hunks, exact);
    let added = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    assert_eq!(added, 1);
}

#[test]
fn tracker_tool_diff_rejects_fragment_to_full_file_existing_preview() {
    let bad_existing = vec![DiffHunk {
        heading: "@@ -1,3 +1,901 @@".into(),
        lines: (1..=901)
            .map(|line| DiffLine {
                kind: DiffLineKind::Added,
                content: format!("line {line}"),
            })
            .collect(),
    }];
    let full_old = (1..=901)
        .map(|line| {
            if line == 42 {
                "old target".to_string()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let full_new = full_old.replace("old target", "new target\nextra target");

    let hunks = tool_hunks_for_tracker_update(
        false,
        None,
        Some(bad_existing),
        None,
        Some(&full_old),
        &full_new,
        &[],
    );

    let added = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    let removed = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count();
    assert_eq!(added, 2);
    assert_eq!(removed, 1);
}

#[test]
fn fragment_to_full_file_text_is_not_trusted_as_exact_edit() {
    let old_fragment = "function target() {\n  return 1;\n}\n";
    let new_whole_file = (1..=901)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(looks_like_fragment_to_full_file_text(
        old_fragment,
        &new_whole_file
    ));
}

#[test]
fn canonical_diff_counts_from_same_base_and_target_pair() {
    let diff = canonical_text_diff(
        &FileChangeType::Modified,
        Some("alpha\r\nold\r\nomega\r\n"),
        Some("alpha\nnew\nextra\nomega\n"),
        None,
    );

    assert_eq!(diff.quality, DiffQuality::Exact);
    assert_eq!(diff.old_text.as_deref(), Some("alpha\nold\nomega\n"));
    assert_eq!(diff.new_text.as_deref(), Some("alpha\nnew\nextra\nomega\n"));
    assert_eq!(diff.added_lines, 2);
    assert_eq!(diff.removed_lines, 1);
    let counted_added = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    let counted_removed = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count();
    assert_eq!(diff.added_lines, counted_added);
    assert_eq!(diff.removed_lines, counted_removed);
}

#[test]
fn canonical_diff_records_quality_for_unavailable_inputs() {
    let missing = canonical_text_diff(
        &FileChangeType::Modified,
        None,
        Some("new whole file\n"),
        None,
    );
    assert_eq!(missing.quality, DiffQuality::MissingBaseline);
    assert_eq!(missing.added_lines, 0);
    assert!(missing.hunks.is_empty());

    let fragment = canonical_text_diff(
        &FileChangeType::Modified,
        Some("tiny fragment\n"),
        Some(
            &(1..=300)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        None,
    );
    assert_eq!(fragment.quality, DiffQuality::FragmentRejected);
    assert_eq!(fragment.added_lines, 0);

    let binary = canonical_text_diff(
        &FileChangeType::Modified,
        Some("old\n"),
        None,
        Some(DiffQuality::BinarySkipped),
    );
    assert_eq!(binary.quality, DiffQuality::BinarySkipped);
    assert!(binary.hunks.is_empty());
}

#[test]
fn file_record_keeps_fragment_rejection_instead_of_full_file_stats() {
    let new_whole_file = (1..=400)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let change = SessionFileChange {
        path: "src/settings.rs".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("const model = \"gpt-5.5\";\n".into()),
        new_text: new_whole_file,
        added_lines: 400,
        removed_lines: 1,
        timestamp: "1".into(),
    };

    let record = Application::file_record_from_session_change("change-set-1", &change)
        .expect("fragment rejection is still a reviewable record");

    assert_eq!(record.quality, DiffQuality::FragmentRejected);
    assert_eq!(record.added_lines, 0);
    assert_eq!(record.removed_lines, 0);
    assert_eq!(record.path, "src/settings.rs");
}

#[test]
fn canonical_deleted_file_diff_keeps_target_absent_but_counts_removed_lines() {
    let diff = canonical_text_diff(
        &FileChangeType::Deleted,
        Some("one\ntwo\nthree\n"),
        None,
        None,
    );

    assert_eq!(diff.quality, DiffQuality::Exact);
    assert_eq!(diff.old_text.as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(diff.new_text, None);
    assert_eq!(diff.added_lines, 0);
    assert_eq!(diff.removed_lines, 3);
}

#[test]
fn review_change_rejects_fragment_old_text_against_full_file_text() {
    let old_fragment = "const model = \"gpt-5.5\";\n";
    let new_whole_file = (1..=1609)
        .map(|line| {
            if line == 700 {
                "const model = \"gpt-5.5\";".to_string()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!is_trustworthy_review_change_text(
        &FileChangeType::Modified,
        Some(old_fragment),
        &new_whole_file,
    ));
}

#[test]
fn tool_diff_fragment_expands_to_full_file_snapshot_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("render_node_registry.py");
    let target = "\
import ipaddress

def normalize_reported_render_ip(value: str) -> str:
    parsed = ipaddress.ip_address(value.strip())
    parsed_ip = str(parsed)
    if not parsed.is_private or parsed_ip.startswith(\"9.134.\"):
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return parsed_ip

def other():
    return None
";
    fs::write(&path, target).unwrap();

    let (base, expanded_target) = expand_tool_diff_fragment_from_disk(
        &path,
        Some(
            "\
    if not parsed.is_private:
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return str(parsed)
",
        ),
        "\
    parsed_ip = str(parsed)
    if not parsed.is_private or parsed_ip.startswith(\"9.134.\"):
        raise ValueError(\"ip must be a private LAN IPv4 address\")
    return parsed_ip
",
    )
    .expect("fragment should expand against target file");

    assert_eq!(expanded_target, target);
    assert!(base.contains("return str(parsed)"));
    assert!(base.starts_with("import ipaddress\n\n"));

    let hunks = diff_to_hunks(Some(&base), &expanded_target);
    let first_changed_line = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .position(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
        .expect("diff should contain a changed line");
    assert!(
        first_changed_line > 1,
        "expanded full-file diff should keep leading file context before the edit",
    );
}

#[test]
fn sanitizer_drops_persisted_fragment_to_full_file_change() {
    let mut changes = vec![SessionFileChange {
        path: "crates/app-core/src/settings.rs".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("old line\n".into()),
        new_text: (1..=1609)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        added_lines: 1609,
        removed_lines: 1,
        timestamp: "1".into(),
    }];

    assert!(sanitize_session_file_changes(&mut changes));
    assert!(changes.is_empty());
}

#[test]
fn whole_file_addition_hunks_are_not_preserved_as_existing_preview() {
    let hunks = vec![DiffHunk {
        heading: "@@ -1,3 +1,901 @@".into(),
        lines: (1..=901)
            .map(|line| DiffLine {
                kind: DiffLineKind::Added,
                content: format!("line {line}"),
            })
            .collect(),
    }];

    assert!(looks_like_whole_file_addition_hunks(&hunks));
}

#[test]
fn tracker_tool_diff_without_any_baseline_does_not_render_full_file() {
    let hunks = tool_hunks_for_tracker_update(
        false,
        None,
        None,
        None,
        None,
        "module.exports = {\n  root: true,\n",
        &[],
    );

    assert!(hunks.is_empty());
}

#[test]
fn write_tool_detection_does_not_match_editor_paths() {
    assert!(!is_file_write_tool_identity(
        "read",
        "docs\\editor-subsystem-design.md"
    ));
    assert!(!is_file_write_tool_identity(
        "read",
        "D:/work/kodex/docs/editor-subsystem-design.md"
    ));
    assert!(is_file_write_tool_identity("edit", "docs/architecture.md"));
    assert!(is_file_write_tool_identity("tool", "mcp__codebuddy__write"));
}

#[test]
fn codex_powershell_command_array_yields_written_file_hint() {
    let raw_input = serde_json::json!({
            "call_id": "call_1",
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                "if (-not (Test-Path \"docs\")) { New-Item -ItemType Directory -Path \"docs\" -Force | Out-Null }; $guideContent = @\"\n# Guide\n\nSet-Content -Path \"fake.md\"\n\"@; Set-Content -Path \"docs/windows-guide.md\" -Value $guideContent -Encoding UTF8"
            ],
        })
        .to_string();

    let paths = tool_event_hint_paths(Some(&raw_input));

    assert_eq!(paths, vec!["docs/windows-guide.md"]);
}

#[test]
fn powershell_positional_set_content_yields_written_file_hint() {
    let raw_input = serde_json::json!({
            "call_id": "call_1",
            "command": [
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "-Command",
                "$lines = Get-Content \"D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts\"; $lines[0] = 'after'; Set-Content \"D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts\" $lines"
            ],
        })
        .to_string();

    let paths = tool_event_hint_paths(Some(&raw_input));

    assert_eq!(
        paths,
        vec!["D:\\work\\InfiniteCanvasOL\\smokeTest\\tests\\app-smoke.spec.ts"]
    );
}

#[test]
fn completed_shell_write_hint_enters_review_via_git_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "smokeTest/tests/app-smoke.spec.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "before\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "after\n").unwrap();

    let raw_input = serde_json::json!({
        "call_id": "call-shell",
        "command": [
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "-Command",
            format!(
                "$lines = Get-Content \"{}\"; $lines[0] = 'after'; Set-Content \"{}\" $lines",
                file_path.display(),
                file_path.display(),
            ),
        ],
        "cwd": dir.path().display().to_string(),
    })
    .to_string();
    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "call-shell".into(),
        parent_call_id: None,
        name: "Shell".into(),
        kind: "Shell".into(),
        summary: "Shell".into(),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: Some(raw_input),
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(app.detect_file_writes_from_tools(&["call-shell".into()]));

    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, relative_path);
    assert_eq!(
        app.ui.review_changes[0].old_text.as_deref(),
        Some("before\n")
    );
    assert_eq!(app.ui.review_changes[0].new_text, "after\n");
    assert_eq!(app.ui.session_changes.len(), 1);
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-shell")
        .unwrap();
    assert!(
        tool.diff_previews
            .iter()
            .any(|preview| !preview.hunks.is_empty())
    );
}

#[test]
fn completed_read_tool_does_not_claim_preexisting_git_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "src/main.rs";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn main() {}\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "fn main() {\n    println!(\"dirty\");\n}\n").unwrap();

    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "read-main".into(),
        parent_call_id: None,
        name: "Read".into(),
        kind: "Read".into(),
        summary: "Read src/main.rs".into(),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: Some(
            serde_json::json!({
                "file_path": relative_path,
            })
            .to_string(),
        ),
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(!app.detect_file_writes_from_tools(&["read-main".into()]));
    assert!(app.ui.review_changes.is_empty());
    assert!(app.ui.session_changes.is_empty());
}

#[test]
fn missing_tool_diff_old_text_uses_session_target_as_turn_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.ui.session_changes = vec![SessionFileChange {
        path: "src/main.rs".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("session base\n".into()),
        new_text: "before this turn\n".into(),
        added_lines: 1,
        removed_lines: 1,
        timestamp: "2026-05-13T00:00:00Z".into(),
    }];

    let baseline = app.tool_diff_baseline_text(
        "call-1",
        "src/main.rs",
        "after this turn\n",
        &HashMap::new(),
    );

    assert_eq!(baseline.as_deref(), Some("before this turn\n"));
}

#[test]
fn late_codebuddy_edit_prefers_exact_turn_diff_over_git_cumulative_diff() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_test_git_repo(dir.path());
    fs::create_dir_all(dir.path().join(".ci/scripts/remote")).unwrap();

    let old_fragment = "npm ci";
    let new_fragment =
        "if npm ci; then\n  echo installed\nelse\n  tail -n 200 vite-preview.log >&2\n  exit 1\nfi";
    let head_text = "set -e\nnpm ci\nnpm run preview\n";
    fs::write(
        dir.path().join(".ci/scripts/remote/frontend_preview.sh"),
        head_text,
    )
    .unwrap();
    commit_paths(
        &repo,
        &[".gitignore", ".ci/scripts/remote/frontend_preview.sh"],
    );

    let preexisting_lines = (1..=70)
        .map(|line| format!("echo preexisting-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let before_turn = format!("set -e\n{preexisting_lines}\n{old_fragment}\nnpm run preview\n");
    let after_turn = before_turn.replacen(old_fragment, new_fragment, 1);
    fs::write(
        dir.path().join(".ci/scripts/remote/frontend_preview.sh"),
        &after_turn,
    )
    .unwrap();

    let mut app = test_app(&dir);
    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    app.current_turn_user_message_id = Some(user_id);
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "fix deploy preview".into(),
        created_at: "1".into(),
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2".into(),
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "edit-frontend-preview".into(),
        parent_call_id: None,
        name: "Edit".into(),
        kind: "Edit".into(),
        summary: "Editing .ci/scripts/remote/frontend_preview.sh".into(),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: Some(
            serde_json::json!({
                "file_path": ".ci/scripts/remote/frontend_preview.sh",
                "old_string": old_fragment,
                "new_string": new_fragment,
            })
            .to_string(),
        ),
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(app.detect_file_writes_from_tools(&["edit-frontend-preview".into()]));
    assert!(app.persist_current_turn_file_changes());

    let turns = app
        .store
        .list_change_sets(
            Some(&app.ui.session.id.to_string()),
            Some(ChangeSetSource::AgentTurn),
        )
        .unwrap();
    let turn = turns
        .iter()
        .find(|summary| summary.message_id == Some(assistant_id))
        .unwrap();
    let diff = app
        .store
        .load_change_set_file_diff(turn.id.as_str(), ".ci/scripts/remote/frontend_preview.sh")
        .unwrap()
        .unwrap();

    assert_eq!(
        (diff.added_lines, diff.removed_lines),
        (new_fragment.lines().count(), old_fragment.lines().count())
    );
    assert_eq!(diff.old_text.as_deref(), Some(before_turn.as_str()));
    assert_eq!(diff.new_text.as_deref(), Some(after_turn.as_str()));
}
