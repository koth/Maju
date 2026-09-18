import { describe, expect, it } from "vitest";
import type { TimelineItem, ToolInvocation } from "../types";
import {
  buildToolActivityGroups,
  editedFileCount,
  isGroupableTool,
  summarizeToolActivity,
} from "../features/tooling/tool-activity";
import { deriveToolPresentation } from "../features/tooling/tool-presentation";

// The desktop grouping rules, ported. One session must collapse the same way
// on the phone as it does on the PC.

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

describe("mobile tool activity grouping", () => {
  it("groups a contiguous run of reads and commands into one summary", () => {
    const tools = [
      readTool("a", "/repo/one.ts"),
      readTool("b", "/repo/two.ts"),
      commandTool("c", "npm test"),
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
    expect(group?.summary).toBe("Searched ×2 · Ran ×1");
    expect([...memberIndexes].sort()).toEqual([2, 3]);
  });

  it("does not group a lone call, or a run broken by a message", () => {
    const tools = [readTool("a", "/repo/one.ts"), readTool("b", "/repo/two.ts")];
    const single: TimelineItem[] = [{ Tool: "a" }, { Message: "m1" }];
    expect(buildToolActivityGroups(single, toolsMap(tools)).byStartIndex.size).toBe(0);

    const split: TimelineItem[] = [{ Tool: "a" }, { Message: "m1" }, { Tool: "b" }];
    const splitGroups = buildToolActivityGroups(split, toolsMap(tools));
    expect(splitGroups.byStartIndex.size).toBe(0);
    expect(splitGroups.memberIndexes.size).toBe(0);
  });

  it("keeps failures, questions and permission rows out of groups", () => {
    const failed = commandTool("failed", "npm run build", "Failed");
    const interrupted = commandTool("interrupted", "npm run build", "Interrupted");
    const question = tool({
      id: "question",
      call_id: "question",
      name: "AskUserQuestion",
      kind: "ask",
      permission_input: { questions: [] },
    });
    const permission = tool({
      id: "permission",
      call_id: "permission",
      name: "Bash",
      kind: "permission",
      permission_input: { questions: [] },
    });

    for (const entry of [failed, interrupted, question, permission]) {
      expect(isGroupableTool(entry), entry.id).toBe(false);
    }

    const timeline: TimelineItem[] = [
      { Tool: "failed" },
      { Tool: "interrupted" },
      { Tool: "question" },
      { Tool: "permission" },
    ];
    expect(
      buildToolActivityGroups(timeline, toolsMap([failed, interrupted, question, permission]))
        .byStartIndex.size,
    ).toBe(0);
  });

  it("folds edits into the run and counts the files they touched", () => {
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
    expect(group?.summary).toBe("Searched ×1 · Ran ×1 · Edited 2 files");
    expect([...memberIndexes].sort()).toEqual([1, 2, 3, 4]);
  });

  it("does not let a thinking segment break the run", () => {
    const tools = [readTool("a", "/repo/one.ts"), commandTool("b", "ls")];
    const timeline: TimelineItem[] = [
      { Tool: "a" },
      "Thinking",
      { Thinking: { text: "thinking" } },
      { Tool: "b" },
    ];

    const { byStartIndex, memberIndexes } = buildToolActivityGroups(timeline, toolsMap(tools));

    expect(byStartIndex.get(0)?.tools.map((entry) => entry.id)).toEqual(["a", "b"]);
    expect([...memberIndexes]).toEqual([3]);
  });

  it("breaks the run on a child call instead of hiding it in a summary", () => {
    const tools = [
      readTool("a", "/repo/one.ts"),
      tool({ id: "child", call_id: "child", parent_call_id: "parent-call" }),
      readTool("b", "/repo/two.ts"),
    ];
    const timeline: TimelineItem[] = [{ Tool: "a" }, { Tool: "child" }, { Tool: "b" }];

    const groups = buildToolActivityGroups(timeline, toolsMap(tools));
    expect(groups.byStartIndex.size).toBe(0);
    expect(groups.memberIndexes.size).toBe(0);
  });

  it("ignores timeline ids that resolve to no tool", () => {
    const timeline: TimelineItem[] = [{ Tool: "a" }, { Tool: "ghost" }, { Tool: "b" }];
    const groups = buildToolActivityGroups(
      timeline,
      toolsMap([readTool("a", "/repo/one.ts"), readTool("b", "/repo/two.ts")]),
    );
    expect(groups.byStartIndex.size).toBe(0);
  });
});

describe("mobile activity summaries", () => {
  it("uses the same verb its rows use", () => {
    // `ls` is a read-only command, which the phone's row classifier has always
    // reported as exploration.
    expect(summarizeToolActivity([commandTool("a", "ls")])).toBe("Searched ×1");
    expect(summarizeToolActivity([commandTool("a", "npm test")])).toBe("Ran ×1");
    expect(
      summarizeToolActivity([commandTool("a", "npm test"), commandTool("b", "cargo build")]),
    ).toBe("Ran ×2");
    expect(summarizeToolActivity([readTool("a", "/repo/one.ts")])).toBe("Searched ×1");
    expect(
      summarizeToolActivity([
        readTool("a", "/repo/one.ts"),
        commandTool("b", "npm test"),
        readTool("c", "/repo/two.ts"),
      ]),
    ).toBe("Searched ×2 · Ran ×1");

    // The row verb and the summary verb must not drift apart: an edit row says
    // "Edited", so a collapsed run of edits must too.
    const edit = editTool("e", "/repo/one.ts");
    expect(deriveToolPresentation(edit).verb).toBe("Edited");
    expect(summarizeToolActivity([edit])).toBe("Edited 1 file");

    const read = readTool("r", "/repo/one.ts");
    expect(deriveToolPresentation(read).verb).toBe("Searched");
    expect(summarizeToolActivity([read])).toBe("Searched ×1");
  });

  it("singularises a one-file edit and counts path-less edits as files", () => {
    expect(summarizeToolActivity([editTool("e", "/repo/one.ts")])).toBe("Edited 1 file");
    expect(
      summarizeToolActivity([
        tool({ id: "e1", call_id: "e1", name: "Edit", kind: "edit" }),
        editTool("e2", "/repo/one.ts"),
      ]),
    ).toBe("Edited 2 files");
    expect(editedFileCount([editTool("e1", "/a"), editTool("e2", "/a")])).toBe(1);
  });

  it("lists activities in a fixed order regardless of call order", () => {
    const forward = [
      readTool("a", "/repo/one.ts"),
      commandTool("b", "npm test"),
      editTool("c", "/repo/one.ts"),
    ];
    const backward = [
      editTool("c", "/repo/one.ts"),
      commandTool("b", "npm test"),
      readTool("a", "/repo/one.ts"),
    ];
    expect(summarizeToolActivity(forward)).toBe("Searched ×1 · Ran ×1 · Edited 1 file");
    expect(summarizeToolActivity(backward)).toBe(summarizeToolActivity(forward));
  });

  it("keeps running rows on their present-tense verb", () => {
    expect(deriveToolPresentation(commandTool("a", "npm test", "Running")).verb).toBe("Running");
    expect(deriveToolPresentation(readTool("b", "/repo/one.ts")).verb).toBe("Searched");
    expect(deriveToolPresentation(commandTool("c", "npm test", "Failed")).verb).toBe("Failed");
    expect(
      deriveToolPresentation(commandTool("d", "npm test", "Interrupted")).verb,
    ).toBe("Interrupted");
  });
});
// end of file
