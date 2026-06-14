use super::*;

#[test]
fn codebuddy_diff_content_preserves_old_text_for_tool_card_stats() {
    let (tx, rx) = mpsc::channel();
    let old_text = "# Kodex\n\n## Project Structure\n\nbody\n";
    let new_text = "# Kodex\n\n## 项目结构\n\nbody\n";

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "claudeCode": {
                        "toolName": "Write"
                    }
                },
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-edit-1",
                "status": "completed",
                "content": [{
                    "type": "diff",
                    "path": "/workspace/README.md",
                    "oldText": old_text,
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            id,
            path,
            old_text: emitted_old_text,
            new_text: emitted_new_text,
        } => {
            assert_eq!(id, "call-edit-1");
            assert_eq!(path, "/workspace/README.md");
            assert_eq!(emitted_old_text.as_deref(), Some(old_text));
            assert_eq!(emitted_new_text, new_text);

            let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
            assert_eq!(added, vec!["## 项目结构"]);
            assert_eq!(removed, vec!["## Project Structure"]);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_path_only_content_reads_workspace_files_for_diff() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "openspec/changes/support-import-without-filename/design.md",
        "## Design\n\nUse offline CSV maps.\n",
    );
    workspace.write(
        "openspec/changes/support-import-without-filename/tasks.md",
        "## Tasks\n\n- [ ] Add parser\n",
    );

    let (tx, rx) = mpsc::channel();
    let handled = emit_codebuddy_notification(
        &tx,
        workspace.root_str(),
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-write-openspec",
                "status": "completed",
                "content": [
                    {
                        "type": "diff",
                        "path": "openspec/changes/support-import-without-filename/design.md"
                    },
                    {
                        "type": "content",
                        "content": {
                            "type": "diff",
                            "filePath": "openspec/changes/support-import-without-filename/tasks.md"
                        }
                    }
                ]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let diffs = rx
        .try_iter()
        .filter_map(|event| match event {
            ClientEvent::ToolDiff {
                id,
                path,
                old_text,
                new_text,
            } => Some((id, path, old_text, new_text)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0].0, "call-write-openspec");
    assert_eq!(
        diffs[0].1,
        "openspec/changes/support-import-without-filename/design.md"
    );
    assert_eq!(diffs[0].2, None);
    assert_eq!(diffs[0].3, "## Design\n\nUse offline CSV maps.\n");
    assert_eq!(
        diffs[1].1,
        "openspec/changes/support-import-without-filename/tasks.md"
    );
    assert_eq!(diffs[1].2, None);
    assert_eq!(diffs[1].3, "## Tasks\n\n- [ ] Add parser\n");
}

#[test]
fn codebuddy_raw_output_changes_emit_tool_diff_preview() {
    let (tx, rx) = mpsc::channel();
    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-raw-diff",
                "status": "completed",
                "title": "Edit D:\\work\\App\\src\\main.rs",
                "rawOutput": {
                    "changes": {
                        "D:\\work\\App\\src\\main.rs": {
                            "move_path": null,
                            "type": "update",
                            "unified_diff": "@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"done\");\n }\n"
                        }
                    },
                    "success": true
                }
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let previews = rx
        .try_iter()
        .filter_map(|event| match event {
            ClientEvent::ToolDiffPreview { id, path, hunks } => Some((id, path, hunks)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].0, "call-raw-diff");
    assert_eq!(previews[0].1, "D:\\work\\App\\src\\main.rs");
    assert_eq!(previews[0].2.len(), 1);
    assert_eq!(previews[0].2[0].heading, "@@ -1,3 +1,4 @@");
    let added = previews[0].2[0]
        .lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Added)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    let removed = previews[0].2[0]
        .lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Removed)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        added,
        vec!["    println!(\"new\");", "    println!(\"done\");"]
    );
    assert_eq!(removed, vec!["    println!(\"old\");"]);
}

