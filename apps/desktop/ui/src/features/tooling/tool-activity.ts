// Tool activity grouping for the conversation timeline.
//
// Codex renders a running turn as a handful of collapsed activity summaries
// ("已读取文件运行了命令") that expand into the individual calls on click. Kodex
// kept the raw rows visible instead, so a busy turn still stacked rows the
// reader had to scroll past.
//
// A group here is a *contiguous run* of tool calls — reads, searches, commands
// AND edits. Edits are summarized by how many files they touched
// ("已编辑 3 个文件"), and expanding the group reveals each row with its diff
// stats. Only calls whose row the reader may have to act on stay out of a group,
// so nothing actionable hides behind a summary:
//
// - failures (the error must be visible),
// - permission/question requests (the reader may have to answer),
// - plan/todo tools (they render a plan list).
//
// Reasoning does NOT break a run: thinking items render nothing in the timeline
// (historical reasoning segments are live-only), so folding them in keeps one
// summary per stretch of work instead of one per interrupted piece.
//
// A run of one is not a group either: the row alone already says everything.

import type { ToolInvocation } from "../../types";
import { classifyTool, isQuestionTool, isTodoWriteTool } from "./tool-card-analysis";

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
const ACTIVITY_ORDER = ["exploring", "executing", "editing", "asking"] as const;
type ActivityCategory = (typeof ACTIVITY_ORDER)[number];

/// Same verbs the individual rows use (`toolVerb`), so the summary reads as a
/// summary *of those rows* rather than a second vocabulary.
const ACTIVITY_VERBS: Record<ActivityCategory, string> = {
  exploring: "已探索",
  executing: "已运行",
  editing: "已编辑",
  asking: "已提问",
};

/// Whether one tool may be folded into an activity group.
export function isGroupableTool(tool: ToolInvocation): boolean {
  // Child/subagent calls render nested under their parent, never as their own
  // timeline row, so they can never be a member of a group.
  if (tool.parent_call_id) return false;
  if (isQuestionTool(tool) || isTodoWriteTool(tool)) return false;
  if (tool.status === "Failed" || tool.status === "Interrupted") return false;
  if (tool.permission_input) return false;
  const category = classifyTool(tool);
  return ACTIVITY_ORDER.includes(category as ActivityCategory);
}

/// How many distinct files a run's edits touched: the count the summary shows.
/// Falls back to the number of edit calls when the harness reported no diff
/// paths (an edit whose row has no preview still counts as one file).
export function editedFileCount(tools: ToolInvocation[]): number {
  const edits = tools.filter((tool) => classifyTool(tool) === "editing");
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

/// Summarize a run of calls, Codex-style: one phrase per activity present, in a
/// fixed activity order (探索 → 运行 → 编辑 → 提问, so two runs with the same mix
/// always read the same way). Edits are counted by file (`已编辑 3 个文件`)
/// because that is what a reader wants to know; everything else is counted by
/// call (`已探索 ×3 · 已运行 ×1`).
export function summarizeToolActivity(tools: ToolInvocation[]): string {
  const counts = new Map<ActivityCategory, number>();
  for (const tool of tools) {
    const category = classifyTool(tool) as ActivityCategory;
    if (!ACTIVITY_ORDER.includes(category)) continue;
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  const files = editedFileCount(tools);
  const parts = ACTIVITY_ORDER.filter((category) => counts.has(category)).map((category) =>
    category === "editing"
      ? `${ACTIVITY_VERBS.editing} ${files} 个文件`
      : `${ACTIVITY_VERBS[category]} ×${counts.get(category) ?? 0}`,
  );
  return parts.length > 0 ? parts.join(" · ") : "已处理";
}

/// Whether a timeline item renders nothing and must therefore not break a run.
/// History folds reasoning segments in without showing them, so a think between
/// two commands is not a reason to start a second summary.
function isInvisibleTimelineItem(item: unknown): boolean {
  if (item === "Thinking") return true;
  return typeof item === "object" && item !== null && "Thinking" in item;
}

/// Build the activity groups for a timeline slice.
///
/// `timeline` entries are either message items (which break a run), invisible
/// thinking items (which do not) or tool ids; `toolsById` resolves the tool
/// rows, and a missing one breaks the run too (its row renders nothing).
///
/// `include` is the timeline's own row predicate
/// (`shouldRenderTimelineTool`). A tool it rejects — a child call rendered
/// nested under its parent, a pending approval whose panel is open elsewhere —
/// must not be folded into a group either, or expanding the group would render
/// a row the timeline deliberately hides.
export function buildToolActivityGroups(
  timeline: readonly unknown[],
  toolsById: ReadonlyMap<string, ToolInvocation>,
  include?: (tool: ToolInvocation) => boolean,
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
    if (!tool || !isGroupableTool(tool) || (include && !include(tool))) {
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
