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
fn tracker_tool_diff_prefers_verified_narrower_diff_over_raw_preview() {
    let raw_preview = vec![
        DiffHunk {
            heading: "@@ -10,2 +10,2 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Removed,
                    content: "pending = partitionSidebarAssets(filteredAssets)".into(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "pending = partitionSidebarAssets(assets)".into(),
                },
            ],
        },
        DiffHunk {
            heading: "@@ -20,2 +20,2 @@".into(),
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Removed,
                    content: "readyPendingAssets = pendingAssets.filter(isReady)".into(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "readyPendingAssets = filteredAssets.filter(isReady)".into(),
                },
            ],
        },
    ];
    let verified = vec![DiffHunk {
        heading: "@@ -10,2 +10,2 @@".into(),
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Removed,
                content: "pending = partitionSidebarAssets(filteredAssets)".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "pending = partitionSidebarAssets(assets)".into(),
            },
        ],
    }];

    let hunks = tool_hunks_for_tracker_update(
        false,
        None,
        Some(raw_preview),
        None,
        Some("full file before"),
        "full file after",
        &verified,
    );

    assert_eq!(hunks, verified);
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
fn reverse_apply_unified_diff_recovers_old_file_text() {
    let after = "import A\nkeep\nnew_two\n";
    let patch = "@@ -1,3 +1,3 @@\n-import OldA\n+import A\n keep\n-old_two\n+new_two\n";

    let before = reverse_apply_unified_diff(after, patch).unwrap();

    assert_eq!(before, "import OldA\nkeep\nold_two\n");
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
fn codebuddy_python_pathlib_command_yields_written_file_hint() {
    let raw_input = serde_json::json!({
        "\"command": "python - <<'PY'\nfrom pathlib import Path\np=Path('D:/work/ArtAssets/packages/frontend/src/pages/GalleryPage.tsx')\ntext=p.read_text(encoding='utf-8')\nold='''before'''\nnew='''after'''\np.write_text(text.replace(old, new), encoding='utf-8')\nPY",
    })
    .to_string();

    let paths = tool_command_write_hint_paths(Some(&raw_input));

    assert_eq!(
        paths,
        vec!["D:/work/ArtAssets/packages/frontend/src/pages/GalleryPage.tsx"]
    );
}

#[test]
fn command_write_hint_retries_only_for_codebuddy_shell_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    app.ui.session.agent_cli = Some("Codex".into());
    let raw_input = serde_json::json!({
        "command": "python - <<'PY'\nfrom pathlib import Path\nPath('packages/frontend/src/pages/GalleryPage.tsx').write_text('after')\nPY",
    })
    .to_string();

    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "call-bash".into(),
        parent_call_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Bash".into(),
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

    assert!(!app.completed_tool_has_detectable_write_hint("call-bash"));

    app.ui.session.agent_cli = Some("CodeBuddy".into());

    assert!(app.completed_tool_has_detectable_write_hint("call-bash"));

    let tool = app
        .ui
        .tools
        .iter_mut()
        .find(|tool| tool.call_id == "call-bash")
        .unwrap();
    tool.name = "Read".into();
    tool.kind = "Read".into();

    assert!(!app.completed_tool_has_detectable_write_hint("call-bash"));
}

#[test]
fn completed_shell_write_hint_without_tool_baseline_does_not_use_git_fallback() {
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

    assert!(!app.detect_file_writes_from_tools(&["call-shell".into()]));

    assert!(app.ui.review_changes.is_empty());
    assert!(app.ui.session_changes.is_empty());
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-shell")
        .unwrap();
    assert!(tool.diff_previews.is_empty());
}

#[test]
fn completed_codebuddy_python_write_without_tool_baseline_does_not_use_git_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "packages/frontend/src/pages/GalleryPage.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "const label = 'before';\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "const label = 'after';\n").unwrap();

    let raw_input = serde_json::json!({
        "command": format!(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ntext=p.read_text(encoding='utf-8')\nold='''before'''\nnew='''after'''\np.write_text(text.replace(old, new), encoding='utf-8')\nPY",
            file_path.display().to_string().replace('\\', "/"),
        ),
    })
    .to_string();
    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "call-codebuddy-python".into(),
        parent_call_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Restyle image collection floating panel".into(),
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

    assert!(!app.detect_file_writes_from_tools(&["call-codebuddy-python".into()]));

    assert!(app.ui.review_changes.is_empty());
    assert!(app.ui.session_changes.is_empty());
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-codebuddy-python")
        .unwrap();
    assert!(tool.diff_previews.is_empty());
}

