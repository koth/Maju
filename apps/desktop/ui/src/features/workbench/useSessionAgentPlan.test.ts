import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AgentPlanEntry, UiSnapshot } from "../../types";
import { useSessionAgentPlan } from "./useSessionAgentPlan";

const planEntry = (
  id: string,
  content: string,
  status: AgentPlanEntry["status"] = "pending",
): AgentPlanEntry => ({
  id,
  content,
  priority: "medium",
  status,
});

function snapshot(
  sessionId: string,
  entries: AgentPlanEntry[],
): Pick<UiSnapshot, "agent_plan" | "session"> {
  return {
    agent_plan: entries,
    session: {
      id: sessionId,
      workspace_id: "ws-1",
      title: "Session",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
  };
}

describe("useSessionAgentPlan", () => {
  it("keeps the latest non-empty plan for the active session", () => {
    const firstPlan = [planEntry("1", "检查布局", "in_progress")];
    const { result, rerender } = renderHook(
      ({ value }) => useSessionAgentPlan(value),
      { initialProps: { value: snapshot("s-1", firstPlan) } },
    );

    expect(result.current).toEqual(firstPlan);

    rerender({ value: snapshot("s-1", []) });

    expect(result.current).toEqual(firstPlan);
  });

  it("updates the retained plan when the same session receives a new plan", () => {
    const firstPlan = [planEntry("1", "检查布局", "in_progress")];
    const nextPlan = [
      planEntry("1", "检查布局", "completed"),
      planEntry("2", "验证常驻进度", "in_progress"),
    ];
    const { result, rerender } = renderHook(
      ({ value }) => useSessionAgentPlan(value),
      { initialProps: { value: snapshot("s-1", firstPlan) } },
    );

    rerender({ value: snapshot("s-1", nextPlan) });
    expect(result.current).toEqual(nextPlan);

    rerender({ value: snapshot("s-1", []) });
    expect(result.current).toEqual(nextPlan);
  });

  it("keeps retained plans scoped by session", () => {
    const firstPlan = [planEntry("1", "检查布局", "in_progress")];
    const { result, rerender } = renderHook(
      ({ value }) => useSessionAgentPlan(value),
      { initialProps: { value: snapshot("s-1", firstPlan) } },
    );

    rerender({ value: snapshot("s-2", []) });

    expect(result.current).toEqual([]);

    rerender({ value: snapshot("s-1", []) });

    expect(result.current).toEqual(firstPlan);
  });
});
