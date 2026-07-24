import { describe, expect, it } from "vitest";
import {
  buildFilePathCandidatePool,
  collectPathsFromText,
} from "./file-path-candidates";
import type { ToolInvocation } from "../../types";

function makeTool(overrides: Partial<ToolInvocation>): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "shell",
    kind: "execute",
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

describe("collectPathsFromText", () => {
  it("extracts relative paths with extensions from shell commands", () => {
    expect(
      collectPathsFromText("rg -n 'state' crates/app-core/src/state.rs apps/desktop/ui"),
    ).toEqual(["crates/app-core/src/state.rs"]);
  });

  it("extracts paths from command output including line references", () => {
    expect(
      collectPathsFromText(
        "crates/app-core/src/state.rs:42: let session = ...\ncrates/acp-core/src/mapping.rs:7: ...",
      ),
    ).toEqual([
      "crates/app-core/src/state.rs:42",
      "crates/acp-core/src/mapping.rs:7",
    ]);
  });

  it("strips diff prefixes and ignores bare directories", () => {
    expect(collectPathsFromText("--- a/crates/x.rs\n+++ b/crates/y.rs\ncrates/foo/")).toEqual([
      "crates/x.rs",
      "crates/y.rs",
    ]);
  });

  it("keeps absolute windows and posix paths", () => {
    expect(
      collectPathsFromText("D:\\work\\kodex\\crates\\app-core\\src\\state.rs /home/user/repo/src/main.rs"),
    ).toEqual([
      "D:\\work\\kodex\\crates\\app-core\\src\\state.rs",
      "/home/user/repo/src/main.rs",
    ]);
  });
});

describe("buildFilePathCandidatePool", () => {
  const messages = new Map([
    ["u1", { id: "u1", role: "User" }],
    ["a1", { id: "a1", role: "Assistant" }],
    ["u2", { id: "u2", role: "User" }],
    ["a2", { id: "a2", role: "Assistant" }],
  ]);

  it("scopes paths to the turn that produced them", () => {
    const tools = new Map([
      [
        "t1",
        makeTool({
          id: "t1",
          raw_input: "rg state crates/app-core/src/state.rs",
        }),
      ],
      [
        "t2",
        makeTool({
          id: "t2",
          call_id: "call-2",
          raw_output: "apps/desktop/ui/src/main.tsx:1: ...",
        }),
      ],
    ]);
    const timeline = [
      { Message: "u1" },
      { Tool: "t1" },
      { Message: "a1" },
      { Message: "u2" },
      { Tool: "t2" },
      { Message: "a2" },
    ];

    const pool = buildFilePathCandidatePool(timeline, messages, tools, {});
    expect(pool.byMessageId.get("a1")).toEqual(["crates/app-core/src/state.rs"]);
    expect(pool.byMessageId.get("a2")).toEqual(["apps/desktop/ui/src/main.tsx:1"]);
    expect([...pool.all].sort()).toEqual([
      "crates/app-core/src/state.rs",
      "apps/desktop/ui/src/main.tsx:1",
    ].sort());
  });

  it("accumulates paths across multiple tools within one turn", () => {
    const tools = new Map([
      ["t1", makeTool({ id: "t1", raw_input: "cat crates/a.rs" })],
      [
        "t2",
        makeTool({
          id: "t2",
          call_id: "call-2",
          terminal_output: { exit_code: 0, output: "modified: crates/b.rs" },
        }),
      ],
    ]);
    const timeline = [
      { Message: "u1" },
      { Tool: "t1" },
      { Tool: "t2" },
      { Message: "a1" },
    ];

    const pool = buildFilePathCandidatePool(timeline, messages, tools, {});
    expect(pool.byMessageId.get("a1")).toEqual(["crates/a.rs", "crates/b.rs"]);
  });

  it("includes turn changeset files in the shared pool", () => {
    const pool = buildFilePathCandidatePool(
      [{ Message: "u1" }, { Message: "a1" }],
      messages,
      new Map(),
      {
        a1: {
          files: [{ path: "crates/app-core/src/state.rs" }],
        },
      },
    );
    expect(pool.all).toEqual(["crates/app-core/src/state.rs"]);
    expect(pool.byMessageId.get("a1")).toEqual([]);
  });
});
