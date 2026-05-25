import { describe, expect, it } from "vitest";
import type { ToolInvocation } from "../../types";
import { deriveToolPresentation } from "./tool-presentation";

function makeTool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "t-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "Bash",
    kind: "bash",
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
    permission_decision: null,
    ...overrides,
  };
}

describe("deriveToolPresentation", () => {
  it("extracts command from structured raw_input", () => {
    const tool = makeTool({
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command: "Get-ChildItem -Path .",
      }),
    });

    const presentation = deriveToolPresentation(tool);

    expect(presentation.presentationKind).toBe("command");
    expect(presentation.headerLabel).toBe("已运行");
    expect(presentation.command).toBe("Get-ChildItem -Path .");
    expect(presentation.toolLabel).toBe("Shell");
  });

  it("does not treat truncated JSON raw_input as a command", () => {
    const tool = makeTool({
      kind: "execute",
      name: "Terminal",
      raw_input:
        '{"content":"## ADDED Requirements\\nsource mask | result mask\\nmore text"',
    });

    const presentation = deriveToolPresentation(tool);

    expect(presentation.presentationKind).toBe("command");
    expect(presentation.command).toBeNull();
  });

  it("summarizes PowerShell -Command strings by their inner command", () => {
    const tool = makeTool({
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        command:
          '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -Command "conda env list 2>$null; python --version 2>$null; node --version 2>$null"',
      }),
    });

    const presentation = deriveToolPresentation(tool);

    expect(presentation.command).toBe(
      "conda env list 2>$null; python --version 2>$null; node --version 2>$null",
    );
  });

  it("summarizes PowerShell command arrays by their inner command", () => {
    const tool = makeTool({
      kind: "execute",
      name: "tool",
      raw_input: JSON.stringify({
        args: [
          "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
          "-Command",
          'Get-ChildItem "D:\\work\\kodex\\apps\\desktop\\ui" -Depth 1',
        ],
      }),
    });

    const presentation = deriveToolPresentation(tool);

    expect(presentation.command).toBe(
      'Get-ChildItem "D:\\work\\kodex\\apps\\desktop\\ui" -Depth 1',
    );
  });

  it("extracts command from backtick tool names", () => {
    const presentation = deriveToolPresentation(
      makeTool({
        kind: "tool",
        name: "`npm run build`",
      }),
    );

    expect(presentation.presentationKind).toBe("command");
    expect(presentation.command).toBe("npm run build");
  });

  it("prefers terminal output over raw output payloads", () => {
    const presentation = deriveToolPresentation(
      makeTool({
        terminal_output: { exit_code: 0, output: "Name\n----\nindex.ts\n" },
        raw_output: JSON.stringify({
          aggregated_output: "Name\\n----\\nindex.ts\\n",
          command: ["pwsh.exe", "-Command", "Get-ChildItem"],
        }),
      }),
    );

    expect(presentation.primaryOutput).toBe("Name\n----\nindex.ts");
    expect(presentation.primaryOutput).not.toContain("aggregated_output");
    expect(presentation.rawDetails).toHaveLength(1);
    expect(presentation.rawDetails[0].title).toBe("Result");
  });

  it("falls back to readable raw_output when terminal output is absent", () => {
    const presentation = deriveToolPresentation(
      makeTool({
        terminal_output: null,
        raw_output: JSON.stringify({
          formatted_output: "\\u001b[32;1mName\\u001b[0m\\n----\\nloop.py",
        }),
      }),
    );

    expect(presentation.primaryOutput).toBe("Name\n----\nloop.py");
  });

  it("reports failed command status with exit code", () => {
    const presentation = deriveToolPresentation(
      makeTool({
        status: "Failed",
        terminal_output: { exit_code: 2, output: "error" },
      }),
    );

    expect(presentation.headerLabel).toBe("失败");
    expect(presentation.footerStatus).toEqual({ label: "失败 (2)", tone: "danger" });
  });

  it("reports interrupted command status", () => {
    const presentation = deriveToolPresentation(
      makeTool({
        status: "Interrupted",
      }),
    );

    expect(presentation.headerLabel).toBe("已中断");
    expect(presentation.footerStatus).toEqual({ label: "已中断", tone: "warning" });
  });
});