#[test]
fn completed_shell_write_with_dirty_file_uses_tool_start_baseline_for_tool_preview() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_test_git_repo(dir.path());
    let relative_path = "packages/frontend/src/pages/GalleryPage.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let git_head = [
        "const stable0 = 0;",
        "const stable1 = 1;",
        "const stable2 = 2;",
        "import { Link, useSearchParams } from 'react-router-dom';",
        "import { SEARCH_CLASSIFICATION_OPTIONS } from '../api/client.js';",
        "const stable3 = 3;",
        "const stable4 = 4;",
        "const stable5 = 5;",
        "const footer = '负向抑制';",
        "const stable6 = 6;",
        "const stable7 = 7;",
        "const stable8 = 8;",
        "",
    ]
    .join("\n");
    let before_tool = [
        "const stable0 = 0;",
        "const stable1 = 1;",
        "const stable2 = 2;",
        "import { useSearchParams } from 'react-router-dom';",
        "import { collectionQueryKeys } from '../api/client.js';",
        "const stable3 = 3;",
        "const stable4 = 4;",
        "const stable5 = 5;",
        "const footer = '负向抑制';",
        "const stable6 = 6;",
        "const stable7 = 7;",
        "const stable8 = 8;",
        "",
    ]
    .join("\n");
    let after_tool = [
        "const stable0 = 0;",
        "const stable1 = 1;",
        "const stable2 = 2;",
        "import { useSearchParams } from 'react-router-dom';",
        "import { collectionQueryKeys } from '../api/client.js';",
        "const stable3 = 3;",
        "const stable4 = 4;",
        "const stable5 = 5;",
        "const footer = '顺序优先';",
        "const stable6 = 6;",
        "const stable7 = 7;",
        "const stable8 = 8;",
        "",
    ]
    .join("\n");

    fs::write(&file_path, &git_head).unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, &before_tool).unwrap();

    let mut app = test_app(&dir);
    let raw_input = serde_json::json!({
        "command": format!(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ns=p.read_text(encoding='utf-8')\ns=s.replace('负向抑制', '顺序优先', 1)\np.write_text(s, encoding='utf-8')\nPY",
            file_path.display().to_string().replace('\\', "/"),
        ),
        "description": "Update boost footer negative copy",
    })
    .to_string();
    let hint_paths = tool_event_hint_paths(Some(&raw_input))
        .into_iter()
        .map(|path| crate::application::normalize_path_for_storage(&path, dir.path()))
        .collect::<Vec<_>>();
    assert_eq!(hint_paths, vec![relative_path.to_string()]);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolStarted {
        id: "call-python".into(),
        parent_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Update boost footer negative copy".into(),
        is_subagent: false,
        raw_input: Some(raw_input),
    }]);
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-python", relative_path),
        Some(before_tool.as_str())
    );

    fs::write(&file_path, &after_tool).unwrap();
    let tracker_hunks = diff_to_hunks(Some(&before_tool), &after_tool);
    let tracker_added = tracker_hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tracker_added, vec!["const footer = '顺序优先';"]);
    let raw_cumulative_hunks = diff_to_hunks(Some(&git_head), &after_tool);
    let result = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolDiffPreview {
            id: "call-python".into(),
            path: relative_path.into(),
            hunks: raw_cumulative_hunks,
        },
        ClientEvent::ToolCompleted {
            id: "call-python".into(),
            name: Some("Bash".into()),
            outcome: "Update boost footer negative copy".into(),
            raw_output: None,
            terminal_output: None,
        },
    ]);
    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(
        app.ui.session_changes[0].old_text.as_deref(),
        Some(before_tool.as_str())
    );

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-python")
        .unwrap();
    assert_eq!(tool.diff_previews.len(), 1);
    let preview = tool
        .diff_previews
        .iter()
        .find(|preview| preview.path == std::path::PathBuf::from(relative_path))
        .unwrap();
    let removed = preview
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    assert!(removed.contains(&"const footer = '负向抑制';"));
    assert!(!removed.contains(&"import { Link, useSearchParams } from 'react-router-dom';"));
    assert!(
        !removed.contains(&"import { SEARCH_CLASSIFICATION_OPTIONS } from '../api/client.js';")
    );
    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(
        app.ui.review_changes[0].old_text.as_deref(),
        Some(before_tool.as_str())
    );
    assert_eq!(app.ui.review_changes[0].new_text, after_tool);
}

#[test]
fn codebuddy_bash_permission_allow_records_baseline_before_terminal_write() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_test_git_repo(dir.path());
    let relative_path = "packages/frontend/src/pages/GalleryPage.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let git_head = [
        "import { Link, useSearchParams } from 'react-router-dom';",
        "const footer = '负向抑制';",
        "",
    ]
    .join("\n");
    let before_tool = [
        "import { useSearchParams } from 'react-router-dom';",
        "const footer = '负向抑制';",
        "",
    ]
    .join("\n");
    let after_tool = [
        "import { useSearchParams } from 'react-router-dom';",
        "const footer = '顺序优先';",
        "",
    ]
    .join("\n");

    fs::write(&file_path, &git_head).unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, &before_tool).unwrap();

    let mut app = test_app(&dir);
    let command = format!(
        "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ns=p.read_text(encoding='utf-8')\ns=s.replace('负向抑制', '顺序优先', 1)\np.write_text(s, encoding='utf-8')\nPY",
        file_path.display().to_string().replace('\\', "/"),
    );
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolPermissionRequest {
        id: "call-python".into(),
        name: "Bash".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "reject".into(),
                label: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some(format!("Command:\n{command}\n\nPath: {relative_path}")),
    }]);

    assert!(app.start_permission_write_baseline_if_allowed("call-python", Some("allow")));
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-python", relative_path),
        Some(before_tool.as_str())
    );

    fs::write(&file_path, &after_tool).unwrap();
    let result = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "call-python".into(),
            parent_id: None,
            name: "Bash".into(),
            kind: "Bash".into(),
            summary: "Update boost footer negative copy".into(),
            is_subagent: false,
            raw_input: None,
        },
        ClientEvent::ToolCompleted {
            id: "call-python".into(),
            name: Some("Bash".into()),
            outcome: "Update boost footer negative copy".into(),
            raw_output: None,
            terminal_output: None,
        },
    ]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(
        app.ui.session_changes[0].old_text.as_deref(),
        Some(before_tool.as_str())
    );
    assert_eq!(app.ui.session_changes[0].new_text, after_tool);

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-python")
        .unwrap();
    let preview = tool
        .diff_previews
        .iter()
        .find(|preview| preview.path == std::path::PathBuf::from(relative_path))
        .unwrap();
    let removed = preview
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    assert!(removed.contains(&"const footer = '负向抑制';"));
    assert!(!removed.contains(&"import { Link, useSearchParams } from 'react-router-dom';"));
}

#[test]
fn codebuddy_bash_permission_paths_merge_into_existing_empty_recording_window() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/frontend/src/pages/gallery/components/BoostChip.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const label = '待生效';\n").unwrap();

    let mut app = test_app(&dir);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolStarted {
        id: "call-bash".into(),
        parent_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Remove pending label".into(),
        is_subagent: false,
        raw_input: None,
    }]);

    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-bash", relative_path),
        None
    );

    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolPermissionRequest {
        id: "call-bash".into(),
        name: "Bash".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "reject".into(),
                label: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some(format!(
            "Command:\npython - <<'PY'\n...\nPY\n\nPath: {relative_path}"
        )),
    }]);

    assert!(app.start_permission_write_baseline_if_allowed("call-bash", Some("allow")));
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-bash", relative_path),
        Some("export const label = '待生效';\n")
    );

    fs::write(&file_path, "export const label = '排除';\n").unwrap();
    let result = app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolCompleted {
        id: "call-bash".into(),
        name: Some("Bash".into()),
        outcome: "Remove pending label".into(),
        raw_output: None,
        terminal_output: None,
    }]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(
        app.ui.session_changes[0].old_text.as_deref(),
        Some("export const label = '待生效';\n")
    );
    assert_eq!(
        app.ui.session_changes[0].new_text,
        "export const label = '排除';\n"
    );
}

