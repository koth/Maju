import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { sessionForkCandidates, settingsGetAgentSnapshot, settingsListDshPresets } from "../../lib/tauri";
import { ConversationTimeline, conversationForkCapability, type TimelineTurnChangeSet } from "./ConversationTimeline";
import {
  appendStreamingMessageDelta,
  ensureStreamingMessageBody,
  replaceStreamingMessageBody,
} from "./streaming-message-store";
import type {
  FileChangeSummary,
  TimelineItem,
  ToolInvocation,
  UiSnapshot,
} from "../../types/index";

// 分叉点选择器从后端拉取全量轮次；交接弹窗要读 Agent 列表与 dsh 预设。
// 其余 tauri 包装保持原实现（未调用）。
vi.mock("../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  sessionForkCandidates: vi.fn(),
  settingsGetAgentSnapshot: vi.fn(),
  settingsListDshPresets: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(sessionForkCandidates).mockReset().mockResolvedValue([]);
  vi.mocked(settingsGetAgentSnapshot).mockResolvedValue({
    agents: [{ id: "codex-acp", label: "Codex", binary: "codex-acp", installed: true }],
    settings: { selected_agent: "codex-acp" },
  } as unknown as Awaited<ReturnType<typeof settingsGetAgentSnapshot>>);
  vi.mocked(settingsListDshPresets).mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  clearMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

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
    prompt_capabilities: { image: false, embedded_context: false, session_steer: false },
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
    permission_input: null,
    permission_decision: null,
    can_stop: false,
    stop_kind: null,
    stop_status: null,
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

  it("renders context compaction notices as divider rows", () => {
    const pending = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [{ id: "compact-start", role: "System", body: "正在压缩上下文" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={pending} onPermissionSelect={() => {}} />,
    );

    expect(within(container).getByRole("status")).toHaveTextContent("正在压缩上下文");
    expect(container.querySelector(".msg-context-compaction.is-pending")).not.toBeNull();

    const completed = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [{ id: "compact-start", role: "System", body: "上下文已自动压缩" }],
    });

    rerender(<ConversationTimeline snapshot={completed} onPermissionSelect={() => {}} />);

    expect(container.querySelector(".msg-context-compaction.is-completed")).not.toBeNull();
    expect(container.textContent).toContain("上下文已自动压缩");
    expect(container.querySelector(".msg-content-system")).toBeNull();
  });

  it("renders dsh compaction lifecycle notices as divider rows", () => {
    // dsh-bridge appends the compaction id to the running notice and the
    // manual /compact outcome arrives as "上下文压缩完成：{text}"; all of
    // these must render as the divider, never as a plain system row.
    const pending = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [{ id: "compact-start", role: "System", body: "正在压缩上下文（cmd-1）" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={pending} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-context-compaction.is-pending")).not.toBeNull();
    expect(within(container).getByRole("status")).toHaveTextContent("正在压缩上下文");

    const completed = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [
        {
          id: "compact-start",
          role: "System",
          body: "上下文压缩完成：Compacted 119 history items (~90334 tokens).",
        },
      ],
    });

    rerender(<ConversationTimeline snapshot={completed} onPermissionSelect={() => {}} />);

    expect(container.querySelector(".msg-context-compaction.is-completed")).not.toBeNull();
    // The redundant prefix is stripped; the summary detail is the label.
    expect(container.textContent).toContain("Compacted 119 history items (~90334 tokens).");
    expect(container.textContent).not.toContain("上下文压缩完成：");
    expect(container.querySelector(".msg-content-system")).toBeNull();

    const failed = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [{ id: "compact-start", role: "System", body: "上下文压缩失败：boom" }],
    });

    rerender(<ConversationTimeline snapshot={failed} onPermissionSelect={() => {}} />);

    expect(container.querySelector(".msg-context-compaction.is-failed")).not.toBeNull();
    expect(container.textContent).toContain("上下文压缩失败：boom");
    expect(container.querySelector(".msg-content-system")).toBeNull();
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

  it("hides execute tools while their permission request is shown near the composer", () => {
    const executeTool = makePermissionTool({
      kind: "execute",
      name: "`ls -la /g/kothbot/ 2>&1`",
      raw_input: JSON.stringify({ command: "ls -la /g/kothbot/ 2>&1" }),
    });
    const snapshot = makeSnapshot({
      timeline: [{ Tool: executeTool.id }],
      tools: [executeTool],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        hiddenPermissionRequestIds={new Set([executeTool.call_id])}
      />,
    );

    expect(container.textContent).not.toContain("ls -la /g/kothbot");
    expect(container.textContent).not.toContain("Allow");
  });

  it("hides resolved permission request tools from the timeline", () => {
    const permissionTool = makePermissionTool({
      status: "Succeeded",
      summary: "Permission resolved: allow",
      detail_text: "Permission 等待权限 | allow / allowAll / deny",
      permission_options: [],
      permission_decision: "Permission resolved: allow",
    });
    const snapshot = makeSnapshot({
      timeline: [{ Tool: permissionTool.id }],
      tools: [permissionTool],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
      />,
    );

    expect(container.textContent).not.toContain("已运行");
    expect(container.textContent).not.toContain("Permission resolved: allow");
    expect(container.textContent).not.toContain("等待权限");
  });

  it("collapses earlier same-turn replies and tools before the final assistant response", () => {
    const shellTool = makePermissionTool({
      id: "tool-shell",
      call_id: "shell-1",
      kind: "execute",
      name: "`pnpm test`",
      summary: "pnpm test",
      status: "Succeeded",
      raw_input: JSON.stringify({ command: "pnpm test" }),
      raw_output: "ok",
      permission_options: [],
    });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [
        { Message: "user-1" },
        { Message: "assistant-mid" },
        { Tool: shellTool.id },
        { Message: "assistant-final" },
      ],
      messages: [
        {
          id: "user-1",
          role: "User",
          body: "do it",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-mid",
          role: "Assistant",
          body: "intermediate reply",
          created_at: "2026-05-12T00:00:05Z",
        },
        {
          id: "assistant-final",
          role: "Assistant",
          body: "final answer",
          created_at: "2026-05-12T00:01:05Z",
        },
      ],
      tools: [shellTool],
    });

    const { container, getByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).toContain("已处理 1 次工具调用 · 1m 5s");
    expect(container.textContent).toContain("final answer");
    // Intermediate same-turn replies collapse into the turn summary together
    // with the tools — expanded content only appears after expanding.
    expect(container.textContent).not.toContain("intermediate reply");
    expect(container.textContent).not.toContain("pnpm test");

    fireEvent.click(getByRole("button", { name: "展开已处理上下文" }));

    expect(container.textContent).toContain("intermediate reply");
    expect(container.textContent).toContain("pnpm test");
  });

  it("collapses tool calls that arrive after the final assistant response", () => {
    const editTool = makePermissionTool({
      id: "tool-edit-after-final",
      call_id: "edit-after-final",
      kind: "execute",
      name: "Edit package.json",
      summary: "edit package.json",
      status: "Succeeded",
      raw_input: JSON.stringify({ command: "edit package.json" }),
      raw_output: "ok",
      permission_options: [],
    });
    const testTool = makePermissionTool({
      id: "tool-test-after-final",
      call_id: "test-after-final",
      kind: "execute",
      name: "`pnpm test`",
      summary: "pnpm test",
      status: "Succeeded",
      raw_input: JSON.stringify({ command: "pnpm test" }),
      raw_output: "ok",
      permission_options: [],
    });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [
        { Message: "user-after-tools" },
        { Message: "assistant-final" },
        { Tool: editTool.id },
        { Tool: testTool.id },
      ],
      messages: [
        {
          id: "user-after-tools",
          role: "User",
          body: "fix it",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-final",
          role: "Assistant",
          body: "final answer",
          created_at: "2026-05-12T00:01:05Z",
        },
      ],
      tools: [editTool, testTool],
    });

    const { container, getByRole } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "assistant-final": makeTurnChangeSet("turn-final", [
            makeFileSummary("package.json", 2, 1, "turn-final"),
          ]),
        }}
      />,
    );

    const collapsedText = container.textContent ?? "";
    expect(collapsedText).toContain("final answer");
    expect(collapsedText).toContain("已编辑 1 个文件");
    expect(collapsedText).not.toContain("edit package.json");
    expect(collapsedText).not.toContain("pnpm test");
    expect(collapsedText.indexOf("final answer")).toBeLessThan(
      collapsedText.indexOf("已编辑 1 个文件"),
    );

    fireEvent.click(getByRole("button", { name: "展开已处理上下文" }));

    // Inside the expanded turn the two calls form one activity group (they are
    // contiguous), so the rows are one click further in. The tool named
    // "Edit package.json" is classified as an edit, hence the file count.
    const expandedText = container.textContent ?? "";
    expect(expandedText).toContain("已运行 ×1 · 已编辑 1 个文件");
    expect(expandedText).not.toContain("edit package.json");

    fireEvent.click(getByRole("button", { name: /展开已运行 ×1 · 已编辑 1 个文件/ }));

    const groupText = container.textContent ?? "";
    expect(groupText).toContain("edit package.json");
    expect(groupText).toContain("pnpm test");
    expect(groupText.indexOf("final answer")).toBeLessThan(
      groupText.indexOf("edit package.json"),
    );
  });

  it("keeps a turn's changes bar below a trailing call that has no sibling to group with", () => {
    // Interrupting a turn mid-flight leaves the call the user stopped as the
    // turn's LAST item: after the closing reply, and alone, so it renders as a
    // raw row instead of an activity group. The changes bar belongs to the turn
    // (it is the turn's footer), so it must not land between the two — that put
    // "已编辑 2 个文件" in the middle of the turn with the agent's last failed
    // call hanging below it.
    const stoppedTool = makePermissionTool({
      id: "tool-stopped-ssh",
      call_id: "stopped-ssh",
      kind: "execute",
      name: "Execute",
      summary: "ssh export",
      status: "Failed",
      raw_input: JSON.stringify({
        command: "ssh -n -o BatchMode=yes 9.134.231.95 'cd /data/workspace/admesh'",
      }),
      raw_output: null,
      error: "Command failed",
      permission_options: [],
    });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [
        { Message: "user-export" },
        { Message: "assistant-export" },
        { Tool: stoppedTool.id },
      ],
      messages: [
        {
          id: "user-export",
          role: "User",
          body: "跑导出",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-export",
          role: "Assistant",
          body: "跑导出。",
          created_at: "2026-05-12T00:01:05Z",
        },
      ],
      tools: [stoppedTool],
    });

    const { container, getByRole } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "assistant-export": makeTurnChangeSet("turn-export", [
            makeFileSummary("scripts/export_x1_meshes.py", 212, 0, "turn-export"),
          ]),
        }}
      />,
    );

    // Folded: the stopped call is part of the turn summary, the bar closes it.
    const collapsedText = container.textContent ?? "";
    expect(container.querySelectorAll(".changes-bar")).toHaveLength(1);
    expect(collapsedText).not.toContain("BatchMode");
    expect(collapsedText.indexOf("跑导出。")).toBeLessThan(
      collapsedText.indexOf("export_x1_meshes.py"),
    );

    fireEvent.click(getByRole("button", { name: "展开已处理上下文" }));

    const expandedText = container.textContent ?? "";
    const replyAt = expandedText.indexOf("跑导出。");
    const toolAt = expandedText.indexOf("BatchMode");
    const barAt = expandedText.indexOf("export_x1_meshes.py");
    expect(replyAt).toBeGreaterThan(-1);
    expect(toolAt).toBeGreaterThan(replyAt);
    expect(barAt).toBeGreaterThan(toolAt);
    expect(container.querySelectorAll(".changes-bar")).toHaveLength(1);
  });

  it("collapses completed turns when the user prompt is outside the visible window", () => {
    const intermediateMessages = Array.from({ length: 83 }, (_, index) => ({
      id: `assistant-mid-${index}`,
      role: "Assistant" as const,
      body: `visible intermediate ${index}`,
    }));
    const timeline: TimelineItem[] = [
      { Message: "user-long" },
      ...intermediateMessages.map((message): TimelineItem => ({ Message: message.id })),
      { Message: "assistant-final" },
    ];
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline,
      messages: [
        {
          id: "user-long",
          role: "User",
          body: "run a long task",
          created_at: "2026-05-12T00:00:00Z",
        },
        ...intermediateMessages,
        {
          id: "assistant-final",
          role: "Assistant",
          body: "final long answer",
          created_at: "2026-05-12T00:01:30Z",
        },
      ],
    });

    const { container, unmount } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).toContain("已处理 · 1m 30s");
    expect(container.textContent).toContain("final long answer");
    // Intermediate assistant narration collapses into the turn summary; the
    // summary sits directly between the (windowed-out) user prompt and the
    // final reply instead of floating mid-prose.
    expect(container.textContent).not.toContain("visible intermediate 10");

    // The collapsed items (intermediate replies) are reachable via the
    // "展开已处理上下文" toggle.
    expect(
      container.querySelector(".timeline-collapse-toggle"),
    ).not.toBeNull();
    unmount();
  });

  it("shows a completed turn divider even without collapsible tool context", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [{ Message: "user-1" }, { Message: "assistant-final" }],
      messages: [
        {
          id: "user-1",
          role: "User",
          body: "hello",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-final",
          role: "Assistant",
          body: "hi",
          created_at: "2026-05-12T00:00:04Z",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".timeline-turn-summary.is-completed")?.textContent).toContain(
      "已处理 · 4s",
    );
    expect(container.querySelector(".timeline-collapse-toggle")).toBeNull();
  });

  it("formats completed turn durations from numeric epoch timestamps", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [{ Message: "user-1" }, { Message: "assistant-final" }],
      messages: [
        {
          id: "user-1",
          role: "User",
          body: "hello",
          created_at: "1710000000000",
        },
        {
          id: "assistant-final",
          role: "Assistant",
          body: "hi",
          created_at: "1710000065",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".timeline-turn-summary.is-completed")?.textContent).toContain(
      "已处理 · 1m 5s",
    );
  });

  it("keeps active streaming turn context expanded until the turn finishes", async () => {
    const shellTool = makePermissionTool({
      id: "tool-stream-shell",
      call_id: "stream-shell-1",
      kind: "execute",
      name: "`cargo test`",
      summary: "cargo test",
      status: "Succeeded",
      raw_input: JSON.stringify({ command: "cargo test" }),
      permission_options: [],
    });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Streaming",
      },
      timeline: [
        { Message: "user-stream" },
        { Tool: shellTool.id },
        { Message: "assistant-streaming" },
      ],
      messages: [
        {
          id: "user-stream",
          role: "User",
          body: "run tests",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-streaming",
          role: "Assistant",
          body: "**live** final",
          created_at: "2026-05-12T00:00:30Z",
        },
      ],
      tools: [shellTool],
    });

    const { container, queryByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).toContain("live final");
    expect(container.textContent).toContain("cargo test");
    expect(container.textContent).toContain("正在处理");
    expect(queryByRole("button", { name: "展开已处理上下文" })).toBeNull();
    await waitFor(() => {
      expect(container.querySelector(".msg-streaming-markdown .md-bold")?.textContent).toBe(
        "live",
      );
    });
  });

  it("keeps a long running turn unfolded, collapsing only the tool runs", () => {
    // While the turn runs there is no turn-level summary at all: narration stays
    // readable and the only folding is the per-run activity groups. The top
    // collapse appears when the turn ends (see the completed-turn tests).
    const tools = Array.from({ length: 12 }, (_, i) =>
      makePermissionTool({
        id: `tool-run-${i}`,
        call_id: `run-${i}`,
        kind: "execute",
        name: `\`echo ${i}\``,
        summary: `echo ${i}`,
        status: "Succeeded",
        raw_input: JSON.stringify({ command: `echo ${i}` }),
        permission_options: [],
      }),
    );
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Streaming" },
      timeline: [
        { Message: "user-long" },
        ...tools.slice(0, 6).map((tool) => ({ Tool: tool.id })),
        { Message: "assistant-first" },
        ...tools.slice(6).map((tool) => ({ Tool: tool.id })),
        { Message: "assistant-last" },
      ],
      messages: [
        {
          id: "user-long",
          role: "User",
          body: "run everything",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "assistant-first",
          role: "Assistant",
          body: "first narration",
          created_at: "2026-05-12T00:00:10Z",
        },
        {
          id: "assistant-last",
          role: "Assistant",
          body: "still working",
          created_at: "2026-05-12T00:00:30Z",
        },
      ],
      tools,
    });

    const { container, queryByRole, getAllByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    // No top-level collapse while the turn is live.
    expect(queryByRole("button", { name: "展开已处理上下文" })).toBeNull();
    // Neither narration segment is folded away.
    expect(container.textContent).toContain("first narration");
    expect(container.textContent).toContain("still working");

    // The twelve contiguous calls became two collapsed activity summaries, so
    // no individual tool row is rendered yet.
    const summaries = container.querySelectorAll(".tool-activity-summary");
    expect(summaries).toHaveLength(2);
    expect(container.querySelectorAll(".tc-header-line")).toHaveLength(0);

    // Expanding one group reveals exactly its own six calls.
    fireEvent.click(getAllByRole("button", { name: /展开已运行 ×6/ })[0]);
    expect(container.querySelectorAll(".tc-header-line")).toHaveLength(6);
    expect(container.textContent).toContain("echo 0");
    expect(container.textContent).not.toContain("echo 6");
  });

  it("updates active turn processing duration while the turn is running", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-12T00:00:09Z"));
    const shellTool = makePermissionTool({
      id: "tool-active-shell",
      call_id: "active-shell-1",
      kind: "execute",
      name: "`pnpm test`",
      summary: "pnpm test",
      status: "Running",
      raw_input: JSON.stringify({ command: "pnpm test" }),
      permission_options: [],
    });
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "WaitingForTool",
      },
      timeline: [{ Message: "user-active" }, { Tool: shellTool.id }],
      messages: [
        {
          id: "user-active",
          role: "User",
          body: "run tests",
          created_at: "2026-05-12T00:00:00Z",
        },
      ],
      tools: [shellTool],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".timeline-turn-summary.is-active")?.textContent).toContain(
      "正在处理 9s",
    );

    await act(async () => {
      vi.advanceTimersByTime(52_000);
    });

    expect(container.querySelector(".timeline-turn-summary.is-active")?.textContent).toContain(
      "正在处理 1m 1s",
    );
  });

  it("keeps the active-turn timer anchored to the original user message when a steer enters the timeline", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-12T00:00:09Z"));
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Streaming",
      },
      timeline: [
        { Message: "user-original" },
        { Message: "steer-1" },
      ],
      messages: [
        {
          id: "user-original",
          role: "User",
          body: "fix the bug",
          created_at: "2026-05-12T00:00:00Z",
        },
        {
          id: "steer-1",
          role: "User",
          body: "追加：先看日志",
          is_steer: true,
          created_at: "2026-05-12T00:00:05Z",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    // The active-turn summary should be anchored right after the ORIGINAL
    // user message (9s elapsed from 00:00:00), not after the steer
    // (which would be 4s from 00:00:05).
    const summaries = container.querySelectorAll(".timeline-turn-summary.is-active");
    expect(summaries).toHaveLength(1);
    expect(summaries[0].textContent).toContain("正在处理 9s");

    // The summary must be rendered right after the original user message,
    // before the steer — not jumped to the steer's position.
    const allTurnItems = container.querySelectorAll(
      ".timeline-item-wrapper, .msg-steer, .timeline-turn-summary",
    );
    let summaryIndex = -1;
    let steerIndex = -1;
    allTurnItems.forEach((el, idx) => {
      if (el.classList.contains("timeline-turn-summary")) summaryIndex = idx;
      if (el.classList.contains("msg-steer")) steerIndex = idx;
    });
    expect(summaryIndex).toBeGreaterThanOrEqual(0);
    expect(steerIndex).toBeGreaterThanOrEqual(0);
    expect(summaryIndex).toBeLessThan(steerIndex);

    // Advance time — the timer must continue from the original start, not
    // restart when the steer was inserted.
    await act(async () => {
      vi.advanceTimersByTime(52_000);
    });

    expect(container.querySelector(".timeline-turn-summary.is-active")?.textContent).toContain(
      "正在处理 1m 1s",
    );
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

  it("copies a completed assistant message as markdown", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

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
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "结论：已完成。\n\n```ts\nconst x = 1;\n```",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".streaming-cursor")).toBeNull();
    const copyButton = within(container).getByRole("button", { name: "复制回复文本" });

    fireEvent.click(copyButton);

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledTimes(1);
    });
    expect(writeText.mock.calls[0][0]).toContain("结论：已完成。");
    expect(writeText.mock.calls[0][0]).toContain("const x = 1;");
    expect(within(container).getByRole("button", { name: "已复制" })).toBeInTheDocument();

    Reflect.deleteProperty(navigator, "clipboard");
  });

  it("does not show the copy button while an assistant message is streaming", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "still going" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(within(container).queryByRole("button", { name: "复制回复文本" })).toBeNull();
  });

  it("shows copy and fork actions under each turn's final assistant reply", () => {
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
      timeline: [{ Message: "u-1" }, { Message: "a-1" }, { Message: "u-2" }, { Message: "a-2" }],
      messages: [
        { id: "u-1", role: "User", body: "第一个问题" },
        { id: "a-1", role: "Assistant", body: "第一轮的回复" },
        { id: "u-2", role: "User", body: "第二个问题" },
        { id: "a-2", role: "Assistant", body: "第二轮的回复" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
      />,
    );

    // 每轮的收尾回复各有一组操作行（共 2 组）。
    expect(
      within(container).queryAllByRole("button", { name: "复制回复文本" }),
    ).toHaveLength(2);
    expect(within(container).getAllByRole("button", { name: "分叉对话" })).toHaveLength(2);
    for (const anchorId of ["a-1", "a-2"]) {
      const row = container.querySelector(`[data-message-id="${anchorId}"]`);
      expect(row?.querySelector(".msg-assistant-actions")).not.toBeNull();
      expect(row?.querySelector(".msg-copy-btn")).not.toBeNull();
      expect(row?.querySelector(".msg-fork-btn")).not.toBeNull();
    }
  });

  it("keeps one activity group across a thinking segment", () => {
    // Reported: reasoning between two commands split one stretch of work into
    // several summaries, so the same run rendered as several collapsed rows with
    // single calls between them. Thinking renders nothing, so it must not break
    // the run.
    const tools = Array.from({ length: 4 }, (_, i) =>
      makePermissionTool({
        id: `tool-think-${i}`,
        call_id: `think-${i}`,
        kind: "execute",
        name: `\`echo ${i}\``,
        summary: `echo ${i}`,
        status: "Succeeded",
        raw_input: JSON.stringify({ command: `echo ${i}` }),
        permission_options: [],
      }),
    );
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [
        { Message: "user-think" },
        { Tool: tools[0].id },
        { Thinking: { text: "先看看现状" } },
        "Thinking",
        { Tool: tools[1].id },
        { Tool: tools[2].id },
        { Thinking: { text: "再核对一遍" } },
        { Tool: tools[3].id },
        { Message: "assistant-think" },
      ],
      messages: [
        { id: "user-think", role: "User", body: "跑一下" },
        { id: "assistant-think", role: "Assistant", body: "跑完了。" },
      ],
      tools,
    });

    const { container, getAllByRole, getByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    // The finished turn folds its work behind the turn summary; expanding it is
    // where the reported layout appeared (rows and summaries interleaved).
    fireEvent.click(getByRole("button", { name: "展开已处理上下文" }));

    const summaries = container.querySelectorAll(".tool-activity-summary");
    expect(summaries).toHaveLength(1);
    expect(summaries[0].textContent).toContain("已运行 ×4");
    // Collapsed: none of the four rows is mounted yet.
    expect(container.querySelectorAll(".tc-header-line")).toHaveLength(0);

    fireEvent.click(getAllByRole("button", { name: /展开已运行 ×4/ })[0]);
    expect(container.querySelectorAll(".tc-header-line")).toHaveLength(4);
  });

  it("opens a handoff briefing from the icon next to fork", async () => {
    const onHandoff = vi.fn().mockResolvedValue(undefined);
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "u-1" }, { Message: "a-1" }],
      messages: [
        { id: "u-1", role: "User", body: "把会话列表项目行改成呼吸灯" },
        { id: "a-1", role: "Assistant", body: "改好了。" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
        onHandoff={onHandoff}
      />,
    );

    const row = container.querySelector('[data-message-id="a-1"]');
    expect(row?.querySelector(".msg-handoff-btn")).not.toBeNull();

    fireEvent.click(within(container).getByRole("button", { name: "交接给下一个智能体" }));

    // The briefing is built locally from the conversation window.
    const textarea = (await screen.findByLabelText("交接说明")) as HTMLTextAreaElement;
    expect(textarea.value).toContain("# 交接说明（来自上一个会话）");
    expect(textarea.value).toContain("把会话列表项目行改成呼吸灯");

    fireEvent.click(screen.getByRole("button", { name: "新建会话并带入" }));
    await waitFor(() =>
      expect(vi.mocked(onHandoff).mock.calls[0][0]).toContain("把会话列表项目行改成呼吸灯"),
    );
    // The dialog passes the picked agent through to the new session.
    expect(vi.mocked(onHandoff).mock.calls[0][1]).toBe("codex-acp");
  });

  it("hides the handoff affordance when no handoff handler is wired", () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "u-1" }, { Message: "a-1" }],
      messages: [
        { id: "u-1", role: "User", body: "问题" },
        { id: "a-1", role: "Assistant", body: "回复" },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-handoff-btn")).toBeNull();
  });

  it("keeps intermediate same-turn replies free of action rows", () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "u-1" }, { Message: "a-1" }, { Message: "a-1b" }],
      messages: [
        { id: "u-1", role: "User", body: "问题" },
        { id: "a-1", role: "Assistant", body: "第一轮的中间回复" },
        { id: "a-1b", role: "Assistant", body: "第一轮的收尾回复" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
      />,
    );

    // 同轮多条回复时只有收尾回复有操作行；中间条目即使渲染也不带按钮。
    expect(
      within(container).queryAllByRole("button", { name: "复制回复文本" }),
    ).toHaveLength(1);
    const midRow = container.querySelector('[data-message-id="a-1"]');
    // 中间回复可能被折叠不渲染（query 为 null）——两种情况都不该有操作行。
    expect(midRow?.querySelector(".msg-assistant-actions") ?? null).toBeNull();
    const finalRow = container.querySelector('[data-message-id="a-1b"]');
    expect(finalRow?.querySelector(".msg-assistant-actions")).not.toBeNull();
  });

  it("shows no action row anywhere while the turn is still running", () => {
    // 轮次进行中（工具执行、状态 WaitingForTool）：中间文本段哪怕已经写完，
    // 也不长出复制/分叉按钮 —— 操作行只属于已完成轮次的收尾回复。
    const runningTool = makePermissionTool({
      id: "tool-1",
      call_id: "call-1",
      status: "Running",
      permission_input: null,
    });
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "WaitingForTool" },
      timeline: [{ Message: "u-1" }, { Message: "a-1" }, { Tool: runningTool.id }],
      messages: [
        { id: "u-1", role: "User", body: "问题" },
        { id: "a-1", role: "Assistant", body: "工具前的中间回复" },
      ],
      tools: [runningTool],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
      />,
    );

    expect(
      within(container).queryAllByRole("button", { name: "复制回复文本" }),
    ).toHaveLength(0);
    expect(within(container).queryAllByRole("button", { name: "分叉对话" })).toHaveLength(0);
  });

  it("opens the fork point picker listing every turn of the session", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "a-2" }],
      messages: [{ id: "a-2", role: "Assistant", body: "done" }],
    });
    vi.mocked(sessionForkCandidates).mockResolvedValue([
      {
        turn_ordinal: 1,
        user_message_id: "u-1",
        user_excerpt: "第一轮的问题",
        reply_excerpt: "第一轮的回复",
      },
      {
        turn_ordinal: 2,
        user_message_id: "u-2",
        user_excerpt: "第二轮的问题",
        reply_excerpt: "第二轮的回复",
      },
    ]);

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
        forkWorktreeSupported
      />,
    );

    fireEvent.click(within(container).getByRole("button", { name: "分叉对话" }));

    // 选择器列出全量轮次（来自后端完整历史，而非当前 UI 窗口）。
    const dialog = await screen.findByRole("dialog", { name: "从这里创建聊天分支" });
    await waitFor(() => {
      expect(within(dialog).getAllByRole("option")).toHaveLength(2);
    });
    expect(within(dialog).getByText("第一轮的问题")).toBeTruthy();
    expect(within(dialog).getByText("第二轮的问题")).toBeTruthy();
    // 两种分叉方式都在底部操作区。
    expect(within(dialog).getByRole("button", { name: "在此工作空间中创建分支" })).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "在新工作树中创建分支" })).toBeTruthy();
  });

  it("disables fork actions in the picker until a turn is selected and gates worktree support", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "a-2" }],
      messages: [{ id: "a-2", role: "Assistant", body: "done" }],
    });
    vi.mocked(sessionForkCandidates).mockResolvedValue([
      {
        turn_ordinal: 1,
        user_message_id: "u-1",
        user_excerpt: "第一轮的问题",
        reply_excerpt: "第一轮的回复",
      },
    ]);

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
      />,
    );

    fireEvent.click(within(container).getByRole("button", { name: "分叉对话" }));
    const dialog = await screen.findByRole("dialog", { name: "从这里创建聊天分支" });
    await waitFor(() => {
      expect(within(dialog).getAllByRole("option")).toHaveLength(1);
    });

    // 未选择轮次时两个分叉动作都不可用；worktree 不支持时始终禁用。
    const workspaceBtn = within(dialog).getByRole("button", { name: "在此工作空间中创建分支" });
    const worktreeBtn = within(dialog).getByRole("button", { name: "在新工作树中创建分支" });
    expect(workspaceBtn).toBeDisabled();
    expect(worktreeBtn).toBeDisabled();

    fireEvent.click(within(dialog).getByRole("option", { selected: false }));
    expect(workspaceBtn).not.toBeDisabled();
    expect(worktreeBtn).toBeDisabled();
  });

  it("hides the fork affordance entirely when the backend has no fork support", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "done" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(within(container).queryByRole("button", { name: "分叉对话" })).toBeNull();
  });

  it("invokes onForkConversation with the selected turn and mode from the picker", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "a-2" }],
      messages: [{ id: "a-2", role: "Assistant", body: "done" }],
    });
    const handleFork = vi.fn().mockResolvedValue(undefined);
    vi.mocked(sessionForkCandidates).mockResolvedValue([
      {
        turn_ordinal: 1,
        user_message_id: "u-1",
        user_excerpt: "第一轮的问题",
        reply_excerpt: "第一轮的回复",
      },
    ]);

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={handleFork}
        forkWorktreeSupported
      />,
    );

    fireEvent.click(within(container).getByRole("button", { name: "分叉对话" }));
    const dialog = await screen.findByRole("dialog", { name: "从这里创建聊天分支" });
    await waitFor(() => {
      expect(within(dialog).getAllByRole("option")).toHaveLength(1);
    });

    fireEvent.click(within(dialog).getByRole("option"));
    fireEvent.click(within(dialog).getByRole("button", { name: "在此工作空间中创建分支" }));
    await waitFor(() => {
      expect(handleFork).toHaveBeenCalledWith("u-1", "workspace");
    });

    // 成功后选择器关闭；重新打开并选择工作树分支。
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "从这里创建聊天分支" })).toBeNull();
    });
    fireEvent.click(within(container).getByRole("button", { name: "分叉对话" }));
    const reopened = await screen.findByRole("dialog", { name: "从这里创建聊天分支" });
    await waitFor(() => {
      expect(within(reopened).getAllByRole("option")).toHaveLength(1);
    });
    fireEvent.click(within(reopened).getByRole("option"));
    fireEvent.click(within(reopened).getByRole("button", { name: "在新工作树中创建分支" }));
    await waitFor(() => {
      expect(handleFork).toHaveBeenCalledWith("u-1", "worktree");
    });
  });

  it("keeps the fork picker open and shows the error when the fork fails", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "a-2" }],
      messages: [{ id: "a-2", role: "Assistant", body: "done" }],
    });
    const handleFork = vi.fn().mockRejectedValue(new Error("分叉失败：会话未完成"));
    vi.mocked(sessionForkCandidates).mockResolvedValue([
      {
        turn_ordinal: 1,
        user_message_id: "u-1",
        user_excerpt: "第一轮的问题",
        reply_excerpt: "第一轮的回复",
      },
    ]);

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={handleFork}
      />,
    );

    fireEvent.click(within(container).getByRole("button", { name: "分叉对话" }));
    const dialog = await screen.findByRole("dialog", { name: "从这里创建聊天分支" });
    await waitFor(() => {
      expect(within(dialog).getAllByRole("option")).toHaveLength(1);
    });

    fireEvent.click(within(dialog).getByRole("option"));
    fireEvent.click(within(dialog).getByRole("button", { name: "在此工作空间中创建分支" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert.textContent).toContain("分叉失败：会话未完成");
    // 失败后选择器保持打开，可重试。
    expect(screen.getByRole("dialog", { name: "从这里创建聊天分支" })).toBeTruthy();
  });

  it("anchors every assistant reply when the transcript has no user prompts", () => {
    // 退化转录：旧版分叉子会话在 user/message 回放修复之前重建，本地只有
    // 助手回复。此时每条非空助手回复各自成为操作行锚点，保持按钮可达。
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "a-1" }, { Message: "a-2" }, { Message: "a-3" }],
      messages: [
        { id: "a-1", role: "Assistant", body: "第一段回复" },
        { id: "a-2", role: "Assistant", body: "第二段回复" },
        { id: "a-3", role: "Assistant", body: "第三段回复" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onForkConversation={() => {}}
      />,
    );

    expect(
      within(container).queryAllByRole("button", { name: "复制回复文本" }),
    ).toHaveLength(3);
    expect(within(container).getAllByRole("button", { name: "分叉对话" })).toHaveLength(3);
    for (const anchorId of ["a-1", "a-2", "a-3"]) {
      const row = container.querySelector(`[data-message-id="${anchorId}"]`);
      expect(row?.querySelector(".msg-assistant-actions")).not.toBeNull();
    }
  });

  it("gates fork capability by agent, accepting both serde ids and display labels", () => {
    // 后端在会话行里可能存 serde id 也可能存显示标签（历史行），两种都要识别。
    expect(conversationForkCapability("deepseek-harness")).toEqual({
      forkSupported: true,
      worktreeSupported: false,
    });
    expect(conversationForkCapability("DeepSeek Harness")).toEqual({
      forkSupported: true,
      worktreeSupported: false,
    });
    expect(conversationForkCapability("codex-acp")).toEqual({
      forkSupported: true,
      worktreeSupported: true,
    });
    expect(conversationForkCapability("Codex")).toEqual({
      forkSupported: true,
      worktreeSupported: true,
    });
    // 其余后端（claude / codebuddy / goose / 未知 / 空）不显示分叉。
    expect(conversationForkCapability("claude-agent-acp").forkSupported).toBe(false);
    expect(conversationForkCapability("Claude").forkSupported).toBe(false);
    expect(conversationForkCapability("CodeBuddy").forkSupported).toBe(false);
    expect(conversationForkCapability(null).forkSupported).toBe(false);
    expect(conversationForkCapability(undefined).forkSupported).toBe(false);
  });

  it("does not let stale snapshot bodies overwrite newer streaming deltas", () => {
    replaceStreamingMessageBody("stream-store-heading", "\n\n##xxxx\n\n#### yy");

    expect(ensureStreamingMessageBody("stream-store-heading", "\n\n##")).toBe(
      "\n\n##xxxx\n\n#### yy",
    );
  });

  it("renders streamed compact heading deltas as markdown", async () => {
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
      timeline: [{ Message: "streaming-heading" }],
      messages: [{ id: "streaming-heading", role: "Assistant", body: "\n\n##" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    appendStreamingMessageDelta("streaming-heading", "xxxx\n\n#### yy");

    await waitFor(() => {
      expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("xxxx");
      expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("yy");
    });
    expect(container.querySelector(".msg-assistant")?.textContent).not.toContain("##xxxx");
  });

  it("preserves assistant soft line breaks while rendering each line as markdown", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "第一行\n**第二行**\n`第三行`",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelectorAll(".msg-assistant .md-line-break")).toHaveLength(2);
    expect(container.querySelector(".msg-assistant .md-bold")?.textContent).toBe("第二行");
    expect(container.querySelector(".msg-assistant .md-inline-code")?.textContent).toBe("第三行");
  });

  it("keeps pasted user terminal output as soft line breaks instead of per-line paragraphs", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "User",
          body:
            "4: 00007FF711A48D46 v8::Function::Experimental_IsNopFunction+3302\n" +
            "5: 00007FF7118A54A0 v8::internal::StrongRootAllocatorBase::StrongRootAllocatorBase+33904\n" +
            "6: 00007FF7118A1B2A v8::internal::StrongRootAllocatorBase::StrongRootAllocatorBase+19194",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelectorAll(".msg-user .md-paragraph")).toHaveLength(0);
    expect(container.querySelectorAll(".msg-user .md-line-break")).toHaveLength(0);
    expect(container.querySelectorAll(".msg-user .msg-user-text")).toHaveLength(1);
    expect(container.querySelector(".msg-user .msg-user-text")?.textContent).toBe(
      "4: 00007FF711A48D46 v8::Function::Experimental_IsNopFunction+3302\n" +
        "5: 00007FF7118A54A0 v8::internal::StrongRootAllocatorBase::StrongRootAllocatorBase+33904\n" +
        "6: 00007FF7118A1B2A v8::internal::StrongRootAllocatorBase::StrongRootAllocatorBase+19194",
    );
  });

  it("renders user CRLF line breaks as plain text without markdown paragraphs", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "User",
          body: "LLM 原始返回片段（前 400 字）\r\n这个有点不太够用啊",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelectorAll(".msg-user .md-paragraph")).toHaveLength(0);
    expect(container.querySelector(".msg-user .msg-user-text")?.textContent).toBe(
      "LLM 原始返回片段（前 400 字）\n这个有点不太够用啊",
    );
  });

  it("allows editing and retrying a failed user prompt before any assistant response", async () => {
    const onRetryUserMessage = vi.fn().mockResolvedValue(undefined);
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Interrupted",
      },
      timeline: [{ Message: "user-1" }, "Thinking", { Message: "system-1" }],
      messages: [
        { id: "user-1", role: "User", body: "old prompt" },
        { id: "system-1", role: "System", body: "会话已断开：boom" },
      ],
    });

    const { getByRole, getByLabelText } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onRetryUserMessage={onRetryUserMessage}
      />,
    );

    fireEvent.click(getByRole("button", { name: "编辑并重发" }));
    const textarea = getByLabelText("编辑用户消息") as HTMLTextAreaElement;
    expect(textarea.value).toBe("old prompt");
    fireEvent.change(textarea, { target: { value: "new prompt" } });
    fireEvent.click(getByRole("button", { name: "重新发送" }));

    await waitFor(() => {
      expect(onRetryUserMessage).toHaveBeenCalledWith("user-1", "new prompt");
    });
  });

  it("does not allow retry editing after the assistant has started replying", () => {
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [{ Message: "user-1" }, { Message: "assistant-1" }],
      messages: [
        { id: "user-1", role: "User", body: "old prompt" },
        { id: "assistant-1", role: "Assistant", body: "already replying" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onRetryUserMessage={vi.fn()}
      />,
    );

    expect(within(container).queryByRole("button", { name: "编辑并重发" })).toBeNull();
  });

  it("does not offer edit-and-resend for a /compact command message", () => {
    // A /compact command never receives a turn response — only system
    // compaction notices follow it, so the trailing-response heuristic would
    // otherwise render the retry affordance permanently and read as a failed
    // send.
    const snapshot = makeSnapshot({
      session: {
        ...makeSnapshot().session,
        status: "Idle",
      },
      timeline: [{ Message: "user-1" }, { Message: "system-1" }],
      messages: [
        { id: "user-1", role: "User", body: "/compact" },
        { id: "system-1", role: "System", body: "上下文压缩完成：Compacted 12 history items." },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onRetryUserMessage={vi.fn()}
      />,
    );

    expect(within(container).queryByRole("button", { name: "编辑并重发" })).toBeNull();
  });

  it("repairs compact headings without spaces across heading levels", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "前文\n##概览\n####细节",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant p")?.textContent).toBe("前文");
    expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("概览");
    expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("细节");
  });

  it("restores escaped markdown line breaks before parsing headings", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "\"\\n\\n##xxxx\\n\\n#### yy\"",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("xxxx");
    expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("yy");
  });

  it("parses compact headings after leading blank lines", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "\n\n##xxxx\n\n#### yy",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("xxxx");
    expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("yy");
    expect(container.querySelector(".msg-assistant")?.textContent).not.toContain("##xxxx");
  });

  it("repairs compact fenced code blocks from dropped whitespace chunks", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body:
            "来自 `docs/tags.md`：\n\n```textassets.subjectasset_structured_tags```\n\n" +
            "例如：\n\n```textassets.subject =角色asset_structured_tags:\n- style = 半写实- style = 奇幻- 性别 = 女-视图 = 半身```\n\n" +
            "可以关闭：\n\n```bashpnpm --filter @artassets/backend offline-tag-assets -- --no-legacy-tags```\n\n" +
            "不会写：\n\n```textvision:subject:*\nvision:style:*\nvision:mood:*\n```",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    const codeBlocks = container.querySelectorAll(".msg-assistant .md-code-block");
    expect(codeBlocks).toHaveLength(4);
    expect(codeBlocks[0].textContent).toContain("assets.subject");
    expect(codeBlocks[0].textContent).toContain("asset_structured_tags");
    expect(codeBlocks[1].textContent).toContain("assets.subject = 角色");
    expect(codeBlocks[1].textContent).toContain("- style = 半写实");
    expect(codeBlocks[1].textContent).toContain("- 视图 = 半身");
    expect(codeBlocks[2].textContent).toContain(
      "pnpm --filter @artassets/backend offline-tag-assets -- --no-legacy-tags",
    );
    expect(container.querySelector(".msg-assistant")?.textContent).not.toContain("半身```");
  });

  it("repairs escaped compact heading markers at line starts", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "\n\n\\#\\#xxxx\n\n\\#### yy",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("xxxx");
    expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("yy");
    expect(container.querySelector(".msg-assistant")?.textContent).not.toContain("##xxxx");
  });

  it("unwraps quoted markdown with literal line breaks before parsing headings", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "\"\n\n##xxxx\n\n#### yy\"",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant h2.md-heading")?.textContent).toBe("xxxx");
    expect(container.querySelector(".msg-assistant h4.md-heading")?.textContent).toBe("yy");
  });

  it("repairs compact numbered markdown lists from proxied model output", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body:
            "核心功能是：\n\n1. **多渠道接入** - 支持多个 IM渠道，统一收发消息2. **LLM驱动** - 通过 providers 层抽象响应3. **工具系统** - agent 可以调用工具",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelectorAll(".msg-assistant ol li")).toHaveLength(3);
  });

  it("repairs compact headings and markdown tables from proxied model output", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body:
            "###7.测试（基本覆盖）\n\n###总结|方面|评价||------|------||功能完整性|链路完整||风险|endpoint 变更需注意|",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant h3")?.textContent).toBe("7.测试（基本覆盖）");
    expect(container.querySelectorAll(".msg-assistant table tr")).toHaveLength(3);
  });

  it("repairs split compact markdown tables with space-separated rows", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body:
            "总结|方面 |评价 |\n|------|------| |功能完整性 | tool-use loop完整，guardrail + memory提取 + session管理齐全 | |代码重复 | 严重 - 三个入口函数的核心 while循环几乎是复制粘贴 | |并发安全 | process_mutex_全覆盖，安全但有吞吐量瓶颈 |",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".msg-assistant p")?.textContent).toBe("总结");
    expect(container.querySelectorAll(".msg-assistant table tr")).toHaveLength(4);
    expect(container.querySelector(".msg-assistant table")?.textContent).toContain("代码重复");
  });

  it("does not render undefined for empty fenced code blocks", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "前文\n\n```cppkabot\n```\n\n后文",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).not.toContain("undefined");
    expect(container.querySelector(".md-code-block")).toBeNull();
  });

  it("renders user image attachments outside the text bubble", () => {
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
    expect(userMessage?.querySelectorAll(".msg-user-image")).toHaveLength(2);
    expect(userMessage?.querySelector(".msg-user-image-strip")).toBeTruthy();
    expect(
      within(userMessage as HTMLElement).getByRole("button", { name: "预览 图1" }),
    ).toBeInTheDocument();
    expect(
      within(userMessage as HTMLElement).getByRole("button", { name: "预览 图2" }),
    ).toBeInTheDocument();
    expect(userMessage?.querySelector(".msg-user-bubble")?.textContent).toBe("› 看看这两张图");
    expect(userMessage?.querySelector(".msg-user-bubble .md-image")).toBeNull();
  });

  it("opens sent image attachments in a preview dialog", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "User",
          body: "看看\n\n![图1](data:image/png;base64,aaaa)",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    const currentTimeline = within(container);

    fireEvent.click(currentTimeline.getByRole("button", { name: "预览 图1" }));
    const dialog = within(document.body).getByRole("dialog", { name: "图片预览：图1" });
    expect(within(dialog).getByAltText("图1")).toHaveClass("msg-image-preview-original");

    fireEvent.click(within(dialog).getByRole("button", { name: "关闭图片预览" }));
    expect(within(document.body).queryByRole("dialog", { name: "图片预览：图1" })).not.toBeInTheDocument();
  });

  it("uses cached original file urls for sent image attachment previews", () => {
    vi.stubGlobal("isTauri", true);
    mockConvertFileSrc("macos");
    const originalUrl = "file:///Users/test/.kodex/attachments/original%20image.png";
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "User",
          body: `看看\n\n![图1](data:image/png;base64,thumb "${originalUrl}")`,
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    const currentTimeline = within(container);

    expect(currentTimeline.getByAltText("图1")).toHaveAttribute(
      "src",
      "data:image/png;base64,thumb",
    );

    fireEvent.click(currentTimeline.getByRole("button", { name: "预览 图1" }));
    const dialog = within(document.body).getByRole("dialog", { name: "图片预览：图1" });
    expect(within(dialog).getByAltText("图1")).toHaveAttribute(
      "src",
      "asset://localhost/%2FUsers%2Ftest%2F.kodex%2Fattachments%2Foriginal%20image.png",
    );
  });

  it("opens generated assistant images in the original preview dialog", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [
        {
          id: "msg-1",
          role: "Assistant",
          body: "![生成的图片](data:image/png;base64,generated)",
        },
      ],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );
    const currentTimeline = within(container);

    fireEvent.click(currentTimeline.getByRole("button", { name: "预览 生成的图片" }));
    const dialog = within(document.body).getByRole("dialog", { name: "图片预览：生成的图片" });
    expect(within(dialog).getByAltText("生成的图片")).toHaveAttribute(
      "src",
      "data:image/png;base64,generated",
    );

    fireEvent.click(within(dialog).getByRole("button", { name: "关闭图片预览" }));
    expect(within(document.body).queryByRole("dialog", { name: "图片预览：生成的图片" })).not.toBeInTheDocument();
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

  it("renders a turn changes bar after its final assistant segment", () => {
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
        { id: "msg-1", role: "Assistant", body: "先完成第一段说明" },
        { id: "msg-2", role: "Assistant", body: "再补充最后一段说明" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("src/turn.ts", 2, 1, "turn-msg-1"),
          ]),
        }}
      />,
    );

    const text = container.textContent ?? "";
    expect(text.indexOf("先完成第一段说明")).toBeLessThan(
      text.indexOf("再补充最后一段说明"),
    );
    expect(text.indexOf("再补充最后一段说明")).toBeLessThan(
      text.indexOf("已编辑 1 个文件"),
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

  it("keeps previous turn changes visible while a new turn is active", () => {
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
      timeline: [{ Message: "msg-1" }, { Message: "msg-2" }, { Message: "msg-3" }],
      messages: [
        { id: "msg-1", role: "Assistant", body: "previous done" },
        { id: "msg-2", role: "User", body: "next prompt" },
        { id: "msg-3", role: "Assistant", body: "working" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("src/previous.ts", 4, 2, "turn-msg-1"),
          ]),
          "msg-3": makeTurnChangeSet("turn-msg-3", [
            makeFileSummary("src/current.ts", 8, 1, "turn-msg-3"),
          ]),
        }}
      />,
    );

    expect(container.querySelectorAll(".changes-bar")).toHaveLength(1);
    expect(container.textContent).toContain("src/previous.ts");
    expect(container.textContent).not.toContain("src/current.ts");
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
      timeline: [
        { Message: "user-1" },
        { Message: "msg-1" },
        { Message: "user-2" },
        { Message: "msg-2" },
      ],
      messages: [
        { id: "user-1", role: "User", body: "first prompt" },
        { id: "msg-1", role: "Assistant", body: "first turn" },
        { id: "user-2", role: "User", body: "second prompt" },
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

  it("keeps a change set at the end of the assistant turn that produced it", () => {
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
    expect(text.indexOf("changed files")).toBeLessThan(text.indexOf("answered only"));
    expect(text.indexOf("answered only")).toBeLessThan(text.indexOf("changed.ts"));
    expect(text.lastIndexOf("changed.ts")).toBe(text.indexOf("changed.ts"));
  });

  it("keeps previous turn changes at the end of that turn", () => {
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
      timeline: [
        { Message: "user-1" },
        { Message: "msg-1" },
        { Message: "msg-2" },
      ],
      messages: [
        { id: "user-1", role: "User", body: "first prompt" },
        { id: "msg-1", role: "Assistant", body: "edited previous turn" },
        { id: "msg-2", role: "Assistant", body: "answered without edits" },
      ],
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        turnChangeSetsByMessageId={{
          "msg-1": makeTurnChangeSet("turn-msg-1", [
            makeFileSummary("previous.ts", 3, 1, "turn-msg-1"),
          ]),
        }}
      />,
    );

    expect(container.querySelectorAll(".changes-bar")).toHaveLength(1);
    const text = container.textContent ?? "";
    expect(text.indexOf("edited previous turn")).toBeLessThan(text.indexOf("answered without edits"));
    expect(text.indexOf("answered without edits")).toBeLessThan(text.indexOf("previous.ts"));
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

  it("follows streaming chunks when already near bottom", async () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "streaming-msg" }],
      messages: [{ id: "streaming-msg", role: "Assistant", body: "hello" }],
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 790;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    appendStreamingMessageDelta("streaming-msg", " **world**");
    // The streaming markdown commit is throttled (STREAMING_RENDER_MIN_INTERVAL_MS);
    // wait past the interval, then the double-rAF stick pass.
    await new Promise((resolve) => window.setTimeout(resolve, 250));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTop).toBe(1000);
    expect(container.querySelector(".msg-streaming-markdown .md-bold")?.textContent).toBe(
      "world",
    );
  });

  it("keeps manual scroll position instead of forcing bottom follow", async () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "hello" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 100;
    let scrollTopWrites = 0;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTopWrites += 1;
        scrollTop = value;
      },
    });
    fireEvent.wheel(scroller, { deltaY: -40 });
    fireEvent.scroll(scroller);
    const writesAfterManualScroll = scrollTopWrites;

    rerender(
      <ConversationTimeline
        snapshot={{ ...snapshot, revision: 2, thinking_status: "Active" }}
        onPermissionSelect={() => {}}
      />,
    );
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTopWrites).toBe(writesAfterManualScroll);
    expect(scrollTop).toBe(100);
  });

  it("does not re-pin an idle restored session to the bottom on re-renders", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "hello" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 100;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    // The user scrolled up and stopped mid-list.
    fireEvent.wheel(scroller, { deltaY: -40 });
    fireEvent.scroll(scroller);

    // A background revision bump on an idle session must not yank the
    // timeline back to the bottom — the user is browsing history.
    rerender(
      <ConversationTimeline
        snapshot={{ ...snapshot, revision: 2 }}
        onPermissionSelect={() => {}}
      />,
    );
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTop).toBe(100);
  });

  it("lands on a freshly submitted user message even after browsing up", async () => {
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline: [{ Message: "user-msg-1" }, { Message: "assistant-msg-1" }],
      messages: [
        { id: "user-msg-1", role: "User", body: "你好" },
        { id: "assistant-msg-1", role: "Assistant", body: "hello" },
      ],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 100;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    // The user scrolled up to re-read an earlier turn and stopped mid-list.
    fireEvent.wheel(scroller, { deltaY: -40 });
    fireEvent.scroll(scroller);

    // Submitting a prompt appends a new user message to the end of the
    // timeline. The view must move to it even though the user is mid-history.
    rerender(
      <ConversationTimeline
        snapshot={{
          ...snapshot,
          revision: 2,
          timeline: [
            { Message: "user-msg-1" },
            { Message: "assistant-msg-1" },
            { Message: "user-msg-2" },
          ],
          messages: [
            { id: "user-msg-1", role: "User", body: "你好" },
            { id: "assistant-msg-1", role: "Assistant", body: "hello" },
            { id: "user-msg-2", role: "User", body: "继续" },
          ],
        }}
        onPermissionSelect={() => {}}
      />,
    );
    await new Promise((resolve) => window.setTimeout(resolve, 100));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTop).toBe(1000);
  });

  it("stays pinned to bottom across unrelated parent re-renders", async () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "Assistant", body: "hello" }],
    });

    const { container, rerender } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    await new Promise((resolve) => requestAnimationFrame(resolve));

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 1000;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });

    // Simulate a layout nudge away from the true bottom (what Git/Files clicks
    // often leave behind), then an unrelated parent re-render.
    scrollTop = 820;
    fireEvent.scroll(scroller);

    rerender(
      <ConversationTimeline
        snapshot={{ ...snapshot, repository: { ...snapshot.repository, branch: "feature" } }}
        onPermissionSelect={() => {}}
      />,
    );
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTop).toBe(1000);
  });

  it("keeps following after programmatic scroll events and content resize", async () => {
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

    const scroller = container.querySelector(".timeline-scroll") as HTMLDivElement;
    let scrollTop = 100;
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });
    // Programmatic scroll events must not unpin sticky follow.
    fireEvent.scroll(scroller);

    triggerResize();
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));

    expect(scrollTop).toBe(1000);
    globalThis.ResizeObserver = OriginalResizeObserver;
  });
});

