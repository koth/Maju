use super::*;
use agent_client_protocol::schema::{
    PermissionOption, PermissionOptionKind, RequestPermissionRequest, SessionId, ToolCallLocation,
    ToolCallUpdate, ToolCallUpdateFields,
};
use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn permission_broker_maps_codex_approval_presets_to_policy_modes() {
    let broker = PermissionBroker::default();

    for mode_id in ["build", "auto", "default", "acceptEdits"] {
        broker.set_mode(mode_id).unwrap();
        assert_eq!(broker.mode(), PermissionPolicyMode::Build, "{mode_id}");
    }

    for mode_id in ["full-access", "bypassPermissions", "完全访问"] {
        broker.set_mode(mode_id).unwrap();
        assert_eq!(broker.mode(), PermissionPolicyMode::FullAccess, "{mode_id}");
    }

    for mode_id in ["plan", "read-only"] {
        broker.set_mode(mode_id).unwrap();
        assert_eq!(broker.mode(), PermissionPolicyMode::ReadOnly, "{mode_id}");
    }
}

fn switch_mode_request() -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "exit-plan",
            ToolCallUpdateFields::new()
                .kind(ToolKind::SwitchMode)
                .title("Ready to code?".to_string()),
        ),
        vec![
            PermissionOption::new("default", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new("plan", "No", PermissionOptionKind::RejectOnce),
        ],
    )
}

fn execute_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "shell",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Execute)
                .title("Shell".to_string())
                .raw_input(raw_input),
        ),
        vec![
            PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
        ],
    )
}

fn read_request(path: &str) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "read",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Read)
                .title("Read file".to_string())
                .locations(vec![ToolCallLocation::new(path)]),
        ),
        vec![
            PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
        ],
    )
}

fn edit_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "edit",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Edit)
                .title("Edit".to_string())
                .raw_input(raw_input),
        ),
        vec![
            PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
        ],
    )
}

fn edit_request_with_location(path: &str) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "edit",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Edit)
                .title("Edit".to_string())
                .locations(vec![ToolCallLocation::new(path)]),
        ),
        vec![
            PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
        ],
    )
}

fn codex_apply_patch_approval_request(path: &str) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "call_patch",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Edit)
                .title("Edit file".to_string())
                .locations(vec![ToolCallLocation::new(path)])
                .raw_input(json!({
                    "call_id": "call_patch",
                    "changes": [
                        { "path": path }
                    ]
                })),
        ),
        vec![
            PermissionOption::new("approved", "Yes", PermissionOptionKind::AllowOnce),
            PermissionOption::new(
                "abort",
                "No, provide feedback",
                PermissionOptionKind::RejectOnce,
            ),
        ],
    )
}

fn user_input_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "ask-user",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Other)
                .title("Ask user".to_string())
                .raw_input(raw_input),
        ),
        vec![
            PermissionOption::new(
                "ask_user_question:0:0",
                "Fast",
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                "ask_user_question:0:1",
                "Robust",
                PermissionOptionKind::AllowOnce,
            ),
        ],
    )
}

fn codebuddy_bash_request(raw_input: serde_json::Value) -> RequestPermissionRequest {
    let mut payload = serde_json::to_value(execute_request(raw_input)).unwrap();
    let tool_call_key = if payload.get("toolCall").is_some() {
        "toolCall"
    } else {
        "tool_call"
    };
    let tool_call = payload
        .get_mut(tool_call_key)
        .and_then(serde_json::Value::as_object_mut)
        .expect("request should serialize a tool call object");
    tool_call.insert(
        "_meta".into(),
        json!({
            "codebuddy.ai/toolName": "Bash"
        }),
    );
    serde_json::from_value(payload).unwrap()
}

fn execute_request_with_permission_options(
    raw_input: serde_json::Value,
    options: Vec<PermissionOption>,
) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("session-1"),
        ToolCallUpdate::new(
            "shell",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Execute)
                .title("Shell".to_string())
                .raw_input(raw_input),
        ),
        options,
    )
}

fn temp_workspace(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be valid")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("kodex-permissions-{name}-{nanos}"));
    fs::create_dir_all(root.join("packages/backend/src")).expect("workspace should be created");
    root
}