#[test]
fn codebuddy_bash_permission_one_tool_records_two_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let boost_path = "packages/frontend/src/pages/gallery/components/BoostChip.tsx";
    let exclusion_path = "packages/frontend/src/pages/gallery/components/ExclusionChip.tsx";
    let boost_file = dir.path().join(boost_path);
    let exclusion_file = dir.path().join(exclusion_path);
    fs::create_dir_all(boost_file.parent().unwrap()).unwrap();
    fs::write(&boost_file, "export const boostLabel = '待生效';\n").unwrap();
    fs::write(&exclusion_file, "export const exclusionLabel = '待生效';\n").unwrap();

    let mut app = test_app(&dir);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolStarted {
        id: "call-bash".into(),
        parent_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Remove pending labels".into(),
        is_subagent: false,
        raw_input: None,
    }]);

    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolPermissionRequest {
        id: "call-bash".into(),
        name: "Bash".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "reject".into(),
                label: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some(format!(
            "Command:\npython - <<'PY'\n...\nPY\n\nPaths:\n- {boost_path}\n- {exclusion_path}"
        )),
    }]);

    assert!(app.start_permission_write_baseline_if_allowed("call-bash", Some("allow")));
    assert_eq!(
        app.file_tracker.get_baseline_text("call-bash", boost_path),
        Some("export const boostLabel = '待生效';\n")
    );
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-bash", exclusion_path),
        Some("export const exclusionLabel = '待生效';\n")
    );

    fs::write(&boost_file, "export const boostLabel = '排除';\n").unwrap();
    fs::write(&exclusion_file, "export const exclusionLabel = '排除';\n").unwrap();
    let result = app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolCompleted {
        id: "call-bash".into(),
        name: Some("Bash".into()),
        outcome: "Remove pending labels".into(),
        raw_output: None,
        terminal_output: None,
    }]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 2);
    assert_eq!(app.ui.review_changes.len(), 2);

    let boost_change = app
        .ui
        .session_changes
        .iter()
        .find(|change| change.path == boost_path)
        .unwrap();
    assert_eq!(
        boost_change.old_text.as_deref(),
        Some("export const boostLabel = '待生效';\n")
    );
    assert_eq!(boost_change.new_text, "export const boostLabel = '排除';\n");

    let exclusion_change = app
        .ui
        .session_changes
        .iter()
        .find(|change| change.path == exclusion_path)
        .unwrap();
    assert_eq!(
        exclusion_change.old_text.as_deref(),
        Some("export const exclusionLabel = '待生效';\n")
    );
    assert_eq!(
        exclusion_change.new_text,
        "export const exclusionLabel = '排除';\n"
    );

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "call-bash")
        .unwrap();
    assert!(
        tool.diff_previews
            .iter()
            .any(|preview| preview.path == std::path::PathBuf::from(boost_path))
    );
    assert!(
        tool.diff_previews
            .iter()
            .any(|preview| preview.path == std::path::PathBuf::from(exclusion_path))
    );
}

#[test]
fn write_like_permission_allow_records_baseline_before_tool_write() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/service.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const value = 'before';\n").unwrap();

    let mut app = test_app(&dir);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolPermissionRequest {
        id: "call-multiedit".into(),
        name: "MultiEdit".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "reject".into(),
                label: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some(format!("Path: {relative_path}")),
    }]);

    assert!(app.start_permission_write_baseline_if_allowed("call-multiedit", Some("allow")));
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-multiedit", relative_path),
        Some("export const value = 'before';\n")
    );

    fs::write(&file_path, "export const value = 'after';\n").unwrap();
    let result = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "call-multiedit".into(),
            parent_id: None,
            name: "MultiEdit".into(),
            kind: "tool".into(),
            summary: format!("Edit {relative_path}"),
            is_subagent: false,
            raw_input: None,
        },
        ClientEvent::ToolCompleted {
            id: "call-multiedit".into(),
            name: Some("MultiEdit".into()),
            outcome: format!("Edit {relative_path}"),
            raw_output: None,
            terminal_output: None,
        },
    ]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(
        app.ui.session_changes[0].old_text.as_deref(),
        Some("export const value = 'before';\n")
    );
    assert_eq!(
        app.ui.session_changes[0].new_text,
        "export const value = 'after';\n"
    );
}

#[test]
fn read_permission_with_path_does_not_record_write_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/service.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const value = 'before';\n").unwrap();

    let mut app = test_app(&dir);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolPermissionRequest {
        id: "call-read".into(),
        name: "Read".into(),
        options: vec![
            workspace_model::PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
                kind: "AllowOnce".into(),
            },
            workspace_model::PermissionOption {
                id: "reject".into(),
                label: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        details: Some(format!("Path: {relative_path}")),
    }]);

    assert!(!app.start_permission_write_baseline_if_allowed("call-read", Some("allow")));
    assert_eq!(
        app.file_tracker
            .get_baseline_text("call-read", relative_path),
        None
    );
}