describe("ConversationTimeline – window paging trigger", () => {
  it("offers backend paging when the local window is exhausted but older history exists", async () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "User", body: "oldest loaded" }],
      history_total: 50,
      history_earliest_seq: 42,
    });
    const onLoadOlderHistory = vi.fn(() => Promise.resolve(true));

    const { getByRole } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={onLoadOlderHistory}
      />,
    );

    const button = getByRole("button", { name: "加载更早历史" });
    fireEvent.click(button);

    await waitFor(() => {
      expect(onLoadOlderHistory).toHaveBeenCalledWith(200);
    });
    // Let the async paging state settle so RTL cleanup unmounts a stable
    // tree; otherwise the post-unmount setState re-renders into the detached
    // container and leaks the button into later tests' document queries.
    await act(async () => {});
  });

  it("hides the backend paging button once the full history is loaded", () => {
    const snapshot = makeSnapshot({
      timeline: [{ Message: "msg-1" }],
      messages: [{ id: "msg-1", role: "User", body: "everything loaded" }],
      history_total: 1,
      history_earliest_seq: null,
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={() => Promise.resolve(true)}
      />,
    );

    // Scope to our own container: an async paging state update from the
    // previous test can leak a stale button into `document` (RTL cleanup is
    // synchronous), but it must never appear inside this component's tree.
    expect(
      Array.from(container.querySelectorAll(".timeline-load-older")).filter(
        (button) => button.textContent?.includes("加载更早历史"),
      ),
    ).toHaveLength(0);
  });

  it("never offers backend paging on an empty timeline even with a stale earliest seq", () => {
    // Regression: after the backend swap fix, a brand-new session's snapshot
    // carries history_earliest_seq: null. Guard regardless so a stale value
    // (e.g. from a patch-merged snapshot during session switching) can never
    // surface the "加载更早历史" button on an empty conversation.
    const snapshot = makeSnapshot({
      timeline: [],
      messages: [],
      history_total: 500,
      history_earliest_seq: 42,
    });

    const { container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={() => Promise.resolve(true)}
      />,
    );

    expect(
      Array.from(container.querySelectorAll(".timeline-load-older")).filter(
        (button) => button.textContent?.includes("加载更早历史"),
      ),
    ).toHaveLength(0);
  });

  it("prefers widening the local slice over paging while hidden items remain", () => {
    const timeline = Array.from({ length: 120 }, (_, index) => ({
      Message: `msg-${index}`,
    }));
    const messages = Array.from({ length: 120 }, (_, index) => ({
      id: `msg-${index}`,
      role: "User" as const,
      body: `body ${index}`,
    }));
    const snapshot = makeSnapshot({
      timeline,
      messages,
      history_total: 500,
      history_earliest_seq: 42,
    });
    const onLoadOlderHistory = vi.fn(() => Promise.resolve(true));

    const { getByRole, container } = render(
      <ConversationTimeline
        snapshot={snapshot}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={onLoadOlderHistory}
      />,
    );

    expect(
      Array.from(container.querySelectorAll(".timeline-load-older")).filter(
        (button) => button.textContent?.includes("加载更早历史"),
      ),
    ).toHaveLength(0);
    fireEvent.click(getByRole("button", { name: /更多对话/ }));
    expect(onLoadOlderHistory).not.toHaveBeenCalled();
  });

  it("collapses the final completed turn of a restored long session", () => {
    // Simulate a reopened session whose loaded window holds several turns and
    // ends with a finished turn: each turn is user → tools → assistant. The
    // initial render window (80) starts mid-conversation, so earlier turns'
    // user messages are above the visible slice — exactly like reopening a
    // long-history session.
    const timeline: TimelineItem[] = [];
    const messages: UiSnapshot["messages"] = [];
    const tools: ToolInvocation[] = [];
    for (let turn = 0; turn < 3; turn += 1) {
      const userId = `reopen-user-${turn}`;
      const toolId = `reopen-tool-${turn}`;
      const assistantId = `reopen-assistant-${turn}`;
      messages.push({
        id: userId,
        role: "User",
        body: `question ${turn}`,
        created_at: `2026-05-12T00:0${turn}:00Z`,
      });
      // Pad each turn with 30 tools so 3 turns exceed the 80-entry window.
      for (let index = 0; index < 30; index += 1) {
        tools.push(
          makePermissionTool({
            id: `${toolId}-${index}`,
            call_id: `${toolId}-${index}`,
            kind: "execute",
            name: `run ${turn}-${index}`,
            summary: `run ${turn}-${index}`,
            status: "Succeeded",
            permission_options: [],
          }),
        );
        timeline.push({ Tool: `${toolId}-${index}` });
      }
      messages.push({
        id: assistantId,
        role: "Assistant",
        body: `answer ${turn}`,
        created_at: `2026-05-12T00:0${turn}:30Z`,
      });
      timeline.splice(
        timeline.length - 30,
        0,
        { Message: userId },
      );
      timeline.push({ Message: assistantId });
    }
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline,
      messages,
      tools,
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    const collapseSummaries = container.querySelectorAll(".timeline-collapse-toggle");
    // Both fully visible completed turns should collapse naturally.
    expect(collapseSummaries.length).toBeGreaterThanOrEqual(2);
    // Restored process context must say what is hidden instead of looking lost.
    expect(collapseSummaries[0].textContent).toContain("30 次工具调用");
    // The last turn's intermediate tools are collapsed away…
    expect(container.textContent).not.toContain("run 2-5");
    // …while its final answer stays visible.
    expect(container.textContent).toContain("answer 2");
  });

  it("expands the initial window when the last turn is mostly hidden tools", () => {
    const timeline: TimelineItem[] = [];
    const messages: UiSnapshot["messages"] = [];
    const tools: ToolInvocation[] = [];
    messages.push({
      id: "hidden-tools-user",
      role: "User",
      body: "run the experiment",
      created_at: "2026-05-12T00:00:00Z",
    });
    timeline.push({ Message: "hidden-tools-user" });
    for (let index = 0; index < 120; index += 1) {
      const toolId = `hidden-tools-${index}`;
      tools.push(
        makePermissionTool({
          id: toolId,
          call_id: toolId,
          kind: "execute",
          name: `run ${index}`,
          summary: `run ${index}`,
          status: "Succeeded",
          permission_options: [],
        }),
      );
      timeline.push({ Tool: toolId });
    }
    messages.push({
      id: "hidden-tools-assistant",
      role: "Assistant",
      body: "experiment finished",
      created_at: "2026-05-12T00:02:00Z",
    });
    timeline.push({ Message: "hidden-tools-assistant" });

    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline,
      messages,
      tools,
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    expect(container.textContent).toContain("run the experiment");
    expect(container.textContent).toContain("experiment finished");
  });

  it("renders a turn-nav anchor on collapsed-turn summaries", () => {
    const timeline: TimelineItem[] = [];
    const messages: UiSnapshot["messages"] = [];
    const tools: ToolInvocation[] = [];
    messages.push({
      id: "collapsed-user",
      role: "User",
      body: "question",
      created_at: "2026-05-12T00:00:00Z",
    });
    for (let index = 0; index < 5; index += 1) {
      tools.push(
        makePermissionTool({
          id: `collapsed-tool-${index}`,
          call_id: `collapsed-tool-${index}`,
          kind: "execute",
          name: `run ${index}`,
          summary: `run ${index}`,
          status: "Succeeded",
          permission_options: [],
        }),
      );
      timeline.push({ Tool: `collapsed-tool-${index}` });
    }
    messages.push({
      id: "collapsed-answer",
      role: "Assistant",
      body: "answer",
      created_at: "2026-05-12T00:00:30Z",
    });
    timeline.splice(0, 0, { Message: "collapsed-user" });
    timeline.push({ Message: "collapsed-answer" });

    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline,
      messages,
      tools,
    });

    const { container } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    // A collapsed turn hides its user row, so the summary must carry the nav
    // anchor — otherwise the turn-nav ruler can never highlight the last
    // turns of a restored session and stays stuck a few turns back.
    const summary = container.querySelector(".timeline-collapse-toggle");
    expect(summary).not.toBeNull();
    expect(summary?.getAttribute("data-nav-user-id")).toBe("collapsed-user");
  });
});

