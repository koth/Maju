import { describe, expect, it } from "vitest";
import {
  agentPlanDockProgressSignature,
  agentPlanProgressSignature,
  findPendingPermissionRequest,
  findPendingPlanApproval,
  isTerminalDockAvailableForWorkspace,
  pendingPermissionRequestIds,
  shouldAutoOpenAgentPlanDock,
  shouldRenderTerminalDock,
} from "./Workbench";
import type { AgentPlanEntry, ToolInvocation, UiSnapshot, WorkspaceDescriptor } from "../../types";
import type { TimelineTurnChangeSet } from "../conversation/ConversationTimeline";

describe("findPendingPlanApproval", () => {
  it("recognizes CodeBuddy ExitPlanMode permission options", () => {
    const approval = findPendingPlanApproval([
      tool({
        name: "Permission request",
        raw_input: '{"plan":"# Plan\\n\\nShip it."}',
        permission_options: [
          { id: "allow_always", label: "Always Allow", kind: "AllowAlways" },
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
          { id: "reject_and_exit_plan", label: "Exit plan mode", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.requestId).toBe("call-exit-plan");
    expect(approval?.planText).toContain("# Plan");
  });

  it("shows only the embedded plan text from structured raw input", () => {
    const approval = findPendingPlanApproval([
      tool({
        name: "ExitPlanMode",
        raw_input: JSON.stringify({
          allowedPrompts: [
            { prompt: "run frontend build validation", tool: "Bash" },
            { prompt: "create new frontend refactor directories", tool: "Bash" },
          ],
          plan: "# GalleryPage.tsx 重构计划\n\n## 目标\n拆分页面模块。",
        }),
        permission_options: [
          { id: "default", label: "Default", kind: "AllowOnce" },
          { id: "plan", label: "Plan", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.planText).toContain("# GalleryPage.tsx 重构计划");
    expect(approval?.planText).not.toContain("allowedPrompts");
    expect(approval?.planText).not.toContain("run frontend build validation");
  });

  it("uses the latest CodeBuddy plan file write as plan approval fallback", () => {
    const approval = findPendingPlanApproval([
      tool({
        call_id: "write-plan",
        name: "Write negative terms implementation plan",
        status: "Succeeded",
        raw_input: JSON.stringify({
          file_path: "C:/Users/yvonchen/.codebuddy/plans/toasty-forging-newton.md",
          content: "# Negative Terms Plan\n\n## Goal\nTune negative term ranking.",
        }),
      }),
      tool({
        name: "ExitPlanMode",
        detail_text: "Path: C:/Users/yvonchen/.codebuddy/plans/toasty-forging-newton.md",
        raw_input: JSON.stringify({ allowedPrompts: [] }),
        permission_options: [
          { id: "allow_always", label: "Always Allow", kind: "AllowAlways" },
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
          { id: "reject_and_exit_plan", label: "Exit plan mode", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.requestId).toBe("call-exit-plan");
    expect(approval?.planText).toContain("# Negative Terms Plan");
    expect(approval?.planText).not.toContain(".codebuddy/plans");
  });

  it("ignores CodeBuddy missing-plan warning when a recent plan file write exists", () => {
    const approval = findPendingPlanApproval([
      tool({
        call_id: "write-plan",
        name: "Write implementation plan",
        status: "Succeeded",
        raw_input: JSON.stringify({
          file_path: "C:/Users/yvonchen/.codebuddy/plans/swift-cascade-babbage.md",
          content: "# Scene Elements Plan\n\n## Goal\nExtract element dimensions.",
        }),
      }),
      tool({
        name: "ExitPlanMode",
        detail_text: "Plan mode exited. Warning: Plan file was not found or empty.",
        raw_input: JSON.stringify({ allowedPrompts: [] }),
        permission_options: [
          { id: "default", label: "Default", kind: "AllowOnce" },
          { id: "plan", label: "Plan", kind: "RejectOnce" },
          { id: "rejectAndExitPlan", label: "Stop planning", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.requestId).toBe("call-exit-plan");
    expect(approval?.planText).toContain("# Scene Elements Plan");
    expect(approval?.planText).not.toContain("Plan mode exited");
  });

  it("ignores CodeBuddy missing-plan warning from raw input plan field", () => {
    const approval = findPendingPlanApproval([
      tool({
        call_id: "write-plan",
        name: "Write implementation plan",
        status: "Succeeded",
        raw_input: JSON.stringify({
          file_path: "C:/Users/yvonchen/.codebuddy/plans/swift-cascade-babbage.md",
          content: "# Scene Elements Plan\n\n## Goal\nExtract element dimensions.",
        }),
      }),
      tool({
        name: "ExitPlanMode",
        raw_input: JSON.stringify({
          plan: "Plan mode exited. Warning: Plan file was not found or empty.",
        }),
        permission_options: [
          { id: "default", label: "Default", kind: "AllowOnce" },
          { id: "plan", label: "Plan", kind: "RejectOnce" },
          { id: "rejectAndExitPlan", label: "Stop planning", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.requestId).toBe("call-exit-plan");
    expect(approval?.planText).toContain("# Scene Elements Plan");
    expect(approval?.planText).not.toContain("Plan mode exited");
  });
  it("uses a recent CodeBuddy plan edit as fallback when ExitPlanMode only has a warning", () => {
    const approval = findPendingPlanApproval([
      tool({
        call_id: "edit-plan",
        name: "Edit",
        status: "Succeeded",
        raw_input: JSON.stringify({
          file_path: "C:/Users/yvonchen/.codebuddy/plans/swift-cascade-babbage.md",
          old_string: "",
          new_string: "## 执行细节补充\n\n### A. extractor.ts 的 prompt 与输出契约\n\n输出严格 JSON。",
        }),
        diff_previews: [
          {
            path: "C:/Users/yvonchen/.codebuddy/plans/swift-cascade-babbage.md",
            hunks: [
              {
                heading: "@@",
                lines: [
                  { kind: "Added", content: "## 执行细节补充" },
                  { kind: "Added", content: "" },
                  { kind: "Added", content: "### A. extractor.ts 的 prompt 与输出契约" },
                  { kind: "Added", content: "" },
                  { kind: "Added", content: "输出严格 JSON。" },
                ],
              },
            ],
          },
        ],
      }),
      tool({
        name: "ExitPlanMode",
        detail_text: "Plan mode exited. Warning: Plan file was not found or empty.",
        raw_input: JSON.stringify({ allowedPrompts: [] }),
        permission_options: [
          { id: "default", label: "Default", kind: "AllowOnce" },
          { id: "plan", label: "Plan", kind: "RejectOnce" },
          { id: "rejectAndExitPlan", label: "Stop planning", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(approval?.requestId).toBe("call-exit-plan");
    expect(approval?.planText).toContain("## 执行细节补充");
    expect(approval?.planText).toContain("输出严格 JSON");
    expect(approval?.planText).not.toContain("Plan mode exited");
  });
});

describe("terminal dock availability", () => {
  it("renders the terminal dock for remote Linux workspaces when mounted", () => {
    const remoteWorkspace: WorkspaceDescriptor = {
      id: "remote",
      name: "project",
      root: "ssh://devbox/srv/project",
      location: {
        kind: "remote_linux",
        ssh_target: "devbox",
        remote_path: "/srv/project",
      },
    };

    expect(isTerminalDockAvailableForWorkspace(remoteWorkspace)).toBe(true);
    expect(shouldRenderTerminalDock(remoteWorkspace, true)).toBe(true);
  });
});

describe("agent plan dock auto-open", () => {
  const entry: AgentPlanEntry = {
    id: "task-1",
    content: "检查现有实现",
    priority: "medium",
    status: "pending",
  };

  it("tracks task content and status changes in the progress signature", () => {
    const firstSignature = agentPlanProgressSignature([entry]);
    const completedSignature = agentPlanProgressSignature([
      { ...entry, status: "completed" },
    ]);
    const renamedSignature = agentPlanProgressSignature([
      { ...entry, content: "补充验证" },
    ]);

    expect(firstSignature).toBe(agentPlanProgressSignature([entry]));
    expect(completedSignature).not.toBe(firstSignature);
    expect(renamedSignature).not.toBe(firstSignature);
  });

  it("opens for new live progress only while a turn is active in the conversation", () => {
    const currentSignature = agentPlanProgressSignature([entry]);
    const base = {
      entryCount: 1,
      sessionStatus: "Streaming" as const,
      activeTabType: "conversation" as const,
      reviewPanelExpanded: false,
      currentSignature,
      lastAutoOpenedSignature: null,
    };

    expect(shouldAutoOpenAgentPlanDock(base)).toBe(true);
    expect(shouldAutoOpenAgentPlanDock({ ...base, sessionStatus: "Idle" })).toBe(false);
    expect(shouldAutoOpenAgentPlanDock({ ...base, activeTabType: "editor" })).toBe(false);
    expect(shouldAutoOpenAgentPlanDock({ ...base, activeTabType: "changes" })).toBe(false);
    expect(shouldAutoOpenAgentPlanDock({ ...base, reviewPanelExpanded: true })).toBe(false);
    expect(shouldAutoOpenAgentPlanDock({ ...base, entryCount: 0 })).toBe(false);
    expect(shouldAutoOpenAgentPlanDock({
      ...base,
      lastAutoOpenedSignature: currentSignature,
    })).toBe(false);
  });

  it("opens for live environment progress even when there are no plan entries", () => {
    const snapshot = makeSnapshot({ agent_plan: [] });
    const liveTurnChanges: TimelineTurnChangeSet = {
      changeSetId: "changes-live",
      updatedAt: "2026-06-23T00:00:00Z",
      files: [
        {
          change_set_id: "changes-live",
          path: "src/app.ts",
          change_type: "Modified",
          added_lines: 2,
          removed_lines: 1,
          quality: "Exact",
          updated_at: "2026-06-23T00:00:00Z",
        },
      ],
    };
    const currentSignature = agentPlanDockProgressSignature(snapshot, liveTurnChanges);

    expect(currentSignature).toContain("changes:");
    expect(shouldAutoOpenAgentPlanDock({
      entryCount: 0,
      hasProgress: currentSignature.length > 0,
      sessionStatus: "WaitingForTool",
      activeTabType: "conversation",
      reviewPanelExpanded: false,
      currentSignature,
      lastAutoOpenedSignature: null,
    })).toBe(true);
  });

  it("does not treat persisted turn changes as live dock progress when live changes are absent", () => {
    const snapshot = makeSnapshot({
      agent_plan: [],
      turn_changes: [
        {
          message_id: "message-1",
          changes: [
            {
              path: "src/app.ts",
              change_type: "Modified",
              old_text: "old",
              new_text: "new",
              added_lines: 2,
              removed_lines: 1,
              timestamp: "2026-06-23T00:00:00Z",
            },
          ],
        },
      ],
    });

    expect(agentPlanDockProgressSignature(snapshot, null)).toBe("");
  });

  it("treats live usage as environment progress for the dock", () => {
    const snapshot = makeSnapshot({
      agent_plan: [],
      usage: {
        context: { used_tokens: 1200, window_tokens: 128000, updated_at: "2026-06-23T00:00:00Z" },
        current_turn: { total_tokens: 1200 },
        session_total: {},
        by_model: [],
      },
    });

    expect(agentPlanDockProgressSignature(snapshot)).toContain("usage");
  });
});

describe("findPendingPermissionRequest", () => {
  it("extracts regular permission requests for composer-level display", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "permission-bash",
        name: "`find /d/work/ArtAssets -type f | head -20`",
        detail_text: "find /d/work/ArtAssets -type f | head -20",
        permission_options: [
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(request?.requestId).toBe("permission-bash");
    expect(request?.title).toContain("find /d/work/ArtAssets");
    expect(request?.details).toContain("head -20");
    expect(request?.isPlanApproval).toBe(false);
    expect(request?.options.map((option) => option.id)).toEqual(["allow", "reject"]);
  });

  it("extracts permission requests attached to running execute tools", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "call-bash",
        kind: "execute",
        name: "`ls -la /g/kothbot/ 2>&1`",
        raw_input: JSON.stringify({ command: "ls -la /g/kothbot/ 2>&1" }),
        permission_options: [
          { id: "allow_always", label: "Always Allow", kind: "AllowAlways" },
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(request?.requestId).toBe("call-bash");
    expect(request?.title).toContain("ls -la /g/kothbot");
    expect(request?.options.map((option) => option.id)).toEqual([
      "allow_always",
      "allow",
      "reject",
    ]);
  });

  it("promotes extracted permission paths into the request title", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "permission-bash",
        name: "Bash",
        detail_text:
          "Command:\npython - << 'PY'\n...\nPY\n\nPath: D:/work/ArtAssets/packages/backend/src/app/index.ts",
        permission_options: [
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(request?.title).toBe("Bash: D:/work/ArtAssets/packages/backend/src/app/index.ts");
  });

  it("promotes terminal permission paths into the request title", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "permission-terminal",
        name: "Bash",
        detail_text:
          "Command:\npython - << 'PY'\n...\nPY\n\nPaths:\n- D:/work/ArtAssets/packages/backend/src/app/index.ts",
        permission_options: [
          { id: "allow", label: "Allow", kind: "AllowOnce" },
          { id: "reject", label: "Reject", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(request?.title).toBe("Bash: D:/work/ArtAssets/packages/backend/src/app/index.ts");
  });

  it("hides internal ACP decision choices from permission request details", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "permission-read",
        name: "Read jni_bridge.cc",
        detail_text:
          "Proposed Amendment: cat\napp/src/main/cpp/jni_bridge.cc\nAvailable Decisions: Approved\nApprovedExecpolicyAmendment\nAbort\n\nPath: app/src/main/cpp/jni_bridge.cc",
        permission_options: [
          { id: "allow", label: "Yes, proceed", kind: "AllowOnce" },
          { id: "allow_always", label: "Yes, don't ask again", kind: "AllowAlways" },
          { id: "reject", label: "拒绝并补充说明", kind: "RejectOnce" },
        ],
      }),
    ]);

    expect(request?.details).toContain("Proposed Amendment: cat");
    expect(request?.details).toContain("Path: app/src/main/cpp/jni_bridge.cc");
    expect(request?.details).not.toContain("Available Decisions");
    expect(request?.details).not.toContain("ApprovedExecpolicyAmendment");
    expect(request?.details).not.toContain("Abort");
    expect(request?.title).toBe("Read jni_bridge.cc: app/src/main/cpp/jni_bridge.cc");
  });

  it("passes structured user-input questions through pending permission requests", () => {
    const request = findPendingPermissionRequest([
      tool({
        call_id: "ask-user",
        name: "Ask user",
        permission_options: [{ id: "answer:0:0", label: "A", kind: "AllowOnce" }],
        permission_input: {
          questions: [
            {
              id: "approach",
              header: "Approach",
              question: "Pick an approach?",
              is_other: false,
              is_secret: false,
              multi_select: false,
              options: [{ label: "A", description: "Use A." }],
            },
          ],
        },
      }),
    ]);

    expect(request?.input?.questions[0]?.id).toBe("approach");
  });

  it("tracks all unresolved permission tools so timeline cards can be hidden", () => {
    expect(
      pendingPermissionRequestIds([
        tool({ call_id: "pending-1", permission_options: [{ id: "allow", label: "Allow", kind: "AllowOnce" }] }),
        tool({
          call_id: "pending-execute",
          kind: "execute",
          permission_options: [{ id: "allow", label: "Allow", kind: "AllowOnce" }],
        }),
        tool({
          call_id: "resolved",
          permission_options: [{ id: "allow", label: "Allow", kind: "AllowOnce" }],
          permission_decision: "Permission selected: Allow",
          status: "Succeeded",
        }),
        tool({ call_id: "pending-2", permission_options: [{ id: "reject", label: "Reject", kind: "RejectOnce" }] }),
      ]),
    ).toEqual(["pending-1", "pending-execute", "pending-2"]);
  });
});

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "kodex", root: "D:/work/kodex" },
    session: {
      id: "session-1",
      workspace_id: "ws-1",
      title: "Session",
      model: "gpt-5.1",
      mode: null,
      agent_cli: null,
      status: "Streaming",
    },
    session_config: { hydrated: true, controls: [] },
    prompt_capabilities: { image: true, embedded_context: true, session_steer: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools: [],
    repository: { branch: "main", head: "abc123", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
    ...overrides,
  };
}
function tool(overrides: Partial<ToolInvocation>): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "call-exit-plan",
    parent_call_id: null,
    name: "Permission request",
    kind: "permission",
    summary: "Waiting",
    status: "Running",
    is_subagent: false,
    detail_text: "",
    logs: [],
    diff_paths: [],
    diff_previews: [],
    raw_input: null,
    raw_output: null,
    terminal_output: null,
    error: null,
    permission_options: [],
    permission_input: null,
    permission_decision: null,
    can_stop: false,
    stop_kind: null,
    stop_status: null,
    ...overrides,
  };
}
