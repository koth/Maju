import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ChangesBar } from "./ChangesBar";
import type { FileChangeSummary } from "../../types";

function makeChange(path: string, added: number, removed: number): FileChangeSummary {
  return {
    change_set_id: "turn-1",
    path,
    change_type: "Modified",
    added_lines: added,
    removed_lines: removed,
    quality: "Exact",
    updated_at: "2026-05-12T00:00:00Z",
  };
}

describe("ChangesBar", () => {
  it("opens files with the owning change set id", () => {
    const onFileSelect = vi.fn();
    render(
      <ChangesBar
        changeSetId="turn-1"
        changes={[makeChange("src/b.ts", 2, 0), makeChange("src/a.ts", 1, 1)]}
        onFileSelect={onFileSelect}
      />,
    );

    expect(screen.getByText("已编辑 2 个文件")).toBeTruthy();
    expect(screen.getByText("+3")).toBeTruthy();
    expect(screen.getAllByText("-1").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByText("src/a.ts"));

    expect(onFileSelect).toHaveBeenCalledWith("src/a.ts", "turn-1");
  });
});
