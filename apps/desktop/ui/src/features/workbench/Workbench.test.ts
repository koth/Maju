import { describe, expect, it } from "vitest";
import {
  findPendingPermissionRequest,
  findPendingPlanApproval,
  pendingPermissionRequestIds,
} from "./Workbench";
import type { ToolInvocation } from "../../types";

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
    ...overrides,
  };
}
