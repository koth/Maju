import { describe, expect, it } from "vitest";
import type { ToolInvocation, UiSnapshot } from "../../types";
import { buildHandoffDigest } from "./handoff";

/** Minimal tool row: the digest reads kind, status, summary and diff paths. */
function tool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "t-1",
    call_id: "c-1",
    parent_call_id: null,
    name: "Read",
    kind: "read",
    summary: "",
    status: "Succeeded",
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
    can_stop: false,
    stop_kind: null,
    stop_status: null,
    ...overrides,
  };
}

/** Minimal snapshot: only the fields the digest reads are meaningful. */
function makeSnapshot(): UiSnapshot {
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
      status: "Idle",
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
  };
}

function withConversation(): UiSnapshot {
  const snapshot = makeSnapshot();
  return {
    ...snapshot,
    messages: [
      {
        id: "u-1",
        role: "User",
        body: "把会话列表的项目行改成折叠时右侧呼吸灯",
        created_at: "2026-05-12T00:00:00Z",
      },
      {
        id: "a-1",
        role: "Assistant",
        body: "改好了：折叠时右侧状态点脉冲。",
        created_at: "2026-05-12T00:01:00Z",
      },
      {
        id: "u-2",
        role: "User",
        body: "顺便把交接功能加上",
        created_at: "2026-05-12T00:02:00Z",
      },
    ],
    tools: [
      tool({
        id: "t-1",
        call_id: "c-1",
        name: "Edit",
        kind: "edit",
        summary: "编辑 SessionList.tsx",
        diff_paths: ["apps/desktop/ui/src/features/session/SessionList.tsx"],
      }),
      tool({
        id: "t-2",
        call_id: "c-2",
        name: "Bash",
        kind: "execute",
        summary: "cargo test",
        status: "Running",
      }),
    ],
  };
}

describe("buildHandoffDigest", () => {
  it("briefs the next agent on goals, progress and state", () => {
    const digest = buildHandoffDigest(withConversation());

    expect(digest).toContain("# 交接说明（来自上一个会话）");
    expect(digest).toContain("## 会话信息");
    expect(digest).toContain("- 会话标题：test");
    expect(digest).toContain("- 工作区：/test");
    expect(digest).toContain("- 分支：main");
    expect(digest).toContain("## 目标 / 用户请求");
    expect(digest).toContain("- 把会话列表的项目行改成折叠时右侧呼吸灯");
    expect(digest).toContain("- 顺便把交接功能加上");
    expect(digest).toContain("工具活动：编辑文件 ×1");
    expect(digest).toContain("改动的文件：");
    expect(digest).toContain("apps/desktop/ui/src/features/session/SessionList.tsx");
    expect(digest).toContain("上一个智能体的最新结论");
    expect(digest).toContain("会话状态：Idle");
    // An unfinished call is called out instead of being counted as progress.
    expect(digest).toContain("有 1 个工具调用尚未结束");
    expect(digest).not.toContain("运行命令 ×1");
  });

  it("carries the agent's own task list, open items first-class", () => {
    const snapshot = withConversation();
    const digest = buildHandoffDigest({
      ...snapshot,
      agent_plan: [
        { id: "p1", content: "改折叠摘要", priority: "high", status: "completed" },
        { id: "p2", content: "补单测", priority: "medium", status: "in_progress" },
        { id: "p3", content: "写文档", priority: "low", status: "pending" },
        { id: "p4", content: "已取消的项", priority: "low", status: "cancelled" },
      ],
    });

    expect(digest).toContain("## 任务清单（上一个智能体的计划）");
    expect(digest).toContain("- [x] 改折叠摘要");
    expect(digest).toContain("- [ ] 补单测");
    expect(digest).toContain("- [ ] 写文档");
    // Cancelled work is neither done nor a next step.
    expect(digest).not.toContain("已取消的项");
  });

  it("lists changed files with their diff size when the harness reported one", () => {
    const snapshot = withConversation();
    const digest = buildHandoffDigest({
      ...snapshot,
      tools: [
        tool({
          id: "t-diff",
          call_id: "c-diff",
          name: "Edit",
          kind: "edit",
          diff_paths: ["/repo/a.ts"],
          diff_previews: [
            {
              path: "/repo/a.ts",
              hunks: [
                {
                  heading: "@@ -1 +1 @@",
                  lines: [
                    { kind: "Removed", content: "old" },
                    { kind: "Added", content: "new" },
                    { kind: "Added", content: "newer" },
                  ],
                },
              ],
            },
          ],
        }),
      ],
    });

    expect(digest).toContain("/repo/a.ts (+2/-1)");
  });

  it("covers only the conversation through the selected turn", () => {
    const digest = buildHandoffDigest(withConversation(), "a-1");

    expect(digest).toContain("把会话列表的项目行改成折叠时右侧呼吸灯");
    // The later turn's request and tools are not part of this handoff.
    expect(digest).not.toContain("顺便把交接功能加上");
  });

  it("returns nothing for an empty conversation", () => {
    const snapshot = makeSnapshot();
    expect(buildHandoffDigest({ ...snapshot, messages: [] })).toBe("");
  });
});
