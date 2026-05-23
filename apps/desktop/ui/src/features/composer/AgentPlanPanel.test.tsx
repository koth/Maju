import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentPlanPanel, PlanApprovalModal } from "./AgentPlanPanel";

afterEach(() => {
  cleanup();
});

describe("AgentPlanPanel", () => {
  it("renders only the current task list inline", () => {
    render(
      <AgentPlanPanel
        entries={[
          {
            id: "1",
            content: "检查现有实现",
            priority: "medium",
            status: "pending",
          },
        ]}
      />,
    );

    expect(screen.getByText("检查现有实现")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "接受计划" })).toBeNull();
  });
});

describe("PlanApprovalModal", () => {
  it("shows pending plan approval content and resolves accept/reject actions", () => {
    const onPermissionSelect = vi.fn();

    render(
      <PlanApprovalModal
        entries={[
          {
            id: "1",
            content: "检查现有实现",
            priority: "medium",
            status: "pending",
          },
        ]}
        approval={{
          requestId: "exit-plan",
          planText: "## 实施计划\n\n1. **检查实现**\n2. `修改交互`\n3. 验证测试",
          options: [
            { id: "default", label: "Yes, and manually approve edits", kind: "AllowOnce" },
            { id: "plan", label: "No, keep planning", kind: "RejectOnce" },
          ],
        }}
        onPermissionSelect={onPermissionSelect}
      />,
    );

    expect(screen.getByText("检查现有实现")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "实施计划" })).toBeTruthy();
    expect(screen.getByText("检查实现").className).toContain("md-bold");
    expect(screen.getByText("修改交互").className).toContain("md-inline-code");
    expect(screen.getByText(/修改交互/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "接受计划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "default");

    fireEvent.click(screen.getByRole("button", { name: "继续规划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "plan");
  });
});
