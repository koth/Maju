import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AgentPlanPanel,
  PermissionRequestPanel,
  PlanApprovalModal,
  findPlanAcceptOption,
  findPlanRejectOption,
  shouldShowAgentPlanNearComposer,
} from "./AgentPlanPanel";
import type { AgentPlanEntry, UiSnapshot } from "../../types";

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

  it("orders active tasks first, pending next, and completed last", () => {
    render(
      <AgentPlanPanel
        entries={[
          {
            id: "done-1",
            content: "Remove term apply buttons",
            priority: "medium",
            status: "completed",
          },
          {
            id: "pending-1",
            content: "Validate interaction hints",
            priority: "medium",
            status: "pending",
          },
          {
            id: "active-1",
            content: "Improve search interaction hints",
            priority: "medium",
            status: "in_progress",
          },
          {
            id: "cancelled-1",
            content: "Skip obsolete cleanup",
            priority: "low",
            status: "cancelled",
          },
          {
            id: "done-2",
            content: "Validate apply button removal",
            priority: "medium",
            status: "completed",
          },
        ]}
      />,
    );

    const contents = screen
      .getAllByRole("listitem")
      .map((item) => item.querySelector(".agent-plan-content")?.textContent);

    expect(contents).toEqual([
      "Improve search interaction hints",
      "Validate interaction hints",
      "Skip obsolete cleanup",
      "Remove term apply buttons",
      "Validate apply button removal",
    ]);
  });

  it("only shows near the composer while a turn is active", () => {
    const entry: AgentPlanEntry = {
      id: "1",
      content: "检查现有实现",
      priority: "medium",
      status: "in_progress",
    };
    const snapshot = (status: UiSnapshot["session"]["status"], entries = [entry]) => ({
      agent_plan: entries,
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status,
      },
    });

    expect(shouldShowAgentPlanNearComposer(snapshot("Streaming"))).toBe(true);
    expect(shouldShowAgentPlanNearComposer(snapshot("WaitingForTool"))).toBe(true);
    expect(shouldShowAgentPlanNearComposer(snapshot("Idle"))).toBe(false);
    expect(shouldShowAgentPlanNearComposer(snapshot("Interrupted"))).toBe(false);
    expect(shouldShowAgentPlanNearComposer(snapshot("Streaming", []))).toBe(false);
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
    expect(screen.getByRole("heading", { name: /实施计划/ })).toBeTruthy();
    expect(screen.getByText("检查实现").className).toContain("md-bold");
    expect(screen.getByText("修改交互").className).toContain("md-inline-code");
    expect(screen.getByText(/修改交互/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "接受计划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "default");

    fireEvent.click(screen.getByRole("button", { name: "继续规划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "plan");
  });

  it("prefers one-shot CodeBuddy plan options over allow always", () => {
    const options = [
      { id: "allow_always", label: "Always Allow", kind: "AllowAlways" },
      { id: "allow", label: "Allow", kind: "AllowOnce" },
      { id: "reject", label: "Reject", kind: "RejectOnce" },
      { id: "reject_and_exit_plan", label: "Exit plan mode", kind: "RejectOnce" },
    ];

    expect(findPlanAcceptOption(options)?.id).toBe("allow");
    expect(findPlanRejectOption(options)?.id).toBe("reject");
  });

  it("recognizes CodeBuddy interruption plan reject option", () => {
    const options = [
      { id: "allow", label: "allow", kind: "CodeBuddy" },
      { id: "rejectAndExitPlan", label: "rejectAndExitPlan", kind: "CodeBuddy" },
    ];

    expect(findPlanAcceptOption(options)?.id).toBe("allow");
    expect(findPlanRejectOption(options)?.id).toBe("rejectAndExitPlan");
  });
});

describe("PermissionRequestPanel", () => {
  it("shows regular permission options above the composer", () => {
    const onPermissionSelect = vi.fn();

    render(
      <PermissionRequestPanel
        entries={[]}
        request={{
          requestId: "permission-bash",
          title: "`find /d/work/ArtAssets -type f | head -20`",
          details: "find /d/work/ArtAssets -type f | head -20",
          options: [
            { id: "allow", label: "Allow", kind: "AllowOnce" },
            { id: "reject", label: "Reject", kind: "RejectOnce" },
          ],
        }}
        onPermissionSelect={onPermissionSelect}
      />,
    );

    expect(screen.getByText("需要权限")).toBeTruthy();
    expect(screen.getAllByText(/find \/d\/work\/ArtAssets/).length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "Allow" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("permission-bash", "allow");

    fireEvent.click(screen.getByRole("button", { name: "Reject" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("permission-bash", "reject");
  });

  it("renders plan approval as an inline composer panel", () => {
    render(
      <PermissionRequestPanel
        entries={[
          {
            id: "1",
            content: "检查现有实现",
            priority: "medium",
            status: "pending",
          },
        ]}
        request={{
          requestId: "exit-plan",
          title: "ExitPlanMode",
          planText: "## 实施计划\n\n拆分模块。",
          options: [
            { id: "default", label: "Default", kind: "AllowOnce" },
            { id: "plan", label: "Plan", kind: "RejectOnce" },
          ],
          isPlanApproval: true,
        }}
      />,
    );

    expect(screen.getByText("待确认计划")).toBeTruthy();
    expect(screen.getByText("检查现有实现")).toBeTruthy();
    expect(screen.getByRole("button", { name: "接受计划" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "继续规划" })).toBeTruthy();
  });

  it("deduplicates plan approval actions from CodeBuddy permission options", () => {
    const onPermissionSelect = vi.fn();

    render(
      <PermissionRequestPanel
        entries={[]}
        request={{
          requestId: "exit-plan",
          title: "ExitPlanMode",
          planText: "# Plan\n\nShip it.",
          options: [
            { id: "allow_always", label: "Always Allow", kind: "AllowAlways" },
            { id: "allow", label: "Allow", kind: "AllowOnce" },
            { id: "reject", label: "Reject", kind: "RejectOnce" },
            { id: "reject_and_exit_plan", label: "Exit plan mode", kind: "RejectOnce" },
          ],
          isPlanApproval: true,
        }}
        onPermissionSelect={onPermissionSelect}
      />,
    );

    expect(screen.queryByRole("button", { name: "Always Allow" })).toBeNull();
    expect(screen.getAllByRole("button", { name: "接受计划" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "继续规划" })).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { name: "继续规划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "reject");

    fireEvent.click(screen.getByRole("button", { name: "接受计划" }));
    expect(onPermissionSelect).toHaveBeenLastCalledWith("exit-plan", "allow");
  });
});
