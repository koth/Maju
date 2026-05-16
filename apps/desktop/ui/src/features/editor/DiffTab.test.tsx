import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { DiffTab } from "./DiffTab";
import type { DiffQuality, FileChangeRecord } from "../../types";

function makeChange(quality: DiffQuality): FileChangeRecord {
  return {
    change_set_id: "cs-1",
    path: "src/file.ts",
    change_type: "Modified",
    old_text: quality === "Exact" ? "old\n" : null,
    new_text: quality === "Exact" ? "new\n" : null,
    added_lines: 1,
    removed_lines: 1,
    quality,
    updated_at: "2026-05-12T00:00:00Z",
  };
}

describe("DiffTab unavailable quality states", () => {
  it.each([
    ["LargeFileSkipped", "文件太大，已跳过内联差异预览。"],
    ["BinarySkipped", "二进制或不可读取文件，无法展示文本差异。"],
    ["MissingBaseline", "缺少可比较的基线内容，无法展示可靠差异。"],
    ["FragmentRejected", "只捕获到了片段级改动，已拒绝渲染为完整文件差异。"],
    ["LegacyIncomplete", "旧历史记录缺少完整快照，无法展示可靠差异。"],
  ] as const)("renders an explicit message for %s", (quality, message) => {
    render(<DiffTab change={makeChange(quality)} appTheme="graphite" />);

    expect(screen.getByText(message)).toBeTruthy();
  });
});