#[test]
fn switch_mode_permission_is_always_interactive() {
    let request = switch_mode_request();

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert_eq!(
        decide_permission(PermissionPolicyMode::ReadOnly, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn apply_patch_policy_rejects_patchable_direct_shell_writes_with_guidance() {
    let root = temp_workspace("apply-patch-policy");
    let request = execute_request(json!({
        "command": "echo ok > packages/backend/src/service.ts"
    }));

    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::Build,
            AgentEditPolicy::PreferApplyPatch,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::SelectWithGuidance(
            "reject".to_string(),
            apply_patch_retry_guidance().to_string(),
        ),
    );
}

#[test]
fn apply_patch_policy_rejects_patchable_direct_edit_tools_with_guidance() {
    let root = temp_workspace("apply-patch-edit-policy");
    let request = edit_request(json!({
        "path": "packages/backend/src/service.ts"
    }));

    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::Build,
            AgentEditPolicy::PreferApplyPatch,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::SelectWithGuidance(
            "reject".to_string(),
            apply_patch_retry_guidance().to_string(),
        ),
    );
}

#[test]
fn apply_patch_policy_allows_codex_apply_patch_approval() {
    let root = temp_workspace("codex-apply-patch-approval");
    let request = codex_apply_patch_approval_request("packages/backend/src/service.ts");

    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::Build,
            AgentEditPolicy::PreferApplyPatch,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::Select("approved".to_string()),
    );

    let _ = fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn full_access_overrides_apply_patch_policy_interception() {
    let root = temp_workspace("apply-patch-full-access");
    let request = execute_request(json!({
        "command": "echo ok > packages/backend/src/service.ts"
    }));

    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::FullAccess,
            AgentEditPolicy::PreferApplyPatch,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::Ask,
    );
}

#[test]
fn apply_patch_policy_keeps_lockfiles_and_formatters_out_of_patch_gate() {
    let lockfile_request = execute_request(json!({
        "command": "python3 -c \"open('package-lock.json', 'w', encoding='utf-8').write('ok')\""
    }));
    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::Build,
            AgentEditPolicy::PreferApplyPatch,
            "D:/work/repo",
            &lockfile_request,
        ),
        PermissionDecision::Ask,
    );

    assert!(!shell_command_prefers_apply_patch_for_writes(
        "D:/work/repo",
        "prettier --write packages/backend/src/service.ts",
    ));
}

#[test]
fn default_edit_policy_preserves_existing_direct_write_behavior() {
    let request = execute_request(json!({
        "command": "python3 -c \"open('packages/backend/src/service.ts', 'w', encoding='utf-8').write('ok')\""
    }));

    assert_eq!(
        decide_permission_with_edit_policy(
            PermissionPolicyMode::Build,
            AgentEditPolicy::None,
            "D:/work/repo",
            &request,
        ),
        PermissionDecision::Ask,
    );
}