#[test]
fn codebuddy_bash_write_updates_existing_session_change() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/frontend/src/pages/gallery/components/BoostChip.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const label = 'old';\n").unwrap();

    let mut app = test_app(&dir);
    app.ui.session.agent_cli = Some("CodeBuddy".into());
    app.apply_tracker_changes(
        "previous-edit",
        vec![crate::file_tracker::VerifiedFileChange {
            path: relative_path.into(),
            change_type: FileChangeType::Modified,
            old_text: Some("export const label = 'old';\n".into()),
            new_text: "export const label = 'middle';\n".into(),
            skipped_diff: false,
            quality: DiffQuality::Exact,
        }],
    );
    fs::write(&file_path, "export const label = 'middle';\n").unwrap();

    let raw_input = serde_json::json!({
        "command": format!(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ns=p.read_text(encoding='utf-8')\np.write_text(s.replace('middle', 'new'), encoding='utf-8')\nPY",
            file_path.display().to_string().replace('\\', "/"),
        ),
    })
    .to_string();

    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolStarted {
        id: "current-bash".into(),
        parent_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Update chip label".into(),
        is_subagent: false,
        raw_input: Some(raw_input),
    }]);
    app.file_tracker.discard_recording("current-bash");

    fs::write(&file_path, "export const label = 'new';\n").unwrap();
    let result = app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolCompleted {
        id: "current-bash".into(),
        name: Some("Bash".into()),
        outcome: "Update chip label".into(),
        raw_output: None,
        terminal_output: None,
    }]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(
        app.ui.session_changes[0].old_text.as_deref(),
        Some("export const label = 'old';\n")
    );
    assert_eq!(
        app.ui.session_changes[0].new_text,
        "export const label = 'new';\n"
    );

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "current-bash")
        .unwrap();
    let preview = tool
        .diff_previews
        .iter()
        .find(|preview| preview.path == std::path::PathBuf::from(relative_path))
        .unwrap();
    let removed = preview
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    let added = preview
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(removed, vec!["export const label = 'middle';"]);
    assert_eq!(added, vec!["export const label = 'new';"]);
}

#[test]
fn late_completed_command_raw_input_retries_until_file_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "packages/backend/src/services/query-understanding/sanitize.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const keepAnchorTerm = false;\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);

    let raw_input = serde_json::json!({
        "command": format!(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ntext=p.read_text(encoding='utf-8')\np.write_text(text.replace('false', 'true'), encoding='utf-8')\nPY",
            file_path.display().to_string().replace('\\', "/"),
        ),
    })
    .to_string();

    let completed = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "write-sanitize".into(),
            parent_id: None,
            name: "Bash".into(),
            kind: "Bash".into(),
            summary: "Filter non-anchor expansions before dedupe".into(),
            is_subagent: false,
            raw_input: None,
        },
        ClientEvent::ToolCompleted {
            id: "write-sanitize".into(),
            name: Some("Bash".into()),
            outcome: "Filter non-anchor expansions before dedupe".into(),
            raw_output: None,
            terminal_output: None,
        },
    ]);
    assert!(!completed.had_file_changes);

    let late_update = app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolUpdated {
        id: "write-sanitize".into(),
        parent_id: None,
        name: Some("Bash".into()),
        kind: Some("Bash".into()),
        summary: None,
        is_subagent: false,
        raw_input: Some(raw_input),
        raw_output: None,
        terminal_output: None,
        is_partial: false,
    }]);
    assert!(!late_update.had_file_changes);

    fs::write(&file_path, "export const keepAnchorTerm = true;\n").unwrap();
    app.advance_runtime_clock(Duration::from_millis(250));
    assert!(app.retry_pending_tool_write_detections());

    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, relative_path);
    assert_eq!(
        app.ui.review_changes[0].old_text.as_deref(),
        Some("export const keepAnchorTerm = false;\n")
    );
    assert_eq!(
        app.ui.review_changes[0].new_text,
        "export const keepAnchorTerm = true;\n"
    );
    assert_eq!(app.ui.session_changes.len(), 1);
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "write-sanitize")
        .unwrap();
    assert!(
        tool.diff_previews
            .iter()
            .any(|preview| !preview.hunks.is_empty())
    );
}

#[test]
fn completed_command_with_known_path_retries_with_tool_start_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "packages/backend/src/services/query-understanding/sanitize.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const keepAnchorTerm = 'head';\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "export const keepAnchorTerm = 'dirty';\n").unwrap();

    let raw_input = serde_json::json!({
        "command": format!(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('{}')\ntext=p.read_text(encoding='utf-8')\np.write_text(text.replace('dirty', 'after'), encoding='utf-8')\nPY",
            file_path.display().to_string().replace('\\', "/"),
        ),
    })
    .to_string();

    let completed = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "write-sanitize".into(),
            parent_id: None,
            name: "Bash".into(),
            kind: "Bash".into(),
            summary: "Filter non-anchor expansions before dedupe".into(),
            is_subagent: false,
            raw_input: Some(raw_input),
        },
        ClientEvent::ToolCompleted {
            id: "write-sanitize".into(),
            name: Some("Bash".into()),
            outcome: "Filter non-anchor expansions before dedupe".into(),
            raw_output: None,
            terminal_output: None,
        },
    ]);
    assert!(!completed.had_file_changes);

    fs::write(&file_path, "export const keepAnchorTerm = 'after';\n").unwrap();
    app.advance_runtime_clock(Duration::from_millis(250));
    assert!(app.retry_pending_tool_write_detections());

    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, relative_path);
    assert_eq!(
        app.ui.review_changes[0].old_text.as_deref(),
        Some("export const keepAnchorTerm = 'dirty';\n")
    );
    assert_eq!(
        app.ui.review_changes[0].new_text,
        "export const keepAnchorTerm = 'after';\n"
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
fn completed_tool_preview_does_not_claim_preexisting_git_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "src/main.rs";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn main() {}\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "fn main() { println!(\"dirty\"); }\n").unwrap();
    let preview_hunks = diff_to_hunks(
        Some("fn main() {}\n"),
        "fn main() { println!(\"dirty\"); }\n",
    );

    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "preview-main".into(),
        parent_call_id: None,
        name: "Bash".into(),
        kind: "Bash".into(),
        summary: "Completed".into(),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: vec![relative_path.into()],
        diff_previews: vec![ToolDiffPreview {
            path: relative_path.into(),
            hunks: preview_hunks,
        }],
        raw_input: None,
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(!app.detect_file_writes_from_tools(&["preview-main".into()]));
    assert!(app.ui.review_changes.is_empty());
    assert!(app.ui.session_changes.is_empty());
}

