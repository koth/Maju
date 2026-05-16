import { describe, it, expect } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { ToolCallCard } from "./ToolCallCard";
import type { ToolInvocation, ToolStatus } from "../../types/index";

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

describe("ToolCallCard animation states", () => {
  const runningStatuses: ToolStatus[] = ["Pending", "Running"];
  const terminalStatuses: ToolStatus[] = ["Succeeded", "Failed", "Interrupted"];

  runningStatuses.forEach((status) => {
    it(`uses tc-bullet-active for ${status} tool`, () => {
      const tool = makeTool({ status });
      const { container } = render(
        <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
      );
      const bullet = container.querySelector(".tc-bullet");
      expect(bullet!.classList.contains("tc-bullet-active")).toBe(true);
    });
  });

  terminalStatuses.forEach((status) => {
    it(`does not use tc-bullet-active for ${status} tool`, () => {
      const tool = makeTool({ status });
      const { container } = render(
        <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
      );
      const bullet = container.querySelector(".tc-bullet");
      expect(bullet!.classList.contains("tc-bullet-active")).toBe(false);
    });
  });

  it("shows editing verb for edit tools with diff_paths", () => {
    const tool = makeTool({
      status: "Running",
      kind: "edit",
      name: "Edit",
      diff_paths: ["/test/foo.rs"],
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("编辑中");
  });

  it("shows explored verb for read tools without diffs", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "Read",
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("已探索");
  });

  it("shows exploring verb for explore subagent tasks", () => {
    const tool = makeTool({
      status: "Running",
      kind: "explore",
      name: "task",
      is_subagent: true,
      raw_input: '{"description":"探索项目结构和状态","subagent_type":"explore"}',
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("探索中");
  });

  it("shows todo write tools as task plan updates instead of edits", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "tool",
      name: "todo: todo write",
      summary: "Updated (107 chars)",
      raw_input: JSON.stringify({
        content: "- [ ] 查看当前 ci.yml 文件内容\n- [x] 添加 release 分支判断逻辑",
      }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已运行");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe("任务计划");
  });

  it("keeps child tool calls collapsed until requested", () => {
    const parent = makeTool({
      id: "parent",
      call_id: "parent-call",
      name: "Task",
      kind: "task",
      summary: "parent detail",
    });
    const child = makeTool({
      id: "child",
      call_id: "child-call",
      parent_call_id: "parent-call",
      name: "Child Read",
      summary: "child detail",
    });
    const childToolsByParent = new Map([["parent-call", [child]]]);

    const { container, getByText } = render(
      <ToolCallCard
        tool={parent}
        childToolsByParent={childToolsByParent}
        nested={false}
        onPermissionSelect={() => {}}
      />,
    );

    expect(container.textContent).toContain("1 个工具调用");
    expect(container.textContent).not.toContain("child detail");

    fireEvent.click(getByText("1 个工具调用"));
    expect(container.textContent).toContain("child detail");
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
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    const added = container.querySelector(".tc-diff-added");
    expect(verb!.textContent).toBe("已探索");
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
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    const added = container.querySelector(".tc-diff-added");
    expect(verb!.textContent).toBe("已运行");
    expect(added).toBeNull();
  });

  it("classifies git checkout pathspec commands as editing when tracked files changed", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command:
          'cd "d:/work/InfiniteCanvasOL" && git checkout -- frontend/src/components/InfiniteCanvas.tsx',
      }),
      terminal_output: { exit_code: 0, output: "Revert InfiniteCanvas.tsx changes" },
      diff_paths: ["frontend/src/components/InfiniteCanvas.tsx"],
      diff_previews: [
        {
          path: "frontend/src/components/InfiniteCanvas.tsx",
          hunks: [
            {
              heading: "@@ -1,1 +1,1 @@",
              lines: [
                { kind: "Removed", content: "old" },
                { kind: "Added", content: "new" },
              ],
            },
          ],
        },
      ],
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已编辑");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe(
      "frontend/src/components/InfiniteCanvas.tsx",
    );
    expect(container.querySelector(".tc-diff-added")?.textContent).toBe("+1");
    expect(container.querySelector(".tc-diff-removed")?.textContent).toBe("-1");

    fireEvent.click(container.querySelector(".tc-header-line")!);
    expect(container.querySelector(".tc-shell-panel")).toBeNull();
    expect(container.querySelector(".tc-diff-preview")).toBeTruthy();
  });

  it("keeps git checkout pathspec commands as executed when no tracked file changed", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command:
          'cd "d:/work/InfiniteCanvasOL" && git checkout -- frontend/src/components/InfiniteCanvas.tsx',
      }),
      terminal_output: { exit_code: 0, output: "" },
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已运行");
  });

  it("shows exploration path for Get-ChildItem command headers", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command: "Get-ChildItem -Path D:\\work\\kodex\\apps\\desktop\\ui -Depth 1",
      }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已探索");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe("D:/work/kodex/apps/desktop/ui");
  });

  it("classifies PowerShell exploration command wrappers as explored paths", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command:
          '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -Command "Get-ChildItem \\"D:\\work\\kodex\\frontend\\node_modules\\" -Directory -ErrorAction SilentlyContinue"',
      }),
      terminal_output: { exit_code: 0, output: "node_modules NOT found" },
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已探索");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe(
      "D:/work/kodex/frontend/node_modules",
    );
  });

  it("classifies Codex PowerShell Set-Content command wrappers as edited paths", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command: [
          "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
          "-Command",
          'if (-not (Test-Path "docs")) { New-Item -ItemType Directory -Path "docs" -Force | Out-Null }; $guideContent = @"\n# Guide\n\nSet-Content -Path "fake.md"\n"@; Set-Content -Path "docs/windows-guide.md" -Value $guideContent -Encoding UTF8',
        ],
      }),
      terminal_output: { exit_code: 0, output: "" },
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已编辑");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe("docs/windows-guide.md");
  });

  it("classifies Get-Content and Test-Path commands as exploration", () => {
    const cases = [
      {
        command: "Get-Content -Path apps/desktop/ui/src/features/tooling/ToolCallCard.tsx",
        title: "apps/desktop/ui/src/features/tooling/ToolCallCard.tsx",
      },
      {
        command: 'Test-Path "D:\\work\\kodex\\apps\\desktop\\ui\\package.json"',
        title: "D:/work/kodex/apps/desktop/ui/package.json",
      },
    ];

    for (const testCase of cases) {
      const tool = makeTool({
        status: "Succeeded",
        kind: "execute",
        name: "tool",
        raw_input: JSON.stringify({ command: testCase.command }),
      });
      const { container, unmount } = render(
        <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
      );

      expect(container.querySelector(".tc-verb")!.textContent).toBe("已探索");
      expect(container.querySelector(".tc-cmd")!.textContent).toBe(testCase.title);
      unmount();
    }
  });

  it("shows exploration path in read tool headers", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "Read",
      raw_input: JSON.stringify({ file_path: "apps/desktop/ui/src/features/tooling/ToolCallCard.tsx" }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已探索");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe(
      "apps/desktop/ui/src/features/tooling/ToolCallCard.tsx",
    );
  });

  it("renders CodeBuddy exploration arrays as a compact result panel", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "List",
      summary: "d:/work/InfiniteCanvasOL",
      detail_text: "/work/InfiniteCanvasOL",
      raw_output: JSON.stringify([
        "d:\\work\\InfiniteCanvasOL\\frontend\\node_modules\\listenercount\\circle.yml",
        "d:\\work\\InfiniteCanvasOL\\frontend\\node_modules\\reusify\\.github\\workflows\\ci.yml",
      ]),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    fireEvent.click(container.querySelector(".tc-header-line")!);

    expect(container.querySelector(".tc-explore-panel")).toBeTruthy();
    expect(container.querySelector(".tc-explore-root")?.textContent).toBe(
      "d:/work/InfiniteCanvasOL",
    );
    expect(container.textContent).toContain(
      "d:/work/InfiniteCanvasOL/frontend/node_modules/listenercount/circle.yml",
    );
    expect(container.textContent).not.toContain('["d:\\\\work');
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
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const added = container.querySelector(".tc-diff-added");
    const removed = container.querySelector(".tc-diff-removed");
    expect(added).toBeTruthy();
    expect(removed).toBeTruthy();
  });

  it("shows zero removed count for added-only tracker-confirmed changes", () => {
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
              heading: "@@ -1,1 +1,2 @@",
              lines: [{ kind: "Added", content: "line1" }],
            },
          ],
        },
      ],
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-diff-added")?.textContent).toBe("+1");
    expect(container.querySelector(".tc-diff-removed")?.textContent).toBe("-0");
    expect(container.querySelector(".tc-cmd")?.textContent).toBe("/test/foo.rs");
  });

  it("shows zero added count for removed-only tracker-confirmed changes", () => {
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
              heading: "@@ -1,2 +1,1 @@",
              lines: [{ kind: "Removed", content: "old" }],
            },
          ],
        },
      ],
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-diff-added")?.textContent).toBe("+0");
    expect(container.querySelector(".tc-diff-removed")?.textContent).toBe("-1");
  });

  it("does not show bogus whole-file addition stats for edit tools", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "edit",
      name: "Edit",
      diff_paths: ["/test/app-smoke.spec.ts"],
      diff_previews: [
        {
          path: "/test/app-smoke.spec.ts",
          hunks: [
            {
              heading: "@@ -1,3 +1,904 @@",
              lines: Array.from({ length: 904 }, (_, index) => ({
                kind: "Added" as const,
                content: `line ${index + 1}`,
              })),
            },
          ],
        },
      ],
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-diff-added")).toBeNull();
    expect(container.querySelector(".tc-diff-removed")).toBeNull();
    expect(container.querySelector(".tc-diff-preview")).toBeNull();
  });

  it("falls back to raw_input old_string/new_string when previews are missing", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "edit",
      name: "Edit",
      raw_input: JSON.stringify({
        file_path: "/test/app-smoke.spec.ts",
        old_string:
          "async function clickCanvasNewMenuItem(page: Page, itemText: string) {\n  await pane.click({ button: 'right' });\n  await page.getByText(itemText, { exact: true }).click();\n}",
        new_string:
          "async function clickCanvasNewMenuItem(page: Page, itemText: string) {\n  await page.keyboard.press('Escape');\n  await pane.evaluate((el) => el.dispatchEvent(new MouseEvent('contextmenu')));\n  await page.getByText(itemText, { exact: true }).click({ timeout: 5_000 });\n}",
      }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-diff-added")?.textContent).toBe("+3");
    expect(container.querySelector(".tc-diff-removed")?.textContent).toBe("-2");
  });

  it("classifies CodeBuddy file replacement payloads as editing even when presented as a command", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "{",
      summary: "{",
      raw_input: JSON.stringify({
        file_path: "d:\\work\\InfiniteCanvasOL\\.ci\\scripts\\deploy_remote_prd.py",
        new_string: "def upload_tree_atomic():\n    return uploaded_files",
      }),
      raw_output: JSON.stringify({ ok: true }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已编辑");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe(
      "d:/work/InfiniteCanvasOL/.ci/scripts/deploy_remote_prd.py",
    );

    fireEvent.click(container.querySelector(".tc-header-line")!);

    expect(container.querySelector(".tc-shell-panel")).toBeNull();
    expect(container.textContent).toContain(
      "d:\\work\\InfiniteCanvasOL\\.ci\\scripts\\deploy_remote_prd.py",
    );
    expect(container.textContent).not.toContain("def upload_tree_atomic");
  });

  it("classifies persisted truncated CodeBuddy replacement payloads as editing", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "{",
      summary: "{",
      raw_input:
        '{\n  "file_path": "d:\\\\work\\\\InfiniteCanvasOL\\\\.ci\\\\scripts\\\\deploy_remote_prd.py",\n  "new_string": "def upload_tree_atomic():\\n    return uploaded_files',
      raw_output: JSON.stringify({ ok: true }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-verb")!.textContent).toBe("已编辑");
    expect(container.querySelector(".tc-cmd")!.textContent).toBe(
      "d:/work/InfiniteCanvasOL/.ci/scripts/deploy_remote_prd.py",
    );

    fireEvent.click(container.querySelector(".tc-header-line")!);

    expect(container.querySelector(".tc-shell-panel")).toBeNull();
    expect(container.textContent).not.toContain("def upload_tree_atomic");
  });

  it("does not fall back to raw_input when it looks like fragment to whole file", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "edit",
      name: "Edit",
      raw_input: JSON.stringify({
        file_path: "/test/app-smoke.spec.ts",
        old_string: "function target() {\n  return 1;\n}",
        new_string: Array.from({ length: 904 }, (_, index) => `line ${index + 1}`).join("\n"),
      }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    expect(container.querySelector(".tc-diff-added")).toBeNull();
    expect(container.querySelector(".tc-diff-removed")).toBeNull();
  });

  it("read tool with path containing editor is not classified as editing without diffs", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "read",
      name: "docs/editor-subsystem-design.md",
      raw_input: '{"file_path":"docs/editor-subsystem-design.md"}',
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );
    const verb = container.querySelector(".tc-verb");
    expect(verb!.textContent).toBe("已探索");
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
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    fireEvent.click(container.querySelector(".tc-header-line")!);

    expect(container.textContent).toContain("step one");
    expect(container.textContent).toContain("step two");
    expect(container.textContent).toContain("Requested");
    expect(container.textContent).toContain("searched files");
  });

  it("renders command tools as a shell panel when expanded", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({ command: "Get-ChildItem -Path ." }),
      terminal_output: { exit_code: 0, output: "Name\n----\nindex.ts\n" },
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    const header = container.querySelector(".tc-header-line") as HTMLButtonElement;
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".tc-shell-panel")).toBeTruthy();
    expect(container.textContent).toContain("Shell");
    expect(container.textContent).toContain("$ Get-ChildItem -Path .");
    expect(container.textContent).toContain("Name");
    expect(container.textContent).toContain("成功");
  });

  it("hides raw JSON from primary command output when terminal output exists", () => {
    const tool = makeTool({
      status: "Succeeded",
      kind: "bash",
      name: "Bash",
      raw_input: JSON.stringify({ command: "pwsh -Command Get-ChildItem" }),
      terminal_output: { exit_code: 0, output: "loop.py\nruntime_status.py\n" },
      raw_output: JSON.stringify({
        aggregated_output: "loop.py\\nruntime_status.py\\n",
        call_id: "call-1",
        command: ["pwsh", "-Command", "Get-ChildItem"],
      }),
    });
    const { container } = render(
      <ToolCallCard tool={tool} nested={false} onPermissionSelect={() => {}} />,
    );

    const header = container.querySelector(".tc-header-line") as HTMLButtonElement;
    fireEvent.click(header);

    expect(container.textContent).toContain("loop.py");
    expect(container.textContent).not.toContain("aggregated_output");

    fireEvent.click(container.querySelector(".tc-raw-toggle")!);
    expect(container.textContent).toContain("aggregated_output");
  });
});