#[test]
fn claude_write_create_raw_input_content_is_not_treated_as_old_text() {
    let (tx, rx) = mpsc::channel();
    let new_text = "export const value = 1;\n";

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "claudeCode": {
                        "toolName": "Write"
                    }
                },
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-write-create",
                "title": "Write packages/backend/scripts/migrate-vision-tags-to-structured.ts",
                "rawInput": {
                    "file_path": "/d/work/ArtAssets/src/new.ts",
                    "content": new_text
                },
                "content": [{
                    "type": "diff",
                    "path": "/d/work/ArtAssets/src/new.ts",
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            id,
            path,
            old_text,
            new_text: emitted_new_text,
        } => {
            assert_eq!(id, "call-write-create");
            assert_eq!(path, "/d/work/ArtAssets/src/new.ts");
            assert_eq!(old_text, None);
            assert_eq!(emitted_new_text, new_text);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_empty_old_text_uses_raw_input_content_for_tool_card_stats() {
    let (tx, rx) = mpsc::channel();
    let old_text = "# Kodex\n\n## Project Structure\n\nbody\n";
    let new_text = "# Kodex\n\n## 项目结构\n\nbody\n";

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-edit-1",
                "status": "completed",
                "rawInput": {
                    "path": "/workspace/README.md",
                    "before": "## Project Structure\n",
                    "after": "## 项目结构\n",
                    "content": old_text
                },
                "content": [{
                    "type": "diff",
                    "path": "/workspace/README.md",
                    "oldText": "",
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            old_text: emitted_old_text,
            new_text: emitted_new_text,
            ..
        } => {
            assert_eq!(emitted_old_text.as_deref(), Some(old_text));

            let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
            assert_eq!((added, removed), (1, 1));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_empty_old_text_can_be_reversed_from_raw_input_replacement() {
    let (tx, rx) = mpsc::channel();
    let old_text = "const value = 'old';\nconsole.log(value);\n";
    let new_text = "const value = 'new';\nconsole.log(value);\n";

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-edit-2",
                "status": "completed",
                "rawInput": {
                    "path": "/workspace/storyboard.ts",
                    "before": "const value = 'old';",
                    "after": "const value = 'new';"
                },
                "content": [{
                    "type": "diff",
                    "path": "/workspace/storyboard.ts",
                    "oldText": "",
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            old_text: emitted_old_text,
            new_text: emitted_new_text,
            ..
        } => {
            assert_eq!(emitted_old_text.as_deref(), Some(old_text));

            let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
            assert_eq!((added, removed), (1, 1));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_old_new_string_can_reconstruct_full_old_text() {
    let (tx, rx) = mpsc::channel();
    let old_fragment = "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.locator('body')).toBeVisible();\n  await page.evaluate(buildTestIdInjector());\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}";
    let new_fragment = "async function openCanvas(page: Page) {\n  await page.goto('/');\n  await expect(page.locator('body')).toBeVisible();\n  await page.evaluate(buildTestIdInjector());\n  await page.waitForFunction(() => {\n    const win = window as Window & { __smokeTagElements?: () => void };\n    win.__smokeTagElements?.();\n    return Boolean(document.querySelector('[data-testid=\"prompt-shell\"]'));\n  }, undefined, { timeout: 10_000 });\n  await expect(page.getByTestId('prompt-shell')).toBeVisible({ timeout: 10_000 });\n}";
    let old_text = format!("header\n\n{old_fragment}\n\nfooter\n");
    let new_text = format!("header\n\n{new_fragment}\n\nfooter\n");

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-edit-old-string",
                "status": "completed",
                "rawInput": {
                    "file_path": "/workspace/smokeTest/tests/app-smoke.spec.ts",
                    "old_string": old_fragment,
                    "new_string": new_fragment
                },
                "content": [{
                    "type": "diff",
                    "path": "/workspace/smokeTest/tests/app-smoke.spec.ts",
                    "oldText": "",
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            old_text: emitted_old_text,
            new_text: emitted_new_text,
            ..
        } => {
            assert_eq!(emitted_old_text.as_deref(), Some(old_text.as_str()));

            let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
            let added = hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == DiffLineKind::Added)
                .count();
            assert!(added < 20, "should not render the whole file as added");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_fragment_old_text_prefers_reconstructed_full_old_text() {
    let (tx, rx) = mpsc::channel();
    let old_text = "def helper():\n    return _UA\n\nprint(helper())\n";
    let new_text = "def helper():\n    return _ua()\n\nprint(helper())\n";

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-edit-3",
                "status": "completed",
                "rawInput": {
                    "path": "/workspace/inspiration.py",
                    "before": "_UA",
                    "after": "_ua()"
                },
                "content": [{
                    "type": "diff",
                    "path": "/workspace/inspiration.py",
                    "oldText": "_UA",
                    "newText": new_text
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolDiff {
            old_text: emitted_old_text,
            new_text: emitted_new_text,
            ..
        } => {
            assert_eq!(emitted_old_text.as_deref(), Some(old_text));

            let hunks = diff_to_hunks(emitted_old_text.as_deref(), &emitted_new_text);
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
            assert_eq!((added, removed), (1, 1));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_raw_response_todos_emit_plan_update() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "codebuddy.ai/rawResponse": {
                        "todos": [
                            {
                                "id": "1",
                                "content": "Read the code",
                                "activeForm": "Reading the code",
                                "status": "completed"
                            },
                            {
                                "id": "2",
                                "content": "Apply the fix",
                                "activeForm": "Applying the fix",
                                "status": "in_progress"
                            },
                            {
                                "id": "3",
                                "activeForm": "Verify behavior",
                                "status": "pending"
                            }
                        ]
                    }
                },
                "sessionUpdate": "tool_call_update",
                "status": "completed",
                "title": "TaskCreate",
                "toolCallId": "call-task"
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolCompleted { id, .. } if id == "call-task"
    )));
    let plan = events
        .iter()
        .find_map(|event| {
            if let ClientEvent::PlanUpdated { entries } = event {
                Some(entries)
            } else {
                None
            }
        })
        .expect("CodeBuddy todos should emit a plan update");

    assert_eq!(
        plan,
        &vec![
            AgentPlanEntry {
                id: Some("codebuddy-todo-1".into()),
                content: "Read the code".into(),
                priority: AgentPlanEntryPriority::Medium,
                status: AgentPlanEntryStatus::Completed,
            },
            AgentPlanEntry {
                id: Some("codebuddy-todo-2".into()),
                content: "Apply the fix".into(),
                priority: AgentPlanEntryPriority::Medium,
                status: AgentPlanEntryStatus::InProgress,
            },
            AgentPlanEntry {
                id: Some("codebuddy-todo-3".into()),
                content: "Verify behavior".into(),
                priority: AgentPlanEntryPriority::Medium,
                status: AgentPlanEntryStatus::Pending,
            },
        ]
    );
}

#[test]
fn codebuddy_exit_plan_mode_preserves_plan_content_in_raw_input() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "codebuddy.ai/toolName": "ExitPlanMode",
                    "codebuddy.ai/planContent": "# Plan\n\nShip the fix."
                },
                "rawInput": {
                    "allowedPrompts": []
                },
                "sessionUpdate": "tool_call",
                "title": "ExitPlanMode",
                "toolCallId": "call-exit-plan"
            }
        }),
    )
    .unwrap();

    assert!(handled);
    match rx.try_recv().unwrap() {
        ClientEvent::ToolStarted { raw_input, .. } => {
            let raw_input = raw_input.unwrap();
            assert!(raw_input.contains("# Plan"));
            assert!(raw_input.contains("allowedPrompts"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_current_mode_update_sets_local_policy_mode() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "current_mode_update",
                "currentModeId": "plan"
            }
        }),
    )
    .unwrap();

    assert!(handled);
    match rx.try_recv().unwrap() {
        ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id,
            value_label,
        } => {
            assert_eq!(control_id, "mode");
            assert_eq!(value_id, "plan");
            assert_eq!(value_label.as_deref(), Some("Plan"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn codebuddy_enter_plan_mode_tool_update_sets_local_policy_mode() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "codebuddy.ai/toolName": "EnterPlanMode"
                },
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-enter-plan",
                "status": "completed",
                "title": "EnterPlanMode",
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": "Entered plan mode."
                    }
                }]
            }
        }),
    )
    .unwrap();

    assert!(handled);
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolCompleted { id, .. } if id == "call-enter-plan"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::SessionConfigValueChanged {
            control_id,
            value_id,
            value_label,
        } if control_id == "mode"
            && value_id == "plan"
            && value_label.as_deref() == Some("Plan")
    )));
}

