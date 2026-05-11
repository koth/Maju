import { describe, it, expect, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { ConversationTimeline } from "./ConversationTimeline";
import { appendStreamingMessageDelta } from "./streaming-message-store";
import type { UiSnapshot, TimelineItem } from "../../types/index";

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    revision: 1,
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

  it("renders the active streaming assistant message without markdown parsing", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Streaming",
      },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "**live** output" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-streaming-text")?.textContent).toBe("**live** output");
    expect(container.querySelector(".streaming-cursor")).toBeTruthy();
  });

  it("windows long timelines so initial render only mounts the latest entries", () => {
    const messages = Array.from({ length: 120 }, (_, index) => ({
      id: `msg-${index}`,
      role: "System" as const,
      body: `message ${index}`,
    }));
    const snapshot = makeSnapshot({
      timeline: messages.map((message) => ({ Message: message.id })),
      messages,
    });

    const { container, getByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).not.toContain("message 0");
    expect(container.textContent).toContain("message 119");
    expect(container.querySelectorAll(".msg")).toHaveLength(80);

    fireEvent.click(getByRole("button", { name: /显示更早/ }));
    expect(container.textContent).toContain("message 0");
    expect(container.querySelectorAll(".msg")).toHaveLength(120);
  });

  it("renders the plan panel inside the timeline flow", () => {
    const snapshot = makeSnapshot();
    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        planPanel={<section className="test-plan-panel">plan lives here</section>}
      />,
    );

    expect(container.querySelector(".timeline-items .test-plan-panel")?.textContent).toBe(
      "plan lives here",
    );
  });

  it("follows plan updates to the bottom when already near bottom", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    const snapshot = makeSnapshot();
    const { rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));
    scrollIntoView.mockClear();

    rerender(
      <ConversationTimeline
        snapshot={{
          ...snapshot,
          revision: 2,
          agent_plan: [
            { id: "plan-1", content: "Do the work", status: "in_progress", priority: "medium" },
          ],
        }}
        onPermissionSelect={() => {}}
        planPanel={<section className="test-plan-panel">Do the work</section>}
      />,
    );
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollIntoView).toHaveBeenCalled();
    delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
  });

  it("follows streaming chunks when already near bottom", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    const snapshot = makeSnapshot({
      timeline: [{ Message: "streaming-msg" }],
      messages: [{ id: "streaming-msg", role: "Assistant", body: "hello" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));
    scrollIntoView.mockClear();

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", { configurable: true, value: 790 });

    appendStreamingMessageDelta("streaming-msg", " world");
    await new Promise((resolve) => window.setTimeout(resolve, 100));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollIntoView).toHaveBeenCalled();
    delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
  });

  it("keeps manual scroll position instead of forcing bottom follow", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "hello" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));
    scrollIntoView.mockClear();

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", { configurable: true, value: 100 });
    fireEvent.scroll(scroller);

    rerender(
      <ConversationTimeline
        snapshot={{ ...snapshot, revision: 2, thinking_status: "Active" }}
        onPermissionSelect={() => {}}
      />,
    );
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollIntoView).not.toHaveBeenCalled();
    delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
  });
});
