import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import type { ToolInvocation } from "../../types";

// pierre resolves its syntax palette in JavaScript (it paints a shadow DOM), so
// the theme has to be handed to it as options — CSS variables never reach it.
// Capture those options instead of rendering the real diff.
const captured = vi.hoisted(() => ({ options: [] as Array<Record<string, unknown>> }));

vi.mock("@pierre/diffs/react", () => ({
  PatchDiff: (props: { options?: Record<string, unknown> }) => {
    if (props.options) captured.options.push(props.options);
    return null;
  },
}));

vi.mock("../../lib/tauri", () => ({
  sessionGetToolDetail: vi.fn(() => Promise.reject(new Error("no mock"))),
}));

const { ToolCallCard } = await import("./ToolCallCard");

function makeEditingTool(): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "Edit",
    kind: "edit",
    summary: "Edit file",
    status: "Succeeded",
    is_subagent: false,
    detail_text: "",
    logs: [],
    diff_paths: ["src/a.ts"],
    diff_previews: [
      {
        path: "src/a.ts",
        hunks: [
          {
            heading: "@@ -1,2 +1,2 @@",
            lines: [
              { kind: "Removed", content: "const a = 1;" },
              { kind: "Added", content: "const a = 2;" },
            ],
          },
        ],
      },
    ],
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
  };
}

function lastOptions() {
  return captured.options[captured.options.length - 1];
}

afterEach(() => {
  cleanup();
  captured.options.length = 0;
  document.documentElement.removeAttribute("data-theme");
});

describe("ToolCallCard inline diff theme", () => {
  it("hands pierre the light palette under the light theme", () => {
    document.documentElement.dataset.theme = "light";
    const { container } = render(
      <ToolCallCard tool={makeEditingTool()} nested={false} onPermissionSelect={() => {}} />,
    );
    fireEvent.click(container.querySelector(".tc-header-line") as HTMLElement);

    expect(lastOptions().theme).toBe("pierre-light");
    expect(lastOptions().themeType).toBe("light");
  });

  it("keeps the dark palette for the default dark theme", () => {
    document.documentElement.dataset.theme = "graphite";
    const { container } = render(
      <ToolCallCard tool={makeEditingTool()} nested={false} onPermissionSelect={() => {}} />,
    );
    fireEvent.click(container.querySelector(".tc-header-line") as HTMLElement);

    expect(lastOptions().theme).toBe("pierre-dark");
    expect(lastOptions().themeType).toBe("dark");
  });

  it("follows a theme switch made after the card rendered", async () => {
    document.documentElement.dataset.theme = "graphite";
    const { container } = render(
      <ToolCallCard tool={makeEditingTool()} nested={false} onPermissionSelect={() => {}} />,
    );
    fireEvent.click(container.querySelector(".tc-header-line") as HTMLElement);
    expect(lastOptions().themeType).toBe("dark");

    document.documentElement.dataset.theme = "light";

    await waitFor(() => expect(lastOptions().themeType).toBe("light"));
    expect(lastOptions().theme).toBe("pierre-light");
  });
});
