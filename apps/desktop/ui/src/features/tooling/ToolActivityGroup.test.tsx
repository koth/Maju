import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ToolInvocation } from "../../types";
import { ToolActivityGroupRow } from "./ToolActivityGroup";
import { summarizeToolActivity } from "./tool-activity";

afterEach(cleanup);

function readTool(id: string, path: string): ToolInvocation {
  return {
    id,
    call_id: id,
    parent_call_id: null,
    name: "Read",
    kind: "read",
    summary: `Read ${path}`,
    status: "Succeeded",
    is_subagent: false,
    detail_text: "",
    logs: [],
    raw_input: JSON.stringify({ path }),
    raw_output: null,
    terminal_output: null,
    error: null,
    diff_paths: [],
    diff_previews: [],
    permission_options: [],
    permission_input: null,
    permission_decision: null,
    can_stop: false,
    stop_kind: null,
    stop_status: null,
  };
}

describe("ToolActivityGroupRow", () => {
  const tools = [readTool("a", "/repo/one.ts"), readTool("b", "/repo/two.ts")];

  it("starts collapsed, showing only the summary", () => {
    render(
      <ToolActivityGroupRow
        group={{ startIndex: 0, indexes: [0, 1], tools, summary: summarizeToolActivity(tools) }}
        renderTool={(tool) => <div data-testid="tool-row">{tool.id}</div>}
      />,
    );

    const summary = screen.getByRole("button", { name: /展开已探索 ×2/ });
    expect(summary).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryAllByTestId("tool-row")).toHaveLength(0);
  });

  it("reveals the individual calls on click and hides them again", () => {
    render(
      <ToolActivityGroupRow
        group={{ startIndex: 0, indexes: [0, 1], tools, summary: summarizeToolActivity(tools) }}
        renderTool={(tool) => <div data-testid="tool-row">{tool.id}</div>}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /展开已探索 ×2/ }));
    expect(screen.getAllByTestId("tool-row").map((row) => row.textContent)).toEqual(["a", "b"]);

    fireEvent.click(screen.getByRole("button", { name: /收起已探索 ×2/ }));
    expect(screen.queryAllByTestId("tool-row")).toHaveLength(0);
  });

  it("draws no marker for a finished run, but keeps the live one", () => {
    const { container, rerender } = render(
      <ToolActivityGroupRow
        group={{ startIndex: 0, indexes: [0, 1], tools, summary: summarizeToolActivity(tools) }}
        renderTool={(tool) => <div data-testid="tool-row">{tool.id}</div>}
      />,
    );
    expect(container.querySelector(".tc-bullet")).toBeNull();

    const runningTools = [{ ...tools[0], status: "Running" as const }, tools[1]];
    rerender(
      <ToolActivityGroupRow
        group={{ startIndex: 0, indexes: [0, 1], tools: runningTools, summary: summarizeToolActivity(runningTools) }}
        renderTool={(tool) => <div data-testid="tool-row">{tool.id}</div>}
      />,
    );
    expect(container.querySelector(".tc-bullet-active")).not.toBeNull();
  });
});