#[test]
fn codebuddy_task_tool_call_marks_latest_subagent_format_as_subagent() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call",
                "title": "task",
                "toolCallId": "chatcmpl-tool-1",
                "rawInput": {
                    "description": "探索项目结构和状态",
                    "prompt": "探索 D:/work/kodex",
                    "subagent_type": "explore"
                }
            }
        }),
    )
    .unwrap();

    assert!(handled);

    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolStarted {
            id,
            parent_id,
            name,
            kind,
            summary,
            is_subagent,
            raw_input,
        } => {
            assert_eq!(id, "chatcmpl-tool-1");
            assert_eq!(parent_id, None);
            assert_eq!(name, "task");
            assert_eq!(kind, "explore");
            assert_eq!(summary, "探索项目结构和状态");
            assert!(is_subagent);
            assert!(raw_input.unwrap_or_default().contains("subagent_type"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn codebuddy_task_update_emits_tool_message_chunk_from_content_text() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "chatcmpl-tool-1",
                "title": "task",
                "status": "completed",
                "rawInput": {
                    "description": "探索项目结构和状态",
                    "subagent_type": "explore"
                },
                "content": [
                    {
                        "type": "content",
                        "content": {
                            "type": "text",
                            "text": "task_id: ses_123\n\n<task_result>done</task_result>"
                        }
                    }
                ],
                "rawOutput": {
                    "output": "task_id: ses_123\n\n<task_result>done</task_result>",
                    "metadata": {
                        "sessionId": "ses_123",
                        "truncated": false
                    }
                }
            }
        }),
    )
    .unwrap();

    assert!(handled);

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolMessageChunk { id, content }
            if id == "chatcmpl-tool-1" && content.contains("<task_result>done</task_result>")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolCompleted { id, .. } if id == "chatcmpl-tool-1"
    )));
}

