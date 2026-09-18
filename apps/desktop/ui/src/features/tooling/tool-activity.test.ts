import { describe, expect, it } from "vitest";
import type { TimelineItem, ToolInvocation } from "../../types";
import {
  buildToolActivityGroups,
  isGroupableTool,
  summarizeToolActivity,
} from "./tool-activity";

function tool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "Read",
    kind: "read",
    summary: "Read a file",
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

function commandTool(id: string, command: string, status: ToolInvocation["status"] = "Succeeded") {
  return tool({
    id,
    call_id: id,
    name: "Bash",
    kind: "execute",
    status,
    raw_input: JSON.stringify({ command }),
  });
}

function readTool(id: string, path: string) {
  return tool({ id, call_id: id, name: "Read", kind: "read", raw_input: JSON.stringify({ path }) });
}

function editTool(id: string, path: string) {
  return tool({
    id,
    call_id: id,
    name: "Edit",
    kind: "edit",
    raw_input: JSON.stringify({ file_path: path, old_string: "a", new_string: "b" }),
    diff_paths: [path],
  });
}

function toolsMap(items: ToolInvocation[]) {
  return new Map(items.map((entry) => [entry.id, entry]));
}

describe("tool activity grouping", () => {
  it("groups a contiguous run of reads and commands into one summary", () => {
    const tools = [
      readTool("a", "/repo/one.ts"),
      readTool("b", "/repo/two.ts"),
      commandTool("c", "rg -n needle src"),
    ];
    const timeline: TimelineItem[] = [
      { Message: "m1" },
      { Tool: "a" },
      { Tool: "b" },
      { Tool: "c" },
      { Message: "m2" },
    ];

    const { byStartIndex, memberIndexes } = buildToolActivityGroups(timeline, toolsMap(tools));

    const group = byStartIndex.get(1);
    expect(group).toBeDefined();
    expect(group?.tools.map((entry) => entry.id)).toEqual(["a", "b", "c"]);
    // One phrase per activity present, reads first because they came first.
    expect(group?.summary).toBe("已探索 ×2 · 已运行 ×1");
    expect([...memberIndexes].sort()).toEqual([2, 3]);
  });

  it("does not group a lone call, or a run broken by a message", () => {
    const tools = [readTool("a", "/repo/one.ts"), readTool("b", "/repo/two.ts")];
    const single: TimelineItem[] = [{ Tool: "a" }, { Message: "m1" }];
    expect(buildToolActivityGroups(single, toolsMap(tools)).byStartIndex.size).toBe(0);

    const split: TimelineItem[] = [
      { Tool: "a" },
      { Message: "m1" },
      { Tool: "b" },
    ];
    const splitGroups = buildToolActivityGroups(split, toolsMap(tools));
    expect(splitGroups.byStartIndex.size).toBe(0);
    expect(splitGroups.memberIndexes.size).toBe(0);
  });

  it("keeps failures, questions and permission rows out of groups", () => {
    const failed = commandTool("failed", "false", "Failed");
    const question = tool({
      id: "question",
      call_id: "question",
      name: "AskUserQuestion",
      kind: "ask",
      permission_input: { questions: [] },
    });

    for (const entry of [failed, question]) {
      expect(isGroupableTool(entry), entry.id).toBe(false);
    }

    const timeline: TimelineItem[] = [{ Tool: "failed" }, { Tool: "question" }];
    expect(
      buildToolActivityGroups(timeline, toolsMap([failed, question])).byStartIndex.size,
    ).toBe(0);
  });

  it("folds edits into the run and counts the files they touched", () => {
    // The screenshot case: edits and commands alternate in one stretch of work.
    // Every row belongs to the same group, and the edits are summarized by file
    // rather than by call.
    const tools = [
      editTool("e1", "/repo/scripts/one.py"),
      commandTool("c1", "scp -P 36000 scripts/one.py root@host:/data/"),
      editTool("e2", "/repo/scripts/two.py"),
      editTool("e3", "/repo/scripts/two.py"),
      readTool("r1", "/repo/scripts/two.py"),
    ];
    const timeline: TimelineItem[] = tools.map((entry) => ({ Tool: entry.id }));

    const { byStartIndex, memberIndexes } = buildToolActivityGroups(timeline, toolsMap(tools));

    const group = byStartIndex.get(0);
    expect(group?.tools.map((entry) => entry.id)).toEqual(["e1", "c1", "e2", "e3", "r1"]);
    // Two distinct files edited across three calls, plus the other activities.
    expect(group?.summary).toBe("已探索 ×1 · 已运行 ×1 · 已编辑 2 个文件");
    expect([...memberIndexes].sort()).toEqual([1, 2, 3, 4]);
  });

  it("does not let a thinking segment break the run", () => {
    // Reasoning renders nothing in the timeline (history folds it in), so it must
    // not split one stretch of work into two summaries.
    const tools = [readTool("a", "/repo/one.ts"), commandTool("b", "ls")];
    const timeline: TimelineItem[] = [
      { Tool: "a" },
      "Thinking" as unknown as TimelineItem,
      { Thinking: "thinking-1" } as unknown as TimelineItem,
      { Tool: "b" },
    ];

    const { byStartIndex, memberIndexes } = buildToolActivityGroups(timeline, toolsMap(tools));

    expect(byStartIndex.get(0)?.tools.map((entry) => entry.id)).toEqual(["a", "b"]);
    expect([...memberIndexes]).toEqual([3]);
  });

  it("leaves out rows the timeline would not render", () => {
    // Child calls render nested under their parent and a pending approval whose
    // panel is open elsewhere is hidden; neither may be folded into a group, or
    // expanding it would render a row the timeline hides.
    const tools = [
      readTool("a", "/repo/one.ts"),
      tool({ id: "child", call_id: "child", parent_call_id: "parent-call" }),
      readTool("b", "/repo/two.ts"),
    ];
    const timeline: TimelineItem[] = [{ Tool: "a" }, { Tool: "child" }, { Tool: "b" }];

    const withAll = buildToolActivityGroups(timeline, toolsMap(tools));
    expect(withAll.byStartIndex.size).toBe(0);

    const withoutChild = buildToolActivityGroups(
      timeline,
      toolsMap(tools),
      (entry) => entry.parent_call_id == null,
    );
    // The child breaks the run instead of being hidden inside a summary.
    expect(withoutChild.byStartIndex.size).toBe(0);
    expect(withoutChild.memberIndexes.size).toBe(0);
  });

  it("summarizes each activity with the same verb its rows use", () => {
    expect(summarizeToolActivity([commandTool("a", "ls"), commandTool("b", "pwd")])).toBe("已运行 ×2");
    expect(summarizeToolActivity([readTool("a", "/repo/one.ts")])).toBe("已探索 ×1");
    expect(
      summarizeToolActivity([
        readTool("a", "/repo/one.ts"),
        commandTool("b", "ls"),
        readTool("c", "/repo/two.ts"),
      ]),
    ).toBe("已探索 ×2 · 已运行 ×1");
    // An edit whose row reported no diff paths still counts as one file.
    expect(
      summarizeToolActivity([
        tool({ id: "e", call_id: "e", name: "Edit", kind: "edit" }),
        editTool("e2", "/repo/one.ts"),
      ]),
    ).toBe("已编辑 2 个文件");
  });
});
