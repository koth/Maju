// Tool activity grouping for the phone timeline — the mobile port of the
// desktop `tooling/tool-activity.ts`, so one session reads the same on both
// surfaces.
//
// Desktop behaviour being mirrored: a *contiguous run* of tool calls collapses
// into a single summary row ("Searched ×2 · Ran ×1 · Edited 2 files") that
// expands into the individual rows. Edits are counted by file rather than by
// call, because "which files did it touch" is what the reader wants; everything
// else is counted by call.
//
// Only calls the reader may have to act on stay out of a group, so nothing
// actionable hides behind a summary:
//
// - failures / interruptions (the error must be visible),
// - permission & question requests (the reader may have to answer),
// - child/subagent calls (they render nested under their parent),
// - permission-pending rows.
//
// Reasoning does NOT break a run: `Thinking` timeline items render nothing, so
// folding them in keeps one summary per stretch of work instead of one per
// interrupted piece.
//
// A run of one is not a group either: the row alone already says everything.

import type { ToolInvocation } from "../../types";
import {
  FINISHED_VERBS,
  classifyToolActivity,
  type ToolActivityCategory,
} from "./tool-presentation";

export interface ToolActivityGroup {
  /// Index in the timeline of the group's first tool (the row that renders the
  /// summary; the rest are skipped).
  startIndex: number;
  /// Every timeline index the group covers.
  indexes: number[];
  tools: ToolInvocation[];
  summary: string;
}

/// Categories a group can summarize, in the order they are listed in a summary.
const ACTIVITY_ORDER: readonly ToolActivityCategory[] = [
  "exploring",
  "executing",
  "editing",
  "asking",
];

/// Whether one tool may be folded into an activity group.
export function isGroupableTool(tool: ToolInvocation): boolean {
  // Child/subagent calls render nested under their parent, never as their own
  // timeline row, so they can never be a member of a group.
  if (tool.parent_call_id) return false;
  if (tool.status === "Failed" || tool.status === "Interrupted") return false;
  if (tool.permission_input) return false;
  return ACTIVITY_ORDER.includes(classifyToolActivity(tool));
}

/// How many distinct files a run's edits touched: the count the summary shows.
/// Falls back to the number of edit calls when the harness reported no diff
/// paths (an edit whose row has no preview still counts as one file).
export function editedFileCount(tools: ToolInvocation[]): number {
  const edits = tools.filter((tool) => classifyToolActivity(tool) === "editing");
  if (edits.length === 0) return 0;
  const paths = new Set<string>();
  let withoutPath = 0;
  for (const tool of edits) {
    if (tool.diff_paths.length === 0) {
      withoutPath += 1;
      continue;
    }
    for (const path of tool.diff_paths) paths.add(path);
  }
  return paths.size + withoutPath;
}

/// Summarize a run of calls: one phrase per activity present, in a fixed
/// activity order (explore → run → edit → ask, so two runs with the same mix
/// always read the same way). Edits are counted by file; the other verbs are
/// the past-tense verbs the individual rows already use.
export function summarizeToolActivity(tools: ToolInvocation[]): string {
  const counts = new Map<ToolActivityCategory, number>();
  for (const tool of tools) {
    const category = classifyToolActivity(tool);
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  const files = editedFileCount(tools);
  const parts = ACTIVITY_ORDER.filter((category) => counts.has(category)).map((category) => {
    if (category === "editing") {
      return `${FINISHED_VERBS.editing} ${files} ${files === 1 ? "file" : "files"}`;
    }
    return `${FINISHED_VERBS[category]} ×${counts.get(category) ?? 0}`;
  });
  return parts.length > 0 ? parts.join(" · ") : "Worked";
}

/// Whether a timeline item renders nothing and must therefore not break a run.
/// A type guard so the timeline can `return` early and keep the remaining
/// `Message` / `Tool` variants narrowed.
export function isInvisibleTimelineItem(
  item: unknown,
): item is "Thinking" | { Thinking: unknown } {
  if (item === "Thinking") return true;
  return typeof item === "object" && item !== null && "Thinking" in item;
}

/// Build the activity groups for a timeline slice.
///
/// `timeline` entries are either message items (which break a run), invisible
/// thinking items (which do not) or tool ids; `toolsById` resolves the tool
/// rows, and a missing one breaks the run too (its row renders nothing).
export function buildToolActivityGroups(
  timeline: readonly unknown[],
  toolsById: ReadonlyMap<string, ToolInvocation>,
): { byStartIndex: Map<number, ToolActivityGroup>; memberIndexes: Set<number> } {
  const byStartIndex = new Map<number, ToolActivityGroup>();
  const memberIndexes = new Set<number>();
  let run: { startIndex: number; indexes: number[]; tools: ToolInvocation[] } | null = null;

  const flush = () => {
    if (!run) return;
    const { startIndex, indexes, tools } = run;
    run = null;
    if (tools.length < 2) return;
    byStartIndex.set(startIndex, {
      startIndex,
      indexes,
      tools,
      summary: summarizeToolActivity(tools),
    });
    for (const index of indexes) {
      if (index !== startIndex) memberIndexes.add(index);
    }
  };

  timeline.forEach((item, index) => {
    if (isInvisibleTimelineItem(item)) return;
    if (typeof item !== "object" || item === null || !("Tool" in item)) {
      flush();
      return;
    }
    const tool = toolsById.get(String((item as { Tool: string }).Tool));
    if (!tool || !isGroupableTool(tool)) {
      flush();
      return;
    }
    if (!run) run = { startIndex: index, indexes: [], tools: [] };
    run.indexes.push(index);
    run.tools.push(tool);
  });
  flush();

  return { byStartIndex, memberIndexes };
}
// end of file