#[test]
fn completed_chinese_edit_summary_without_tool_baseline_does_not_use_git_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let repo = init_test_git_repo(dir.path());
    let relative_path = "openspec/changes/tag-system-revamp/proposal.md";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "before\n").unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, "after\n").unwrap();

    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "edit-proposal".into(),
        parent_call_id: None,
        name: "Edit".into(),
        kind: "edit".into(),
        summary: format!("已编辑 {relative_path}"),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: None,
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(!app.detect_file_writes_from_tools(&["edit-proposal".into()]));
    assert!(app.ui.review_changes.is_empty());
    assert!(app.ui.session_changes.is_empty());
}

#[test]
fn write_create_without_old_text_enters_review_as_created() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let normalized_path = "packages/backend/scripts/migrate-vision-tags-to-structured.ts";
    let new_text = "export const migrated = true;\n";

    let change_type = app.tool_diff_change_type("write-create", normalized_path, None);
    app.upsert_review_file_change(normalized_path, change_type.clone(), None, new_text.into());

    assert_eq!(change_type, FileChangeType::Created);
    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, normalized_path);
    assert_eq!(
        app.ui.review_changes[0].change_type,
        FileChangeType::Created
    );
    assert_eq!(app.ui.review_changes[0].old_text, None);
    assert_eq!(app.ui.review_changes[0].new_text, new_text);
}

#[test]
fn failed_tool_without_recorded_file_change_discards_speculative_diff() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&dir);
    let change = SessionFileChange {
        path: "docs/tags.md".into(),
        change_type: FileChangeType::Modified,
        old_text: Some("before\n".into()),
        new_text: "after\n".into(),
        added_lines: 1,
        removed_lines: 1,
        timestamp: "2026-05-21T00:00:00Z".into(),
    };

    app.ui.session_changes = vec![change.clone()];
    app.ui.review_changes = vec![change];
    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "write-tags".into(),
        parent_call_id: None,
        name: "Write".into(),
        kind: "Write".into(),
        summary: "Write docs/tags.md".into(),
        status: ToolStatus::Failed,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: vec![PathBuf::from("docs/tags.md")],
        diff_previews: vec![ToolDiffPreview {
            path: PathBuf::from("docs/tags.md"),
            hunks: vec![DiffHunk {
                heading: "@@ -1 +1 @@".into(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        content: "before".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "after".into(),
                    },
                ],
            }],
        }],
        raw_input: Some(
            serde_json::json!({
                "file_path": "/d/work/ArtAssets/docs/tags.md",
            })
            .to_string(),
        ),
        raw_output: None,
        terminal_output: None,
        error: Some("User refused permission to run tool".into()),
        permission_options: Vec::new(),
        permission_decision: Some("Reject".into()),
    });

    assert!(app.discard_failed_tool_speculative_diffs("write-tags"));
    assert!(app.ui.session_changes.is_empty());
    assert!(app.ui.review_changes.is_empty());
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "write-tags")
        .unwrap();
    assert!(tool.diff_paths.is_empty());
    assert!(tool.diff_previews.is_empty());
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

#[test]
fn late_codebuddy_unified_diff_recovers_turn_base_when_git_diff_is_smaller() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_test_git_repo(dir.path());
    let relative_path = "src/main.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let git_head = "import A\nkeep\nold_two\n";
    let before_turn = "import OldA\nkeep\nold_two\n";
    let after_turn = "import A\nkeep\nnew_two\n";
    fs::write(&file_path, git_head).unwrap();
    commit_paths(&repo, &[".gitignore", relative_path]);
    fs::write(&file_path, before_turn).unwrap();

    let mut app = test_app(&dir);
    fs::write(&file_path, after_turn).unwrap();

    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "edit-main".into(),
        parent_call_id: None,
        name: "Edit".into(),
        kind: "Edit".into(),
        summary: format!("Edited {relative_path}"),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: Vec::new(),
        diff_previews: Vec::new(),
        raw_input: Some(
            serde_json::json!({
                "changes": {
                    "src/main.ts": {
                        "type": "update",
                        "move_path": null,
                        "unified_diff": "@@ -1,3 +1,3 @@\n-import OldA\n+import A\n keep\n-old_two\n+new_two\n"
                    }
                }
            })
            .to_string(),
        ),
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(app.detect_file_writes_from_tools(&["edit-main".into()]));

    assert_eq!(app.ui.review_changes.len(), 1);
    assert_eq!(app.ui.review_changes[0].path, relative_path);
    assert_eq!(
        app.ui.review_changes[0].old_text.as_deref(),
        Some(before_turn)
    );
    assert_eq!(app.ui.review_changes[0].new_text, after_turn);
    assert_eq!(
        (
            app.ui.review_changes[0].added_lines,
            app.ui.review_changes[0].removed_lines,
        ),
        (2, 2)
    );
}

#[test]
fn review_uses_landed_tool_preview_instead_of_narrow_exact_edit() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/frontend/src/pages/GalleryPage.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let before = [
        "const query = search.trim();",
        "{!isOnline && query.trim() && (",
        "  <span>old offline helper</span>",
        ")}",
        "const filler = 1;",
        "const more = 2;",
        "{boosts.length < 5 && (",
        "  <button>Add boost</button>",
        ")}",
        "",
    ]
    .join("\n");
    let after = [
        "const query = search.trim();",
        "{!isOnline && (",
        "  <span>new offline helper</span>",
        ")}",
        "const filler = 1;",
        "const more = 2;",
        "{boosts.length < 5 && query.trim() && (",
        "  <button>Add boost</button>",
        ")}",
        "",
    ]
    .join("\n");
    fs::write(&file_path, &after).unwrap();

    let mut app = test_app(&dir);
    let landed_hunks = diff_to_hunks(Some(&before), &after);
    app.ui.tools.push(ToolInvocation {
        id: uuid::Uuid::new_v4(),
        call_id: "edit-gallery".into(),
        parent_call_id: None,
        name: "Edit".into(),
        kind: "Edit".into(),
        summary: format!("Edited {relative_path}"),
        status: ToolStatus::Succeeded,
        is_subagent: false,
        detail_text: String::new(),
        logs: Vec::new(),
        diff_paths: vec![relative_path.into()],
        diff_previews: vec![ToolDiffPreview {
            path: relative_path.into(),
            hunks: landed_hunks.clone(),
        }],
        raw_input: Some(
            serde_json::json!({
                "file_path": relative_path,
                "old_string": "{!isOnline && query.trim() && (",
                "new_string": "{!isOnline && (",
            })
            .to_string(),
        ),
        raw_output: None,
        terminal_output: None,
        error: None,
        permission_options: Vec::new(),
        permission_decision: None,
    });

    assert!(app.apply_tracker_changes(
        "edit-gallery",
        vec![crate::file_tracker::VerifiedFileChange {
            path: relative_path.into(),
            change_type: FileChangeType::Modified,
            old_text: Some(before.clone()),
            new_text: after.clone(),
            skipped_diff: false,
            quality: DiffQuality::Exact,
        }],
    ));

    assert_eq!(app.ui.review_changes.len(), 1);
    let review_change = &app.ui.review_changes[0];
    assert_eq!(review_change.path, relative_path);
    assert_eq!(review_change.old_text.as_deref(), Some(before.as_str()));
    assert_eq!(review_change.new_text, after);
    assert_eq!(
        (review_change.added_lines, review_change.removed_lines),
        (3, 3)
    );

    let tool_hunks = &app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "edit-gallery")
        .unwrap()
        .diff_previews[0]
        .hunks;
    let tool_changed_lines = tool_hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed))
        .count();
    assert_eq!(tool_changed_lines, 6);
}

