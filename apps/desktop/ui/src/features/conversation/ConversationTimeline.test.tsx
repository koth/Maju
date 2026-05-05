import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ConversationTimeline } from "./ConversationTimeline";
import type { UiSnapshot, TimelineItem } from "../../types/index";

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    workspace: { id: "ws-1", name: "test", root: "/test" },
    session: {
      id: "s-1",
      workspace_id: "ws-1",
      title: "test",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Streaming",
    },
    session_config: { hydrated: false, controls: [] },
    prompt_capabilities: { image: false, embedded_context: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools: [],
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    thinking_status: null,
    ...overrides,
  };
}

describe("ThinkingIndicator", () => {
  it("renders thinking-active class when thinking is active", () => {
    const timeline: TimelineItem[] = ["Thinking"];
    const snapshot = makeSnapshot({
      timeline,
      thinking_status: "Active",
    });
    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    const indicator = container.querySelector(".thinking-indicator");
    expect(indicator).toBeTruthy();
    expect(indicator!.classList.contains("thinking-active")).toBe(true);
    expect(container.querySelector(".thinking-text")!.textContent).toBe("思考中");
  });

  it("hides thinking indicator when thinking is completed", () => {
    const timeline: TimelineItem[] = ["Thinking"];
    const snapshot = makeSnapshot({
      timeline,
      thinking_status: "Completed",
    });
    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    expect(container.querySelector(".thinking-indicator")).toBeNull();
  });

  it("renders only the latest thinking indicator when timeline contains history", () => {
    const timeline: TimelineItem[] = ["Thinking", { Message: "msg-1" }, "Thinking"];
    const snapshot = makeSnapshot({
      timeline,
      messages: [{ id: "msg-1", role: "Assistant", body: "done" }],
      thinking_status: "Active",
    });
    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    expect(container.querySelectorAll(".thinking-indicator")).toHaveLength(1);
  });

  it("does not render thinking indicator when timeline has no Thinking item", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "User", body: "hello" }],
      thinking_status: null,
    });
    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    expect(container.querySelector(".thinking-indicator")).toBeNull();
  });
});
