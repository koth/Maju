import type { DiffHunk, ToolDiffPreview } from "../../types";

export const DIFF_CONTEXT_LINES = 3;

interface PatchLine {
  kind: DiffHunk["lines"][number]["kind"];
  content: string;
  oldStart: number;
  newStart: number;
  hunkIndex: number;
}

interface PatchRange {
  start: number;
  end: number;
  hunkIndex: number;
}

export function getDiffStats(previews: ToolDiffPreview[]) {
  return previews.reduce(
    (stats, preview) => {
      for (const hunk of preview.hunks) {
        for (const line of hunk.lines) {
          if (line.kind === "Added") stats.added += 1;
          if (line.kind === "Removed") stats.removed += 1;
        }
      }
      return stats;
    },
    { added: 0, removed: 0 }
  );
}

export function previewToCompactPatch(preview: ToolDiffPreview): string {
  const path = normalizePatchPath(preview.path);
  const lines = toPatchLines(preview.hunks);
  const ranges = compactPatchRanges(lines);
  const hunks = ranges.map((range) => compactRangeToPatch(lines, range));

  return [
    `diff --git a/${path} b/${path}`,
    `--- a/${path}`,
    `+++ b/${path}`,
    ...hunks,
  ]
    .filter(Boolean)
    .join("\n");
}

function toPatchLines(hunks: DiffHunk[]): PatchLine[] {
  let fallbackOldLine = 1;
  let fallbackNewLine = 1;

  return hunks.flatMap((hunk, hunkIndex) => {
    const range = parseHunkRange(hunk.heading);
    let oldLine = range ? lineCursorStart(range.oldStart, range.oldCount) : fallbackOldLine;
    let newLine = range ? lineCursorStart(range.newStart, range.newCount) : fallbackNewLine;
    const patchLines = hunk.lines.map((line) => {
      const patchLine = {
        kind: line.kind,
        content: line.content,
        oldStart: oldLine,
        newStart: newLine,
        hunkIndex,
      };

      if (line.kind !== "Added") oldLine += 1;
      if (line.kind !== "Removed") newLine += 1;

      return patchLine;
    });
    fallbackOldLine = oldLine;
    fallbackNewLine = newLine;
    return patchLines;
  });
}

function compactPatchRanges(lines: PatchLine[]): PatchRange[] {
  const hunkBounds = patchLineHunkBounds(lines);
  const changedIndexes = lines
    .map((line, index) => (line.kind === "Context" ? -1 : index))
    .filter((index) => index >= 0);

  if (changedIndexes.length === 0) {
    const first = lines[0];
    if (!first) return [];
    const bounds = hunkBounds.get(first.hunkIndex);
    return bounds
      ? [
          {
            start: bounds.start,
            end: Math.min(bounds.end, bounds.start + 12),
            hunkIndex: first.hunkIndex,
          },
        ]
      : [];
  }

  const ranges: PatchRange[] = [];
  for (const index of changedIndexes) {
    const line = lines[index];
    const bounds = hunkBounds.get(line.hunkIndex) ?? { start: 0, end: lines.length };
    const start = Math.max(bounds.start, index - DIFF_CONTEXT_LINES);
    const end = Math.min(bounds.end, index + DIFF_CONTEXT_LINES + 1);
    const last = ranges[ranges.length - 1];

    if (last && last.hunkIndex === line.hunkIndex && start <= last.end) {
      last.end = Math.max(last.end, end);
    } else {
      ranges.push({ start, end, hunkIndex: line.hunkIndex });
    }
  }

  return ranges;
}

function compactRangeToPatch(lines: PatchLine[], range: PatchRange): string {
  const rangeLines = lines.slice(range.start, range.end);
  const first = rangeLines[0];
  const oldCount = rangeLines.filter((line) => line.kind !== "Added").length;
  const newCount = rangeLines.filter((line) => line.kind !== "Removed").length;
  const body = rangeLines.map(patchLineToText).join("\n");

  return [
    `@@ -${formatPatchRange(first.oldStart, oldCount)} +${formatPatchRange(
      first.newStart,
      newCount
    )} @@`,
    body,
  ].join("\n");
}

function patchLineToText(line: PatchLine): string {
    const prefix = line.kind === "Added" ? "+" : line.kind === "Removed" ? "-" : " ";
    return `${prefix}${line.content}`;
}

export function formatPatchRange(start: number, lineCount: number): string {
  if (lineCount === 0) return `${Math.max(0, start - 1)},0`;
  return lineCount === 1 ? `${start}` : `${start},${lineCount}`;
}

interface ParsedHunkRange {
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
}

function parseHunkRange(heading: string): ParsedHunkRange | null {
  const match = heading.match(/^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/);
  if (!match) return null;
  const oldStart = Number(match[1]);
  const oldCount = match[2] == null ? 1 : Number(match[2]);
  const newStart = Number(match[3]);
  const newCount = match[4] == null ? 1 : Number(match[4]);
  if (
    !Number.isFinite(oldStart) ||
    !Number.isFinite(oldCount) ||
    !Number.isFinite(newStart) ||
    !Number.isFinite(newCount)
  ) {
    return null;
  }
  return { oldStart, oldCount, newStart, newCount };
}

function lineCursorStart(start: number, count: number): number {
  return count === 0 ? start + 1 : start;
}

function patchLineHunkBounds(lines: PatchLine[]): Map<number, { start: number; end: number }> {
  const bounds = new Map<number, { start: number; end: number }>();
  lines.forEach((line, index) => {
    const existing = bounds.get(line.hunkIndex);
    if (existing) {
      existing.end = index + 1;
    } else {
      bounds.set(line.hunkIndex, { start: index, end: index + 1 });
    }
  });
  return bounds;
}

function normalizePatchPath(path: string): string {
  return path.replace(/\\/g, "/").replace(/^[a-zA-Z]:\//, "");
}