#[test]
fn user_input_questions_are_always_interactive() {
    let request = user_input_request(json!({
        "questions": [
            {
                "id": "approach",
                "header": "Approach",
                "question": "Which implementation approach should I use?",
                "options": [
                    { "label": "Fast", "description": "Smallest viable change" },
                    { "label": "Robust", "description": "Add tests and validation" }
                ]
            }
        ]
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert_eq!(
        decide_permission(PermissionPolicyMode::ReadOnly, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn reject_permission_option_prefers_once_over_always() {
    let request = execute_request_with_permission_options(
        json!({ "command": "python -c \"open('src/main.rs','w').write('x')\"" }),
        vec![
            PermissionOption::new(
                "reject_always",
                "No, always",
                PermissionOptionKind::RejectAlways,
            ),
            PermissionOption::new("reject", "No", PermissionOptionKind::RejectOnce),
            PermissionOption::new("allow", "Yes", PermissionOptionKind::AllowOnce),
        ],
    );

    assert_eq!(
        reject_permission_option_id(&request).as_deref(),
        Some("reject")
    );
}

#[test]
fn build_permission_asks_for_shell_redirection_file_writes() {
    let request = execute_request(json!({
        "command": "cat > AGENTS.md << 'ENDOFFILE'\n# Guidelines\nENDOFFILE"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_asks_for_powershell_file_writes() {
    let request = execute_request(json!({
        "command": [
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "-Command",
            "Set-Content -Path AGENTS.md -Value '# Guidelines'"
        ]
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_asks_for_powershell_remove_item() {
    let request = execute_request(json!({
        "command": "Remove-Item README.md -Force"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_asks_for_python_open_file_writes() {
    let request = execute_request(json!({
        "command": "python3 -c \"open('packages/backend/src/service.ts', 'w', encoding='utf-8').write('ok')\""
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_asks_for_python_open_dynamic_file_writes() {
    let request = execute_request(json!({
        "command": "python3 -c \"path='packages/backend/src/service.ts'; open(path, 'w', encoding='utf-8').write('ok')\""
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_allows_shell_reads_and_apply_patch_wrappers() {
    let read_request = execute_request(json!({ "command": "rg -n \"TODO\" src 2>/dev/null" }));
    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &read_request),
        PermissionDecision::Select("allow".to_string()),
    );

    let patch_request = execute_request(json!({
        "command": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\nPATCH"
    }));
    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &patch_request),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn build_permission_allows_remote_workspace_read_paths() {
    let request = read_request("/g/kknovel/text_chunker/convert_to_onnx.py");

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "/g/kknovel", &request),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn build_permission_allows_workspace_edit_paths() {
    let request = edit_request_with_location("packages/backend/src/service.ts");

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn build_permission_asks_for_workspace_edit_without_paths() {
    let request = edit_request(json!({}));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn build_permission_asks_for_outside_workspace_edit_paths() {
    let request = edit_request_with_location("D:/outside/service.ts");

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn automatic_permission_selection_prefers_once_over_always() {
    let request = execute_request_with_permission_options(
        json!({ "command": "rg -n \"TODO\" src" }),
        vec![
            PermissionOption::new(
                "allow_always",
                "Always Allow",
                PermissionOptionKind::AllowAlways,
            ),
            PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        ],
    );
    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn build_permission_asks_for_dynamic_shell_writes_without_static_path() {
    let request = execute_request(json!({
        "command": "python - <<'PY'\nfrom pathlib import Path\np=Path.cwd() / 'generated.ts'\np.write_text('ok', encoding='utf-8')\nPY"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn codebuddy_bash_read_only_command_is_auto_allowed() {
    let request = codebuddy_bash_request(json!({
        "command": "rg -n \"TODO\" src"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn codebuddy_terminal_read_only_command_is_allowed() {
    assert_eq!(
        decide_codebuddy_terminal_permission("D:/work/repo", "rg -n \"TODO\" src"),
        CodeBuddyTerminalPermissionDecision::Allow,
    );
}

#[test]
fn codebuddy_bash_windows_find_pipeline_inside_workspace_is_auto_allowed() {
    let command = r#"find "d:\work\ArtAssets\packages\frontend\src" -name "*auth*" -o -name "*user*" | head -20"#;
    let request = codebuddy_bash_request(json!({
        "command": command
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/ArtAssets", &request),
        PermissionDecision::Select("allow".to_string()),
    );
    assert_eq!(
        decide_codebuddy_terminal_permission("D:/work/ArtAssets", command),
        CodeBuddyTerminalPermissionDecision::Allow,
    );
}

#[test]
fn codebuddy_terminal_pathlib_write_with_explicit_path_is_interactive() {
    assert_eq!(
        decide_codebuddy_terminal_permission(
            "D:/work/repo",
            "python - <<'PY'\nfrom pathlib import Path\np=Path('packages/backend/src/service.ts')\np.write_text('ok', encoding='utf-8')\nPY",
        ),
        CodeBuddyTerminalPermissionDecision::Ask(vec![PathBuf::from(
            "packages/backend/src/service.ts"
        )]),
    );
}

#[test]
fn codebuddy_terminal_mkdir_with_static_paths_is_interactive() {
    let command = "mkdir -p /Users/kothchen/code/hotnovel/src/server/routes && echo \"ok1\"; mkdir -p /Users/kothchen/code/hotnovel/web && echo \"ok2\"; mkdir -p /Users/kothchen/code/hotnovel/tests/unit/server && echo \"ok3\"";

    assert_eq!(
        decide_codebuddy_terminal_permission("/Users/kothchen/code/hotnovel", command),
        CodeBuddyTerminalPermissionDecision::Ask(vec![
            PathBuf::from("/Users/kothchen/code/hotnovel/src/server/routes"),
            PathBuf::from("/Users/kothchen/code/hotnovel/tests/unit/server"),
            PathBuf::from("/Users/kothchen/code/hotnovel/web"),
        ]),
    );
}

#[test]
fn codebuddy_terminal_suspected_write_without_static_path_is_interactive() {
    assert_eq!(
        decide_codebuddy_terminal_permission(
            "D:/work/repo",
            "python - <<'PY'\nfrom pathlib import Path\np=Path.cwd() / 'generated.ts'\np.write_text('ok', encoding='utf-8')\nPY",
        ),
        CodeBuddyTerminalPermissionDecision::Ask(Vec::new()),
    );
}

#[test]
fn codebuddy_terminal_build_command_without_static_path_is_interactive() {
    assert_eq!(
        decide_codebuddy_terminal_permission("D:/work/repo", "pnpm build"),
        CodeBuddyTerminalPermissionDecision::Ask(Vec::new()),
    );
}

#[test]
fn codebuddy_bash_write_with_explicit_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "cat > src/main.rs << 'EOF'\nfn main() {}\nEOF"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert_eq!(
        codebuddy_bash_write_hint_paths(&request),
        vec![PathBuf::from("src/main.rs")]
    );
}

#[test]
fn codebuddy_bash_write_hint_ignores_shell_heredoc_body_markup() {
    let request = codebuddy_bash_request(json!({
        "command": "cat >> /Users/kothchen/code/hotnovel/web/app.js << 'JS_EOF'\nrender(`\n  <main class=\"space-y-4\">\n    <h2>查看某日报告</h2>\n    <label class=\"block\">日期</label>\n  </main>\n`);\nJS_EOF"
    }));

    assert_eq!(
        codebuddy_bash_write_hint_paths(&request),
        vec![PathBuf::from("/Users/kothchen/code/hotnovel/web/app.js")]
    );
}

#[test]
fn codebuddy_bash_write_hint_ignores_non_writing_heredoc_markup() {
    let request = codebuddy_bash_request(json!({
        "command": "node <<'JS'\nconsole.log('<main>preview</main>')\nJS"
    }));

    assert!(codebuddy_bash_write_hint_paths(&request).is_empty());
}

#[test]
fn codebuddy_bash_pathlib_write_with_explicit_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "python - <<'PY'\nfrom pathlib import Path\np=Path('packages/backend/src/service.ts')\np.write_text('ok', encoding='utf-8')\nPY"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert_eq!(
        codebuddy_bash_write_hint_paths(&request),
        vec![PathBuf::from("packages/backend/src/service.ts")]
    );
}

#[test]
fn codebuddy_bash_python_open_write_with_explicit_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "python3 -c \"open('packages/backend/src/service.ts', 'w', encoding='utf-8').write('ok')\""
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert_eq!(
        codebuddy_bash_write_hint_paths(&request),
        vec![PathBuf::from("packages/backend/src/service.ts")]
    );
}

#[test]
fn codebuddy_bash_suspected_write_without_static_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "python - <<'PY'\nfrom pathlib import Path\np=Path.cwd() / 'generated.ts'\np.write_text('ok', encoding='utf-8')\nPY"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn codebuddy_bash_python_open_write_without_static_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "python3 -c \"path='packages/backend/src/service.ts'; open(path, 'w').write('ok')\""
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
    assert!(codebuddy_bash_write_hint_paths(&request).is_empty());
}

#[test]
fn codebuddy_bash_build_command_without_static_path_is_interactive() {
    let request = codebuddy_bash_request(json!({
        "command": "pnpm build"
    }));

    assert_eq!(
        decide_permission(PermissionPolicyMode::Build, "D:/work/repo", &request),
        PermissionDecision::Ask,
    );
}

#[test]
fn plan_permission_allows_read_only_shell_exploration_inside_workspace() {
    let root = temp_workspace("readonly");
    let root_display = root.to_string_lossy().replace('\\', "/");
    let request = execute_request(json!({
        "command": format!(
            "find {root_display}/packages/backend/src -type f -name \"*.ts\" | grep -E \"(search|score)\" | head -20"
        )
    }));

    assert_eq!(
        decide_permission(
            PermissionPolicyMode::ReadOnly,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::Select("allow".to_string()),
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn plan_permission_allows_codebuddy_unix_drive_shell_paths_inside_workspace() {
    let root = std::env::current_dir().expect("test should run in the workspace");
    let root_display = root.to_string_lossy().replace('\\', "/");
    let mut chars = root_display.chars();
    let drive = chars
        .next()
        .expect("windows current dir should start with a drive letter");
    assert_eq!(chars.next(), Some(':'));
    let unix_drive_root = format!("/{}{}", drive.to_ascii_lowercase(), chars.as_str());
    let request = execute_request(json!({
        "command": format!(
            "find {unix_drive_root}/crates/acp-core/src -type f -name \"*.rs\" | head -20"
        )
    }));

    assert_eq!(
        decide_permission(
            PermissionPolicyMode::ReadOnly,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::Select("allow".to_string()),
    );
}

#[test]
fn plan_permission_asks_for_shell_file_mutations() {
    let root = temp_workspace("mutation");
    let request = execute_request(json!({
        "command": "find packages -type f -name \"*.ts\" -delete"
    }));

    assert_eq!(
        decide_permission(
            PermissionPolicyMode::ReadOnly,
            root.to_str().unwrap(),
            &request,
        ),
        PermissionDecision::Ask,
    );

    let _ = fs::remove_dir_all(root);
}
