import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownBody from "./MarkdownBody";

const originalClipboard = navigator.clipboard;

describe("MarkdownBody", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    if (originalClipboard) {
      Object.defineProperty(navigator, "clipboard", {
        value: originalClipboard,
        configurable: true,
      });
    } else {
      Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("removes leaked repeated course break noise", () => {
    render(
      <MarkdownBody
        content={[
          "放在 `toggleAllArmourGroupCollapsed` 之后。",
          "",
          "course",
          "<br>",
          "course",
          "",
          "course",
          "",
          "Let me add the derived values.",
        ].join("\n")}
      />,
    );

    expect(screen.getByText(/toggleAllArmourGroupCollapsed/)).toBeInTheDocument();
    expect(screen.getByText(/Let me add the derived values/)).toBeInTheDocument();
    expect(screen.queryByText("course")).not.toBeInTheDocument();
    expect(screen.queryByText("<br>")).not.toBeInTheDocument();
  });

  it("keeps normal course text", () => {
    render(<MarkdownBody content="This course of action is reasonable." />);

    expect(screen.getByText("This course of action is reasonable.")).toBeInTheDocument();
  });

  it("copies fenced code block content", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    render(
      <MarkdownBody
        content={[
          "```cpp",
          "AActor* Actor = World->SpawnActor<AActor>(...);",
          "PMC->RegisterComponent();",
          "```",
        ].join("\n")}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "复制代码" }));

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(
        "AActor* Actor = World->SpawnActor<AActor>(...);\nPMC->RegisterComponent();",
      );
    });
    expect(screen.getByRole("button", { name: "已复制代码" })).toBeInTheDocument();
  });
});
