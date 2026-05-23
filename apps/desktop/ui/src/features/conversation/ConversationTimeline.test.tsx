import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/react";
import { ConversationTimeline, type TimelineTurnChangeSet } from "./ConversationTimeline";
import { appendStreamingMessageDelta } from "./streaming-message-store";
import type {
  FileChangeSummary,
  TimelineItem,
  ToolInvocation,
  UiSnapshot,
} from "../../types/index";

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
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
    ...overrides,
  };
}

function makeFileSummary(
  path: string,
  addedLines: number,
  removedLines: number,
  changeSetId = "cs-1",
): FileChangeSummary {
  return {
    change_set_id: changeSetId,
    path,
    change_type: "Modified",
    added_lines: addedLines,
    removed_lines: removedLines,
    quality: "Exact",
    updated_at: "2026-05-12T00:00:00Z",
  };
}

function makeTurnChangeSet(
  changeSetId: string,
  files: FileChangeSummary[],
): TimelineTurnChangeSet {
  return {
    changeSetId,
    files,
    updatedAt: "2026-05-12T00:00:00Z",
  };
}

function makePermissionTool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "permission-1",
    parent_call_id: null,
    name: "Permission",
    kind: "permission",
    summary: "Permission required",
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
    permission_options: [
      { id: "default", label: "Allow", kind: "AllowOnce" },
      { id: "plan", label: "Reject", kind: "RejectOnce" },
    ],
    permission_decision: null,
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
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
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
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
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
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
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

  it("skips whitespace-only assistant and system messages", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }, { Message: "msg-2" }, { Message: "msg-3" }],
      messages: [
        { id: "msg-1", role: "Assistant", body: "\n\n" },
        { id: "msg-2", role: "System", body: " \t\n" },
        { id: "msg-3", role: "Assistant", body: "done" },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelectorAll(".msg")).toHaveLength(1);
    expect(container.querySelector(".msg-assistant")?.textContent).toContain("done");
  });

  it("hides permission requests that are handled by the plan approval modal", () => {
    const permissionTool = makePermissionTool();
    const snapshot = makeSnapshot({
      timeline: [{ Tool: permissionTool.id }],
      tools: [permissionTool],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        hiddenPermissionRequestIds={new Set([permissionTool.call_id])}
      />,
    );

    expect(container.textContent).not.toContain("选择权限");
    expect(container.textContent).not.toContain("Allow");
  });

  it("renders the active streaming assistant message as markdown", async () => {
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
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
    );

    await waitFor(() => {
      expect(container.querySelector(".msg-streaming-markdown .md-bold")?.textContent).toBe(
        "live",
      );
    });
    expect(container.querySelector(".msg-streaming-markdown")?.textContent).toBe("live output");
    expect(container.querySelector(".streaming-cursor")).toBeTruthy();
  });

  it("marks adjacent image-only user paragraphs so attachments can flow in one row", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "User",
          body:
            "看看这两张图\n\n![图1](data:image/png;base64,aaaa)\n\n![图2](data:image/png;base64,bbbb)",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    const userMessage = container.querySelector(".msg-user");
    expect(userMessage?.querySelectorAll(".md-image")).toHaveLength(2);
    expect(userMessage?.querySelectorAll(".md-image-paragraph")).toHaveLength(2);
    expect(userMessage?.querySelector(".md-paragraph:not(.md-image-paragraph)")?.textContent).toBe(
      "看看这两张图",
    );
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

  it("renders per-turn changes under the matching assistant message", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Idle",
      },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "done" }],
      session_changes: [
        {
          path: "apps/desktop/ui/src/features/workbench/Workbench.tsx",
          change_type: "Modified",
          old_text: null,
          new_text: "",
          added_lines: 50,
          removed_lines: 20,
          timestamp: "2026-05-12T00:00:00Z",
        },
      ],
      turn_changes: [
        {
          message_id: "msg-1",
          changes: [
            {
              path: "apps/desktop/ui/src/features/conversation/ConversationTimeline.tsx",
              change_type: "Modified",
              old_text: null,
              new_text: "",
              added_lines: 3,
              removed_lines: 1,
              timestamp: "2026-05-12T00:00:00Z",
            },
          ],
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary(
              "apps/desktop/ui/src/features/conversation/ConversationTimeline.tsx",
              3,
              1,
              "turn-msg-1",
            ),
          ]),
        }}
      />,
    );

    expect(container.querySelector(".timeline-items .changes-bar")).toBeTruthy();
    expect(container.textContent?.indexOf("done")).toBeLessThan(
      container.textContent?.indexOf("已编辑 1 个文件") ?? -1,
    );
    expect(container.textContent).toContain("已编辑 1 个文件");
    expect(container.textContent).toContain(
      "apps/desktop/ui/src/features/conversation/ConversationTimeline.tsx",
    );
    expect(container.textContent).not.toContain(
      "apps/desktop/ui/src/features/workbench/Workbench.tsx",
    );
  });

  it("does not render transient review changes at the end of the timeline", () => {
    const snapshot = makeSnapshot({
      review_changes: [
        {
          path: "apps/desktop/ui/src/features/conversation/ConversationTimeline.tsx",
          change_type: "Modified",
          old_text: null,
          new_text: "",
          added_lines: 3,
          removed_lines: 1,
          timestamp: "2026-05-12T00:00:00Z",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".changes-bar")).toBeNull();
  });

  it("does not render live turn changes before the turn is attached to an assistant message", () => {
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
      messages: [{ id: "msg-1", role: "Assistant", body: "working" }],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
    );

    expect(container.querySelector(".changes-bar")).toBeNull();
    expect(container.textContent).not.toContain("已编辑 1 个文件");
    expect(container.textContent).not.toContain("src/live.ts");
  });

  it("does not render attached turn changes while the current turn is still active", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "WaitingForTool",
      },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "working" }],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("src/attached.ts", 4, 2, "turn-msg-1"),
          ]),
        }}
      />,
    );

    expect(container.querySelector(".changes-bar")).toBeNull();
    expect(container.textContent).not.toContain("src/attached.ts");
  });

  it("does not render live turn changes after the turn is idle", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Idle",
      },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "done" }],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
    );

    expect(container.querySelector(".changes-bar")).toBeNull();
  });

  it("keeps separate changes with each historical turn", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Idle",
      },
      timeline: [{ Message: "msg-1" }, { Message: "msg-2" }],
      messages: [
        { id: "msg-1", role: "Assistant", body: "first turn" },
        { id: "msg-2", role: "Assistant", body: "second turn" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("first.ts", 1, 0, "turn-msg-1"),
          ]),
          "msg-2": makeTurnChangeSet("turn-msg-2", [
            makeFileSummary("second.ts", 2, 1, "turn-msg-2"),
          ]),
        }}
      />,
    );

    expect(container.querySelectorAll(".changes-bar")).toHaveLength(2);
    const text = container.textContent ?? "";
    expect(text.indexOf("first turn")).toBeLessThan(text.indexOf("first.ts"));
    expect(text.indexOf("first.ts")).toBeLessThan(text.indexOf("second turn"));
    expect(text.indexOf("second turn")).toBeLessThan(text.indexOf("second.ts"));
  });

  it("does not repeat a previous turn change set under a later assistant message", () => {
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Idle",
      },
      timeline: [{ Message: "msg-1" }, { Message: "msg-2" }],
      messages: [
        { id: "msg-1", role: "Assistant", body: "changed files" },
        { id: "msg-2", role: "Assistant", body: "answered only" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("changed.ts", 5, 3, "turn-msg-1"),
          ]),
        }}
      />,
    );

    expect(container.querySelectorAll(".changes-bar")).toHaveLength(1);
    const text = container.textContent ?? "";
    expect(text.indexOf("changed.ts")).toBeLessThan(text.indexOf("answered only"));
    expect(text.lastIndexOf("changed.ts")).toBe(text.indexOf("changed.ts"));
  });

  it("opens timeline changes with the producing change set id", () => {
    const onReviewFileSelect = vi.fn();
    const snapshot = makeSnapshot({
      session: {
        id: "s-1",
        workspace_id: "ws-1",
        title: "test",
        model: "test-model",
        mode: null,
        agent_cli: null,
        status: "Idle",
      },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "done" }],
    });

    const { getByText } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onReviewFileSelect={onReviewFileSelect}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("src/file.ts", 1, 1, "turn-msg-1"),
          ]),
        }}
      />,
    );

    fireEvent.click(getByText("src/file.ts"));
    expect(onReviewFileSelect).toHaveBeenCalledWith("src/file.ts", "turn-msg-1");
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

    appendStreamingMessageDelta("streaming-msg", " **world**");
    await new Promise((resolve) => window.setTimeout(resolve, 100));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollIntoView).toHaveBeenCalled();
    expect(container.querySelector(".msg-streaming-markdown .md-bold")?.textContent).toBe(
      "world",
    );
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
    fireEvent.wheel(scroller);
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

  it("keeps following after programmatic scroll events and content resize", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    let triggerResize = () => {};
    const OriginalResizeObserver = globalThis.ResizeObserver;
    class TestResizeObserver implements ResizeObserver {
      constructor(callback: ResizeObserverCallback) {
        triggerResize = () => callback([], {} as ResizeObserver);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    globalThis.ResizeObserver = TestResizeObserver;
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "hello" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));
    scrollIntoView.mockClear();

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", { configurable: true, value: 100 });
    fireEvent.scroll(scroller);

    triggerResize();
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollIntoView).toHaveBeenCalled();
    globalThis.ResizeObserver = OriginalResizeObserver;
    delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
  });
});