#[test]
fn claude_metadata_update_without_status_updates_tool_path() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "claudeCode": {
                        "toolName": "Read"
                    }
                },
                "kind": "read",
                "locations": [
                    {
                        "line": 1,
                        "path": "D:/work/ArtAssets/packages/frontend/src/utils/display.ts"
                    }
                ],
                "rawInput": {
                    "file_path": "D:/work/ArtAssets/packages/frontend/src/utils/display.ts"
                },
                "sessionUpdate": "tool_call_update",
                "title": "Read packages/frontend/src/utils/display.ts",
                "toolCallId": "tooluse_read_1"
            }
        }),
    )
    .unwrap();

    assert!(handled);

    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolUpdated {
            id,
            name,
            kind,
            summary,
            raw_input,
            is_partial,
            ..
        } => {
            assert_eq!(id, "tooluse_read_1");
            assert_eq!(
                name.as_deref(),
                Some("Read packages/frontend/src/utils/display.ts")
            );
            assert_eq!(kind.as_deref(), Some("read"));
            assert_eq!(
                summary.as_deref(),
                Some("D:/work/ArtAssets/packages/frontend/src/utils/display.ts")
            );
            assert!(
                raw_input
                    .as_deref()
                    .is_some_and(|input| input.contains("\"file_path\"")
                        && input.contains("packages/frontend/src/utils/display.ts"))
            );
            assert!(!is_partial);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn claude_tool_response_file_path_is_synthesized_as_raw_input() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "claudeCode": {
                        "toolName": "Read",
                        "toolResponse": {
                            "file": {
                                "filePath": "D:/work/ArtAssets/packages/frontend/src/pages/AssetDetailPage.tsx",
                                "numLines": 618,
                                "startLine": 1,
                                "totalLines": 618
                            },
                            "type": "text"
                        }
                    }
                },
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tooluse_read_2"
            }
        }),
    )
    .unwrap();

    assert!(handled);

    let event = rx.try_recv().unwrap();
    match event {
        ClientEvent::ToolUpdated {
            name,
            raw_input,
            is_partial,
            ..
        } => {
            assert_eq!(
                name.as_deref(),
                Some("Read D:/work/ArtAssets/packages/frontend/src/pages/AssetDetailPage.tsx")
            );
            assert!(
                raw_input
                    .as_deref()
                    .is_some_and(|input| input.contains("\"file_path\"")
                        && input.contains("AssetDetailPage.tsx"))
            );
            assert!(!is_partial);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn claude_tool_response_api_error_is_mapped_as_tool_failure() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "_meta": {
                    "claudeCode": {
                        "toolName": "Agent",
                        "toolResponse": {
                            "content": [
                                {
                                    "text": "API Error: 400 指定模型不存在，请重启 claude-internal 后重试",
                                    "type": "text"
                                }
                            ],
                            "status": "completed"
                        }
                    }
                },
                "sessionUpdate": "tool_call_update",
                "status": "completed",
                "toolCallId": "tooluse_agent_error"
            }
        }),
    )
    .unwrap();

    assert!(handled);

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolUpdated { id, raw_output, .. }
            if id == "tooluse_agent_error"
                && raw_output.as_deref().is_some_and(|output| output.contains("API Error: 400"))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolFailed { id, name, error, raw_output, .. }
            if id == "tooluse_agent_error"
                && name.as_deref() == Some("Agent")
                && error.contains("指定模型不存在")
                && raw_output.as_deref().is_some_and(|output| output.contains("API Error: 400"))
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ClientEvent::ToolCompleted { id, .. } if id == "tooluse_agent_error"
    )));
}

#[test]
fn codebuddy_whitespace_agent_chunks_are_preserved() {
    let (tx, rx) = mpsc::channel();

    let handled = emit_codebuddy_notification(
        &tx,
        "",
        &serde_json::json!({
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": "\n\n"
                }
            }
        }),
    )
    .unwrap();

    assert!(handled);
    match rx.try_recv().expect("whitespace chunk should be emitted") {
        ClientEvent::MessageChunk { role, content } => {
            assert_eq!(role, MessageRole::Assistant);
            assert_eq!(content, "\n\n");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}
