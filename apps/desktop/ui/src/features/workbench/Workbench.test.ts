import { describe, expect, it } from "vitest";
import { findPendingPlanApproval } from "./Workbench";
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
    permission_decision: null,
    ...overrides,
  };
}
