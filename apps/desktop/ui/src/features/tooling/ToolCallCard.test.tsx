import { describe, it, expect } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { ToolCallCard } from "./ToolCallCard";
import type { ToolInvocation, UiSnapshot, ToolStatus } from "../../types/index";

function makeTool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "t-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "Read",
    kind: "read",
    summary: "Read file",
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
    permission_decision: null,
    ...overrides,
  };
}

function makeSnapshot(tools: ToolInvocation[] = []): UiSnapshot {
  return {
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
    prompt_capabilities: { image: false, embedded_context: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools,
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    thinking_status: null,
  };
}

describe("ToolCallCard animation states", () => {
  const runningStatuses: ToolStatus[] = ["Pending", "Running"];
  const terminalStatuses: ToolStatus[] = ["Succeeded", "Failed", "Interrupted"];

  runningStatuses.forEach((status) => {
    it(`uses tc-bullet-active for ${status} tool`, () => {
      const tool = makeTool({ status });
      const snapshot = makeSnapshot([tool]);
      const { container } = render(
        <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
      );
      const bullet = container.querySelector(".tc-bullet");
      expect(bullet!.classList.contains("tc-bullet-active")).toBe(true);
    });
  });

  terminalStatuses.forEach((status) => {
    it(`does not use tc-bullet-active for ${status} tool`, () => {
      const tool = makeTool({ status });
      const snapshot = makeSnapshot([tool]);
      const { container } = render(
        <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
      );
      const bullet = container.querySelector(".tc-bullet");
      expect(bullet!.classList.contains("tc-bullet-active")).toBe(false);
    });
  });

  it("shows Editing verb for edit tools with diff_paths", () => {
    const tool = makeTool({
      status: "Running",
      kind: "edit",
      name: "Edit",
      diff_paths: ["/test/foo.rs"],
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("Editing");
  });

  it("shows Explored verb for read tools without diffs", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "Read",
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("Explored");
  });

  it("shows Exploring verb for explore subagent tasks", () => {
    const tool = makeTool({
      status: "Running",
      kind: "explore",
      name: "task",
      is_subagent: true,
      raw_input: '{"description":"探索项目结构和状态","subagent_type":"explore"}',
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("Exploring");
  });
});

describe("ToolCallCard tracker-confirmed diffs", () => {
  it("does not classify read tool with diff_previews as editing", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "Read",
      diff_paths: ["/test/docs/editor-subsystem-design.md"],
      diff_previews: [
        {
          path: "/test/docs/editor-subsystem-design.md",
          hunks: [
            {
              heading: "@@ -1,3 +1,4 @@",
              lines: [
                { kind: "Context", content: "line1" },
                { kind: "Added", content: "new line" },
              ],
            },
          ],
        },
      ],
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    const added = container.querySelector(".tc-diff-added");
    expect(verb!.textContent).toBe("Explored");
    expect(added).toBeNull();
  });

  it("does not classify bash tool with tracked diffs as editing", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "bash",
      name: "Bash",
      summary: "Run build",
      diff_paths: ["/test/dist/app.js"],
      diff_previews: [
        {
          path: "/test/dist/app.js",
          hunks: [
            {
              heading: "@@ -1,1 +1,1 @@",
              lines: [{ kind: "Added", content: "bundle" }],
            },
          ],
        },
      ],
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    const added = container.querySelector(".tc-diff-added");
    expect(verb!.textContent).toBe("Ran");
    expect(added).toBeNull();
  });

  it("shows diff stats for tracker-confirmed changes", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "edit",
      name: "Edit",
      diff_paths: ["/test/foo.rs"],
      diff_previews: [
        {
          path: "/test/foo.rs",
          hunks: [
            {
              heading: "@@ -1,3 +1,4 @@",
              lines: [
                { kind: "Added", content: "line1" },
                { kind: "Added", content: "line2" },
                { kind: "Removed", content: "old" },
              ],
            },
          ],
        },
      ],
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const added = container.querySelector(".tc-diff-added");
    const removed = container.querySelector(".tc-diff-removed");
    expect(added).toBeTruthy();
    expect(removed).toBeTruthy();
  });

  it("read tool with path containing editor is not classified as editing without diffs", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "docs/editor-subsystem-design.md",
      raw_input: '{"file_path":"docs/editor-subsystem-design.md"}',
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("Explored");
  });

  it("renders detail text and logs when expanded", () => {
    const tool = makeTool({
      status: "Running",
      detail_text: "step one\nstep two",
      logs: [
        { title: "Requested", body: "探索项目结构和状态" },
        { title: "Agent", body: "searched files" },
      ],
    });
    const snapshot = makeSnapshot([tool]);
    const { container } = render(
      <ToolCallCard tool={tool} snapshot={snapshot} nested={false} onPermissionSelect={() => {}} />,
    );

    fireEvent.click(container.querySelector(".tc-header-line")!);

    expect(container.textContent).toContain("step one");
    expect(container.textContent).toContain("step two");
    expect(container.textContent).toContain("Requested");
    expect(container.textContent).toContain("searched files");
  });
});
