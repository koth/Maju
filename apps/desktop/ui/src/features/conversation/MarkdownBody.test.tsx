import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import MarkdownBody from "./MarkdownBody";

describe("MarkdownBody", () => {
  afterEach(() => {
    cleanup();
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
});