describe("ConversationTimeline – turn-based history folding", () => {
  // 每轮 user → assistant（已完成轮次默认折叠为摘要行 + 收尾回复），以
  // 收尾回复文本（answer N）判定整轮是否渲染——折叠从不把轮次切成两半。
  function makeTurnSnapshot(turnCount: number) {
    const timeline: TimelineItem[] = [];
    const messages: UiSnapshot["messages"] = [];
    for (let turn = 0; turn < turnCount; turn += 1) {
      const hh = String(turn).padStart(2, "0");
      messages.push({
        id: `turn-user-${turn}`,
        role: "User",
        body: `question ${turn}`,
        created_at: `2026-05-12T${hh}:00:00Z`,
      });
      timeline.push({ Message: `turn-user-${turn}` });
      messages.push({
        id: `turn-assistant-${turn}`,
        role: "Assistant",
        body: `answer ${turn}`,
        created_at: `2026-05-12T${hh}:00:30Z`,
      });
      timeline.push({ Message: `turn-assistant-${turn}` });
    }
    return makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline,
      messages,
    });
  }

  it("shows 3 turns by default; each 更多对话 click reveals +6 turns, then doubles", () => {
    const { container, getByRole } = render(
      <ConversationTimeline
        snapshot={makeTurnSnapshot(12)}
        onPermissionSelect={() => {}}
      />,
    );

    // 默认展示最近 3 轮（第 9、10、11 轮），更早 9 轮收进「更多对话」。
    expect(container.textContent).toContain("answer 11");
    expect(container.textContent).toContain("answer 9");
    expect(container.textContent).not.toContain("answer 8");
    expect(container.textContent).not.toContain("answer 0");

    // 第一次点击 +6 轮 → 还有 3 轮。
    fireEvent.click(getByRole("button", { name: "更多对话（还有 9 轮）" }));
    expect(container.textContent).toContain("answer 3");
    expect(container.textContent).not.toContain("answer 2");

    // 第二次点击 +12 轮（6 翻倍）→ 全部可见，按钮消失。
    fireEvent.click(getByRole("button", { name: "更多对话（还有 3 轮）" }));
    expect(container.textContent).toContain("answer 0");
    expect(container.querySelectorAll(".timeline-load-older")).toHaveLength(0);
  });

  it("never slices mid-turn: a hidden turn hides its user, tools and reply together", () => {
    const timeline: TimelineItem[] = [];
    const messages: UiSnapshot["messages"] = [];
    const tools: ToolInvocation[] = [];
    for (let turn = 0; turn < 5; turn += 1) {
      const hh = String(turn).padStart(2, "0");
      messages.push({
        id: `mid-user-${turn}`,
        role: "User",
        body: `question ${turn}`,
        created_at: `2026-05-12T${hh}:00:00Z`,
      });
      timeline.push({ Message: `mid-user-${turn}` });
      tools.push(
        makePermissionTool({
          id: `mid-tool-${turn}`,
          call_id: `mid-tool-${turn}`,
          kind: "execute",
          name: `run ${turn}`,
          summary: `run ${turn}`,
          status: "Succeeded",
          permission_options: [],
        }),
      );
      timeline.push({ Tool: `mid-tool-${turn}` });
      messages.push({
        id: `mid-assistant-${turn}`,
        role: "Assistant",
        body: `answer ${turn}`,
        created_at: `2026-05-12T${hh}:00:30Z`,
      });
      timeline.push({ Message: `mid-assistant-${turn}` });
    }
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Idle" },
      timeline,
      messages,
      tools,
    });

    const { container, getByRole } = render(
      <ConversationTimeline snapshot={snapshot} onPermissionSelect={() => {}} />,
    );

    // 默认 3 轮（第 2、3、4 轮）：被隐藏的第 1 轮整体隐藏——用户消息、
    // 工具、收尾回复一起切走，不会出现"上半轮隐藏、下半轮可见"的切口。
    expect(container.textContent).toContain("answer 2");
    expect(container.textContent).not.toContain("answer 1");
    expect(container.textContent).not.toContain("question 1");
    expect(container.textContent).not.toContain("run 1");

    fireEvent.click(getByRole("button", { name: "更多对话（还有 2 轮）" }));
    // 展开后第 0、1 轮整轮回归（已完成轮次折叠为摘要 + 收尾回复）。
    expect(container.textContent).toContain("answer 1");
    expect(container.textContent).toContain("answer 0");
  });

  it("keeps the visible top aligned to a turn boundary across backend paging", () => {
    // 分页按「条」加载，页首可能落在轮次中间：开头的用户消息还在更早的
    // 一页里。只要还有未加载历史，这个切口不显示，可视边界对齐到第一个
    // 轮次开头；全部加载完毕后它作为真正的会话开场恢复显示。
    const makePagedSnapshot = (historyEarliestSeq: number | null) =>
      makeSnapshot({
        session: { ...makeSnapshot().session, status: "Idle" },
        timeline: [
          { Message: "cut-assistant" }, // 上一轮的切口（其开头在未加载页）
          { Message: "page-user-1" },
          { Message: "page-assistant-1" },
          { Message: "page-user-2" },
          { Message: "page-assistant-2" },
        ],
        messages: [
          {
            id: "cut-assistant",
            role: "Assistant",
            body: "orphan narration",
            created_at: "2026-05-12T00:30:00Z",
          },
          {
            id: "page-user-1",
            role: "User",
            body: "question 1",
            created_at: "2026-05-12T01:00:00Z",
          },
          {
            id: "page-assistant-1",
            role: "Assistant",
            body: "answer 1",
            created_at: "2026-05-12T01:00:30Z",
          },
          {
            id: "page-user-2",
            role: "User",
            body: "question 2",
            created_at: "2026-05-12T02:00:00Z",
          },
          {
            id: "page-assistant-2",
            role: "Assistant",
            body: "answer 2",
            created_at: "2026-05-12T02:00:30Z",
          },
        ],
        history_total: 500,
        history_earliest_seq: historyEarliestSeq,
      });

    const { container, getByRole, rerender } = render(
      <ConversationTimeline
        snapshot={makePagedSnapshot(42)}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={() => Promise.resolve(true)}
      />,
    );

    // 切口隐藏，可视内容从第一个轮次开头（question 1 那轮）开始。
    expect(container.textContent).not.toContain("orphan narration");
    expect(container.textContent).toContain("answer 2");
    expect(container.textContent).toContain("answer 1");
    // 本地已全部展开 → 仍可继续向后翻页让切口的开头加载进来。
    expect(getByRole("button", { name: "加载更早历史" })).toBeInTheDocument();

    // 全部历史加载完毕：切口恢复为真正的会话开场。
    rerender(
      <ConversationTimeline
        snapshot={makePagedSnapshot(null)}
        onPermissionSelect={() => {}}
        onLoadOlderHistory={() => Promise.resolve(true)}
      />,
    );
    expect(container.textContent).toContain("orphan narration");
    expect(
      Array.from(container.querySelectorAll(".timeline-load-older")),
    ).toHaveLength(0);
  });
});
