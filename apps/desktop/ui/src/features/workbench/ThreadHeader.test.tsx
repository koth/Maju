import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ThreadHeader } from "./ThreadHeader";
import type { SessionSummary } from "../../types";

afterEach(cleanup);

function sessionWithAgent(agentCli: string | null): SessionSummary {
  return {
    id: "session-1",
    workspace_id: "workspace-1",
    title: "查找关闭按钮被tooltip遮挡",
    model: "model",
    mode: null,
    agent_cli: agentCli,
    status: "Idle",
  };
}

describe("ThreadHeader", () => {
  it("names the agent in parentheses after the title", () => {
    render(<ThreadHeader session={sessionWithAgent("deepseek-harness")} />);

    const heading = screen.getByRole("heading", { level: 1 });
    expect(heading.textContent).toBe("查找关闭按钮被tooltip遮挡(DeepSeek)");
    // The full pair stays in the tooltip for when the title ellipsis truncates.
    expect(heading).toHaveAttribute("title", "查找关闭按钮被tooltip遮挡 (DeepSeek)");
  });

  it("maps a stored display label instead of echoing the raw agent id", () => {
    render(<ThreadHeader session={sessionWithAgent("Codex")} />);

    expect(screen.getByRole("heading", { level: 1 }).textContent).toBe(
      "查找关闭按钮被tooltip遮挡(Codex)",
    );
  });

  it("omits the agent label when the session has none", () => {
    render(<ThreadHeader session={sessionWithAgent(null)} />);

    const heading = screen.getByRole("heading", { level: 1 });
    expect(heading.textContent).toBe("查找关闭按钮被tooltip遮挡");
    expect(heading).toHaveAttribute("title", "查找关闭按钮被tooltip遮挡");
  });
});
