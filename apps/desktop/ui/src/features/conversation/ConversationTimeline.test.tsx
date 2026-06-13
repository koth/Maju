import { afterEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, waitFor, within } from "@testing-library/react";
import { clearMocks, mockConvertFileSrc } from "@tauri-apps/api/mocks";
import { ConversationTimeline, type TimelineTurnChangeSet } from "./ConversationTimeline";
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

afterEach(() => {
  clearMocks();
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
    permission_input: null,
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

  it("renders context compaction notices as divider rows", () => {
    const pending = makeSnapshot({
      timeline: [{ Message: "compact-start" }],
      messages: [{ id: "compact-start", role: "System", body: "正在压缩上下文" }],
    });

    const { container, getByRole, rerender } = render(
      <ConversationTimeline snapshot={pending} onPermissionSelect={() => {}} />,
    );

    expect(getByRole("status")).toHaveTextContent("正在压缩上下文");
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

  it("moves a stale same-turn change set after the later assistant message", () => {
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
    expect(text.indexOf("answered only")).toBeLessThan(text.indexOf("changed.ts"));
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