#[test]
fn raw_output_diff_preview_populates_turn_change_set_with_all_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "frontend/src/components/layout/TopBar.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let before = [
        "type Props = {",
        "  targetSimilaritySort?: TargetSimilaritySort",
        "  completionSort?: boolean",
        "  showArmourGroups?: boolean",
        "}",
        "",
        "function TopBar(props: Props) {",
        "  const {",
        "    targetSimilaritySort,",
        "    completionSort,",
        "    showArmourGroups,",
        "    onCycleTargetSimilaritySort,",
        "    onToggleCompletionSort,",
        "    onToggleArmourGroups,",
        "  } = props",
        "",
        "  return (",
        "    <button>相似度</button>",
        "    {onToggleCompletionSort ? <button>完成顺序</button> : null}",
        "  )",
        "}",
        "",
    ]
    .join("\n");
    let after = [
        "type Props = {",
        "  targetSimilaritySort?: TargetSimilaritySort",
        "  showArmourGroups?: boolean",
        "}",
        "",
        "function TopBar(props: Props) {",
        "  const {",
        "    targetSimilaritySort,",
        "    showArmourGroups,",
        "    onCycleTargetSimilaritySort,",
        "    onToggleArmourGroups,",
        "  } = props",
        "",
        "  return (",
        "    <button>相似度</button>",
        "  )",
        "}",
        "",
    ]
    .join("\n");
    let mut app = test_app(&dir);
    let hunks = diff_to_hunks(Some(&before), &after);
    fs::write(&file_path, &before).unwrap();
    app.file_tracker
        .start_recording("edit-topbar", vec![relative_path.into()]);
    fs::write(&file_path, &after).unwrap();
    app.file_tracker
        .add_candidate("edit-topbar", relative_path.into());

    assert!(app.tool_diff_preview_matches_recording_window("edit-topbar", relative_path, &hunks));
    assert!(app.apply_tool_diff_preview_to_review(relative_path, relative_path, &hunks));
    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.old_text.as_deref(), Some(before.as_str()));
    assert_eq!(change.new_text, after);
    assert_eq!(change.added_lines, 0);
    assert_eq!(change.removed_lines, 4);
}

#[test]
fn raw_output_diff_preview_replaces_same_path_content_fragments_in_review() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "datas/scripts/eagle-export.py";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let before = [
        "import json",
        "import sys",
        "",
        "",
        "def main():",
        "    combined_tags = []",
        "",
        "    if combined_tags:",
        "        print(combined_tags)",
        "",
    ]
    .join("\n");
    let after = [
        "import json",
        "import sys",
        "from urllib.parse import urlparse",
        "",
        "",
        "def detect_source_tag(url: str) -> str | None:",
        "    if not url:",
        "        return None",
        "    return f\"source:{urlparse(url).netloc}\"",
        "",
        "",
        "def main():",
        "    combined_tags = []",
        "",
        "    source_tag = detect_source_tag('https://example.com/a.png')",
        "    if source_tag:",
        "        combined_tags.append(source_tag)",
        "",
        "    if combined_tags:",
        "        print(combined_tags)",
        "",
    ]
    .join("\n");

    let mut app = test_app(&dir);
    fs::write(&file_path, &before).unwrap();
    app.file_tracker
        .start_recording("edit-eagle", vec![relative_path.into()]);
    fs::write(&file_path, &after).unwrap();

    let full_hunks = diff_to_hunks(Some(&before), &after);
    let full_added = full_hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();

    app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolDiff {
            id: "edit-eagle".into(),
            path: relative_path.into(),
            old_text: Some("import json\nimport sys\n\n".into()),
            new_text: "import json\nimport sys\nfrom urllib.parse import urlparse\n\n".into(),
        },
        ClientEvent::ToolDiff {
            id: "edit-eagle".into(),
            path: relative_path.into(),
            old_text: Some("\ndef main():\n".into()),
            new_text: "\ndef detect_source_tag(url: str) -> str | None:\n    if not url:\n        return None\n    return f\"source:{urlparse(url).netloc}\"\n\n\ndef main():\n".into(),
        },
        ClientEvent::ToolDiff {
            id: "edit-eagle".into(),
            path: relative_path.into(),
            old_text: Some("\n    if combined_tags:\n".into()),
            new_text: "\n    source_tag = detect_source_tag('https://example.com/a.png')\n    if source_tag:\n        combined_tags.append(source_tag)\n\n    if combined_tags:\n".into(),
        },
        ClientEvent::ToolDiffPreview {
            id: "edit-eagle".into(),
            path: relative_path.into(),
            hunks: full_hunks.clone(),
        },
    ]);

    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.old_text.as_deref(), Some(before.as_str()));
    assert_eq!(change.new_text, after);
    assert_eq!(change.added_lines, full_added);
    assert!(change.added_lines > 1);
}

#[test]
fn raw_output_diff_preview_without_tool_start_baseline_stays_out_of_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let unrelated_path = "frontend/src/components/layout/TopBar.tsx";
    let hinted_path = "frontend/src/components/layout/Sidebar.tsx";
    let unrelated_file = dir.path().join(unrelated_path);
    let hinted_file = dir.path().join(hinted_path);
    fs::create_dir_all(unrelated_file.parent().unwrap()).unwrap();

    let before = "export const label = 'before'\n";
    let after = "export const label = 'after'\n";
    fs::write(&unrelated_file, before).unwrap();
    fs::write(&hinted_file, "export const sidebar = true\n").unwrap();

    let mut app = test_app(&dir);
    app.file_tracker
        .start_recording("edit-sidebar", vec![hinted_path.into()]);

    fs::write(&unrelated_file, after).unwrap();
    let hunks = diff_to_hunks(Some(before), after);

    // This simulates CodeBuddy exposing a raw_output changes entry only after the
    // file has already been written. It is still useful on the tool card, but it
    // must not be trusted as this tool's review/change-set diff without a
    // pre-tool baseline for the path.
    app.file_tracker
        .add_candidate("edit-sidebar", unrelated_path.into());

    assert!(!app.tool_diff_preview_matches_recording_window(
        "edit-sidebar",
        unrelated_path,
        &hunks
    ));
    if app.tool_diff_preview_matches_recording_window("edit-sidebar", unrelated_path, &hunks) {
        app.apply_tool_diff_preview_to_review(unrelated_path, unrelated_path, &hunks);
    }
    assert!(app.ui.review_changes.is_empty());
}

#[test]
fn late_tool_diff_preview_for_landed_session_change_populates_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "frontend/src/components/layout/TopBar.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let before = "export const label = 'before'\n";
    let after = "export const label = 'after'\n";
    fs::write(&file_path, after).unwrap();

    let mut app = test_app(&dir);
    app.ui.session_changes = vec![SessionFileChange {
        path: relative_path.into(),
        change_type: FileChangeType::Modified,
        old_text: Some(before.into()),
        new_text: after.into(),
        added_lines: 1,
        removed_lines: 1,
        timestamp: current_timestamp(),
    }];

    let hunks = diff_to_hunks(Some(before), after);
    app.apply_runtime_events_with_file_tracking(vec![ClientEvent::ToolDiffPreview {
        id: "late-edit".into(),
        path: relative_path.into(),
        hunks: hunks.clone(),
    }]);

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "late-edit")
        .unwrap();
    assert_eq!(tool.diff_previews.len(), 1);
    assert_eq!(tool.diff_previews[0].hunks, hunks);

    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.change_type, FileChangeType::Modified);
    assert_eq!(change.old_text.as_deref(), Some(before));
    assert_eq!(change.new_text, after);
    assert_eq!(change.added_lines, 1);
    assert_eq!(change.removed_lines, 1);
}

#[test]
fn idle_late_created_file_preview_persists_completed_turn_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/services/query-understanding/schema.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    let new_text = [
        "export type QueryUnderstanding = {",
        "  subject: string",
        "  confidence: number",
        "}",
        "",
    ]
    .join("\n");
    fs::write(&file_path, &new_text).unwrap();

    let mut app = test_app(&dir);
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "add query understanding files".into(),
        created_at: "2026-06-04T00:00:00Z".into(),
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-06-04T00:00:01Z".into(),
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = None;

    let hunks = vec![DiffHunk {
        heading: "@@ -0,0 +1,4 @@".into(),
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                content: "export type QueryUnderstanding = {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "  subject: string".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "  confidence: number".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "}".into(),
            },
        ],
    }];

    let result =
        app.apply_idle_runtime_events_with_file_tracking(vec![ClientEvent::ToolDiffPreview {
            id: "late-schema".into(),
            path: relative_path.into(),
            hunks,
        }]);

    assert!(result.had_file_changes);
    assert_eq!(app.current_turn_user_message_id, None);
    assert_eq!(app.ui.review_changes.len(), 1);

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
        .expect("late preview should be attached to the completed assistant turn");
    assert_eq!(turn.status, ChangeSetStatus::Complete);
    assert_eq!(turn.file_count, 1);
    assert_eq!(turn.added_lines, 4);
    assert_eq!(turn.removed_lines, 0);

    let diff = app
        .store
        .load_change_set_file_diff(&turn.id, relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(diff.path, relative_path);
    assert_eq!(diff.change_type, FileChangeType::Created);
    assert_eq!(diff.old_text, None);
    assert_eq!(diff.new_text.as_deref(), Some(new_text.as_str()));
    assert_eq!(diff.added_lines, 4);
    assert_eq!(diff.removed_lines, 0);
}

#[test]
fn preview_before_file_create_retries_and_enters_review_after_landing() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/services/query-understanding/prompt.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let user_id = uuid::Uuid::new_v4();
    let assistant_id = uuid::Uuid::new_v4();
    let new_text = [
        "export const queryUnderstandingPrompt = [",
        "  'Extract the subject from the user query.',",
        "].join('\\n')",
        "",
    ]
    .join("\n");

    let mut app = test_app(&dir);
    app.set_runtime_clock_now(std::time::Instant::now());
    app.ui.messages.clear();
    app.ui.timeline.clear();
    app.ui.messages.push(ChatMessage {
        id: user_id,
        role: MessageRole::User,
        body: "add query understanding files".into(),
        created_at: "2026-06-04T00:00:00Z".into(),
    });
    app.ui.messages.push(ChatMessage {
        id: assistant_id,
        role: MessageRole::Assistant,
        body: "done".into(),
        created_at: "2026-06-04T00:00:01Z".into(),
    });
    app.ui.timeline.push(TimelineItem::Message(user_id));
    app.ui.timeline.push(TimelineItem::Message(assistant_id));
    app.current_turn_user_message_id = None;

    let hunks = vec![DiffHunk {
        heading: "@@ -0,0 +1,3 @@".into(),
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                content: "export const queryUnderstandingPrompt = [".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "  'Extract the subject from the user query.',".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "].join('\\n')".into(),
            },
        ],
    }];

    let result =
        app.apply_idle_runtime_events_with_file_tracking(vec![ClientEvent::ToolDiffPreview {
            id: "late-prompt".into(),
            path: relative_path.into(),
            hunks,
        }]);
    assert!(!result.had_file_changes);
    assert!(app.ui.review_changes.is_empty());
    assert!(
        app.store
            .list_change_sets(
                Some(&app.ui.session.id.to_string()),
                Some(ChangeSetSource::AgentTurn),
            )
            .unwrap()
            .is_empty()
    );

    fs::write(&file_path, &new_text).unwrap();
    app.advance_runtime_clock(Duration::from_secs(1));

    assert!(app.retry_pending_tool_diff_previews());

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
        .expect("landed preview should be attached to the completed assistant turn");
    assert_eq!(turn.status, ChangeSetStatus::Complete);
    assert_eq!(turn.file_count, 1);
    assert_eq!(turn.added_lines, 3);
    assert_eq!(turn.removed_lines, 0);

    let diff = app
        .store
        .load_change_set_file_diff(&turn.id, relative_path)
        .unwrap()
        .unwrap();
    assert_eq!(diff.change_type, FileChangeType::Created);
    assert_eq!(diff.old_text, None);
    assert_eq!(diff.new_text.as_deref(), Some(new_text.as_str()));
}

#[test]
fn raw_output_created_file_preview_with_late_baseline_enters_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "frontend/src/components/gallery/NewCard.tsx";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let new_text = [
        "export function NewCard() {",
        "  return <article>new</article>",
        "}",
        "",
    ]
    .join("\n");
    fs::write(&file_path, &new_text).unwrap();

    let mut app = test_app(&dir);
    app.file_tracker
        .start_recording("write-new-card", vec![relative_path.into()]);
    let hunks = vec![DiffHunk {
        heading: "@@ -0,0 +1,3 @@".into(),
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Added,
                content: "export function NewCard() {".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "  return <article>new</article>".into(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                content: "}".into(),
            },
        ],
    }];

    assert!(app.tool_diff_preview_matches_recording_window(
        "write-new-card",
        relative_path,
        &hunks
    ));
    assert!(app.apply_tool_diff_preview_to_review(relative_path, relative_path, &hunks));

    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.change_type, FileChangeType::Created);
    assert_eq!(change.old_text, None);
    assert_eq!(change.new_text, new_text);
    assert_eq!(change.added_lines, 3);
    assert_eq!(change.removed_lines, 0);
}

#[test]
fn late_write_tool_diff_without_recoverable_baseline_still_shows_tool_preview() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/services/query-understanding/index.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "export const before = true;\n").unwrap();

    let mut app = test_app(&dir);
    let new_text = [
        "import { config } from '../../config/index.js';",
        "export function analyzeQueryUnderstanding() {",
        "  return config.SYNONYM_MODEL;",
        "}",
        "",
    ]
    .join("\n");
    fs::write(&file_path, &new_text).unwrap();

    let result = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "write-index".into(),
            parent_id: None,
            name: "Write".into(),
            kind: "edit".into(),
            summary: format!("Write {relative_path}"),
            is_subagent: false,
            raw_input: Some(
                serde_json::json!({
                    "file_path": relative_path,
                    "content": new_text,
                })
                .to_string(),
            ),
        },
        ClientEvent::ToolDiff {
            id: "write-index".into(),
            path: relative_path.into(),
            old_text: None,
            new_text: new_text.clone(),
        },
        ClientEvent::ToolCompleted {
            id: "write-index".into(),
            name: Some("Write".into()),
            outcome: format!("Successfully wrote to file: {relative_path}"),
            raw_output: None,
            terminal_output: None,
        },
    ]);

    assert!(result.had_file_changes);
    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "write-index")
        .unwrap();
    assert_eq!(tool.diff_previews.len(), 1);
    let added = tool.diff_previews[0]
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == DiffLineKind::Added)
        .count();
    assert_eq!(added, 4);

    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.change_type, FileChangeType::Created);
    assert_eq!(change.old_text, None);
    assert_eq!(change.new_text, new_text);
    assert_eq!(change.added_lines, 4);
}

#[test]
fn completed_write_raw_input_content_without_acp_diff_enters_review_changes() {
    let dir = tempfile::tempdir().unwrap();
    let relative_path = "packages/backend/src/services/query-understanding/schema.ts";
    let file_path = dir.path().join(relative_path);
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    let new_text = [
        "export interface QueryUnderstanding {",
        "  subject: string;",
        "  confidence: number;",
        "}",
        "",
    ]
    .join("\n");
    fs::write(&file_path, &new_text).unwrap();

    let mut app = test_app(&dir);
    let result = app.apply_runtime_events_with_file_tracking(vec![
        ClientEvent::ToolStarted {
            id: "write-schema".into(),
            parent_id: None,
            name: "Write".into(),
            kind: "edit".into(),
            summary: format!("Write {relative_path}"),
            is_subagent: false,
            raw_input: Some(
                serde_json::json!({
                    "file_path": relative_path,
                    "content": new_text,
                })
                .to_string(),
            ),
        },
        ClientEvent::ToolCompleted {
            id: "write-schema".into(),
            name: Some("Write".into()),
            outcome: format!("Successfully wrote to file: {relative_path}"),
            raw_output: None,
            terminal_output: None,
        },
    ]);

    assert!(result.had_file_changes);
    assert_eq!(app.ui.session_changes.len(), 1);
    assert_eq!(app.ui.review_changes.len(), 1);
    let change = &app.ui.review_changes[0];
    assert_eq!(change.path, relative_path);
    assert_eq!(change.change_type, FileChangeType::Created);
    assert_eq!(change.old_text, None);
    assert_eq!(change.new_text, new_text);
    assert_eq!(change.added_lines, 4);

    let tool = app
        .ui
        .tools
        .iter()
        .find(|tool| tool.call_id == "write-schema")
        .unwrap();
    assert_eq!(tool.diff_previews.len(), 1);
}
