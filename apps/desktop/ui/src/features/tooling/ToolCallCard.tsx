import { useState } from "react";
import { PatchDiff } from "@pierre/diffs/react";
import type { DiffHunk, ToolDiffPreview, ToolInvocation, UiSnapshot, ToolStatus } from "../../types";
import "./ToolCallCard.css";

const MAX_OUTPUT_LINES = 5;
const DIFF_CONTEXT_LINES = 3;
const DIFF_OPTIONS = {
  diffStyle: "unified",
  disableFileHeader: true,
  hunkSeparators: "metadata",
  lineDiffType: "word",
  overflow: "wrap",
  themeType: "dark",
} as const;

interface Props {
  tool: ToolInvocation;
  snapshot: UiSnapshot;
  nested: boolean;
  onPermissionSelect: (requestId: string, optionId: string | null) => void;
}

export function ToolCallCard({ tool, snapshot, nested, onPermissionSelect }: Props) {
  const [expanded, setExpanded] = useState(false);

  const children = snapshot.tools.filter(
    (t) => t.parent_call_id === tool.call_id
  );

  const [childrenCollapsed, setChildrenCollapsed] = useState(false);

  const trackedDiffPaths = getTrackedDiffPaths(tool);
  const diffPreviews = getTrackedDiffPreviews(tool);
  const category = classifyTool(tool);
  const bullet = statusBullet(tool.status);
  const verb = toolVerb(tool.status, category);
  const headerTitle = extractHeaderTitle(tool, trackedDiffPaths);
  const cmdDetail = extractCommandDetail(tool, trackedDiffPaths);
  const outputLines = getOutputLines(tool);
  const detailLines = getDetailLines(tool);
  const logEntries = getVisibleLogEntries(tool);
  const errorLine =
    tool.error && !isVagueError(tool.error) ? tool.error : null;
  const diffStats = getDiffStats(diffPreviews);

  // raw_output as expandable content (for non-terminal tools like Read, Search, etc.)
  const rawOutputLines = getRawOutputLines(tool);
  const needsPermission =
    tool.kind === "permission" &&
    tool.status === "Running" &&
    tool.permission_options.length > 0 &&
    !tool.permission_decision;

  // Does this card have expandable content?
  const hasDetail =
    !!errorLine ||
    !!cmdDetail ||
    detailLines.lines.length > 0 ||
    logEntries.entries.length > 0 ||
    outputLines.lines.length > 0 ||
    rawOutputLines.lines.length > 0 ||
    diffPreviews.length > 0 ||
    trackedDiffPaths.length > 0;

  return (
    <div className={`tc ${nested ? "tc-nested" : ""}`}>
      {/* Header line: bullet + verb + title + expand chevron on hover */}
      <div
        className={`tc-line tc-header-line ${hasDetail ? "tc-expandable" : ""}`}
        onClick={hasDetail ? () => setExpanded((v) => !v) : undefined}
      >
        <span className={`tc-bullet ${bullet.className}`}>{bullet.char}</span>
        <span className="tc-verb">{verb}</span>
        <span className="tc-cmd">{headerTitle}</span>
        {category === "editing" && (diffStats.added > 0 || diffStats.removed > 0) && (
          <span className="tc-diff-stats" aria-label={`${diffStats.added} 处添加，${diffStats.removed} 处删除`}>
            {diffStats.added > 0 && <span className="tc-diff-added">+{diffStats.added}</span>}
            {diffStats.removed > 0 && <span className="tc-diff-removed">-{diffStats.removed}</span>}
          </span>
        )}
        {hasDetail && (
          <span className={`tc-chevron ${expanded ? "tc-chevron-open" : ""}`}>
            ›
          </span>
        )}
      </div>

      {needsPermission && (
        <div className="tc-permission-panel">
          <div className="tc-permission-title">选择权限</div>
          <div className="tc-permission-actions">
            {tool.permission_options.map((option) => (
              <button
                key={option.id}
                className={`tc-permission-btn tc-permission-${permissionTone(option.kind)}`}
                type="button"
                onClick={(event) => {
                  event.stopPropagation();
                  onPermissionSelect(tool.call_id, option.id);
                }}
              >
                {option.label}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* Expandable detail — only visible when expanded */}
      {expanded && (
        <>
          {/* Command detail (actual command or file path) */}
          {cmdDetail && (
            <div className="tc-output-block">
              <div className="tc-output-line">
                <span className="tc-output-prefix">└ </span>
                <span className="tc-cmd-detail">{cmdDetail}</span>
              </div>
            </div>
          )}

          {detailLines.lines.length > 0 && (
            <div className="tc-output-block">
              {detailLines.lines.map((line, i) => (
                <div key={i} className="tc-output-line">
                  <span className="tc-output-prefix">
                    {i === 0 ? "└ " : "  "}
                  </span>
                  {line}
                </div>
              ))}
              {detailLines.omitted > 0 && (
                <div className="tc-output-line tc-output-ellipsis">
                  <span className="tc-output-prefix">  </span>… +
                  {detailLines.omitted} 行
                </div>
              )}
            </div>
          )}

          {logEntries.entries.length > 0 && (
            <div className="tc-output-block">
              {logEntries.entries.map((entry, i) => (
                <div key={`${entry.title}-${i}`} className="tc-output-line tc-log-line">
                  <span className="tc-output-prefix">
                    {i === 0 ? "└ " : "  "}
                  </span>
                  <span className="tc-log-title">{entry.title}</span>
                  <span className="tc-log-body">{entry.body}</span>
                </div>
              ))}
              {logEntries.omitted > 0 && (
                <div className="tc-output-line tc-output-ellipsis">
                  <span className="tc-output-prefix">  </span>… +
                  {logEntries.omitted} 条更新
                </div>
              )}
            </div>
          )}

          {/* Error line */}
          {errorLine && (
            <div className="tc-output-block">
              <div className="tc-output-line tc-output-error">
                <span className="tc-output-prefix">└ </span>
                {errorLine}
              </div>
            </div>
          )}

          {/* Output lines (max 5, only for terminal/command tools) */}
          {!errorLine && outputLines.lines.length > 0 && (
            <div className="tc-output-block">
              {outputLines.lines.map((line, i) => (
                <div key={i} className="tc-output-line">
                  <span className="tc-output-prefix">
                    {i === 0 ? "└ " : "  "}
                  </span>
                  {line}
                </div>
              ))}
              {outputLines.omitted > 0 && (
                <div className="tc-output-line tc-output-ellipsis">
                  <span className="tc-output-prefix">  </span>… +
                  {outputLines.omitted} 行
                </div>
              )}
            </div>
          )}

          {/* Raw output for non-terminal tools (Read, Search, etc.) */}
          {!errorLine && outputLines.lines.length === 0 && rawOutputLines.lines.length > 0 && (
            <div className="tc-output-block">
              {rawOutputLines.lines.map((line, i) => (
                <div key={i} className="tc-output-line">
                  <span className="tc-output-prefix">
                    {i === 0 ? "└ " : "  "}
                  </span>
                  {line}
                </div>
              ))}
              {rawOutputLines.omitted > 0 && (
                <div className="tc-output-line tc-output-ellipsis">
                  <span className="tc-output-prefix">  </span>… +
                  {rawOutputLines.omitted} 行
                </div>
              )}
            </div>
          )}

          {diffPreviews.length > 0 && (
            <div className="tc-diff-list">
              {diffPreviews.map((preview) => (
                <div className="tc-diff-preview" key={preview.path}>
                  <div className="tc-diff-path">{preview.path}</div>
                  <PatchDiff
                    patch={previewToCompactPatch(preview)}
                    className="tc-pierre-diff"
                    options={DIFF_OPTIONS}
                    disableWorkerPool
                  />
                </div>
              ))}
            </div>
          )}

          {diffPreviews.length === 0 && trackedDiffPaths.length > 0 && (
            <div className="tc-output-block">
              {trackedDiffPaths.map((p, i) => (
                <div key={i} className="tc-output-line">
                  <span className="tc-output-prefix">
                    {i === 0 ? "└ " : "  "}
                  </span>
                  <span className="tc-file-path">{p}</span>
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {/* Nested subtasks */}
      {children.length > 0 && (
        <div className="tc-children">
          <div
            className="tc-children-toggle"
            onClick={() => setChildrenCollapsed((v) => !v)}
          >
            <span className={`tc-children-chevron ${!childrenCollapsed ? "tc-children-chevron-open" : ""}`}>›</span>
            <span className="tc-children-count">
              {children.length} 个工具调用
            </span>
          </div>
          {!childrenCollapsed && children.map((child) => (
            <ToolCallCard
              key={child.id}
              tool={child}
              snapshot={snapshot}
              nested
              onPermissionSelect={onPermissionSelect}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface PatchLine {
  kind: DiffHunk["lines"][number]["kind"];
  content: string;
  oldStart: number;
  newStart: number;
}

interface PatchRange {
  start: number;
  end: number;
}

function getDiffStats(previews: ToolDiffPreview[]) {
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

function previewToCompactPatch(preview: ToolDiffPreview): string {
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
  let oldLine = 1;
  let newLine = 1;

  return hunks.flatMap((hunk) =>
    hunk.lines.map((line) => {
      const patchLine = {
        kind: line.kind,
        content: line.content,
        oldStart: oldLine,
        newStart: newLine,
      };

      if (line.kind !== "Added") oldLine += 1;
      if (line.kind !== "Removed") newLine += 1;

      return patchLine;
    })
  );
}

function compactPatchRanges(lines: PatchLine[]): PatchRange[] {
  const changedIndexes = lines
    .map((line, index) => (line.kind === "Context" ? -1 : index))
    .filter((index) => index >= 0);

  if (changedIndexes.length === 0) {
    return lines.length > 0 ? [{ start: 0, end: Math.min(lines.length, 12) }] : [];
  }

  const ranges: PatchRange[] = [];
  for (const index of changedIndexes) {
    const start = Math.max(0, index - DIFF_CONTEXT_LINES);
    const end = Math.min(lines.length, index + DIFF_CONTEXT_LINES + 1);
    const last = ranges[ranges.length - 1];

    if (last && start <= last.end) {
      last.end = Math.max(last.end, end);
    } else {
      ranges.push({ start, end });
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

function formatPatchRange(start: number, lineCount: number): string {
  if (lineCount === 0) return `${Math.max(0, start - 1)},0`;
  return lineCount === 1 ? `${start}` : `${start},${lineCount}`;
}

function normalizePatchPath(path: string): string {
  return path.replace(/\\/g, "/").replace(/^[a-zA-Z]:\//, "");
}

function permissionTone(kind: string) {
  const normalized = kind.toLowerCase();
  if (normalized.includes("reject")) return "reject";
  if (normalized.includes("always")) return "always";
  return "allow";
}

/**
 * Extract a short, human-readable title for the header line.
 * Prefers stable input metadata and file paths over completion output.
 * Keeps raw commands in the expanded detail instead of the header.
 */
function extractHeaderTitle(tool: ToolInvocation, trackedDiffPaths: string[]): string {
  const inputTitle = extractInputTitle(tool);
  if (inputTitle) return truncate(inputTitle, 80);

  // For edit tools, show workspace-relative path for context
  if (trackedDiffPaths.length > 0) {
    return truncate(trackedDiffPaths[trackedDiffPaths.length - 1].replace(/\\/g, "/"), 80);
  }

  const namePath = extractPathFromToolName(tool.name);
  if (namePath) {
    return shortPath(namePath);
  }

  // Use summary only if it looks like a real description, not output or file content.
  if (
    tool.summary &&
    !tool.summary.startsWith("Editing ") &&
    !looksLikeToolOutput(tool.summary) &&
    tool.summary !== tool.name &&
    !isGenericTitle(tool.summary) &&
    isUsefulTitle(tool.summary) &&
    !looksLikeCommand(tool.summary) &&
    !looksLikeDisplayPayload(tool.summary) &&
    !looksLikePath(tool.summary)
  ) {
    return truncate(tool.summary, 80);
  }

  // If name is a backtick-wrapped command, use just the tool kind
  if (tool.name.startsWith("`")) {
    return tool.kind || "命令";
  }

  if (isCommandTool(tool)) {
    return commandToolLabel(tool);
  }

  // If name itself is useful and not just a generic label like "Tool"
  if (
    isUsefulTitle(tool.name) &&
    !isGenericTitle(tool.name) &&
    !looksLikeCommand(tool.name) &&
    !looksLikeDisplayPayload(tool.name)
  ) {
    return truncate(tool.name, 80);
  }

  // Last resort: tool kind or generic label
  return tool.kind || tool.name || "工具";
}

/**
 * Extract a human-readable title from raw_input for the header.
 * Prefers description > short file name > pattern. NOT raw commands.
 */
function extractInputTitle(tool: ToolInvocation): string | null {
  if (!tool.raw_input) return null;
  try {
    const input = JSON.parse(tool.raw_input);

    // Description is the best header title (human-readable summary)
    if (input.description && typeof input.description === "string") {
      return input.description;
    }

    // File path: show filename only for header
    if (input.file_path || input.filePath || input.path) {
      const p = String(input.file_path || input.filePath || input.path);
      return shortPath(p);
    }

    // Pattern (grep/glob)
    if (input.pattern && typeof input.pattern === "string") {
      const path = input.path || input.include;
      return path ? `${input.pattern} in ${shortPath(String(path))}` : input.pattern;
    }

    // URL, prompt, query
    if (input.url && typeof input.url === "string") return truncate(input.url, 60);
    if (input.prompt && typeof input.prompt === "string") return truncate(input.prompt, 60);
    if (input.query && typeof input.query === "string") return truncate(input.query, 60);

    // Commands belong in the expanded detail, not the header.
  } catch {
    if (
      tool.raw_input &&
      looksLikePath(tool.raw_input) &&
      !looksLikeCommand(tool.raw_input)
    ) {
      return shortPath(tool.raw_input);
    }
    if (
      tool.raw_input &&
      isUsefulTitle(tool.raw_input) &&
      !looksLikeCommand(tool.raw_input) &&
      !looksLikePath(tool.raw_input)
    ) {
      return tool.raw_input;
    }
  }
  return null;
}

function isGenericTitle(text: string): boolean {
  const lower = text.trim().toLowerCase();
  return (
    lower === "tool" ||
    lower === "completed" ||
    lower === "succeeded" ||
    lower === "executing" ||
    lower === "working" ||
    lower === "tool failed"
  );
}

function extractPathFromToolName(name: string): string | null {
  const match = name.match(/^(?:Write|Read|Edit)\s+`?(.+?)`?$/i);
  return match?.[1] ?? null;
}

/** Check if a string is a useful human-readable title (not garbage/fragments) */
function isUsefulTitle(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length < 3) return false;
  // JSON fragments: starts/ends with braces, brackets, quotes
  if (/^[{}\[\]"'`]$/.test(trimmed)) return false;
  if (/^[{}\[\]"'`]/.test(trimmed) && trimmed.length < 6) return false;
  // Pure punctuation or whitespace
  if (/^[\s\W]+$/.test(trimmed)) return false;
  return true;
}

/** Check if a string looks like a file path (contains slashes or backslashes) */
function looksLikePath(text: string): boolean {
  return /[/\\]/.test(text.trim());
}

function looksLikeCommand(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith("`") && trimmed.endsWith("`")) return true;
  if (/[;&|]/.test(trimmed)) return true;
  return /^(?:bash|sh|cmd|powershell|pwsh|npm|pnpm|yarn|bun|cargo|git|ls|dir|cd|mkdir|rm|cp|mv|python|node|npx)\b/i.test(
    trimmed
  );
}

function looksLikeDisplayPayload(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;
  if (trimmed.includes("\n")) return true;
  if (/^\d+\s*[→:|]\s*/.test(trimmed)) return true;
  if (/^#{1,6}\s+/.test(trimmed)) return true;
  if (/^(?:import|export|function|class|const|let|var|use|pub)\s/.test(trimmed)) return true;
  if (/^Successfully\s+(?:edited|wrote|updated)\s+file:/i.test(trimmed)) return true;
  return false;
}

/** Returns true for strings that look like tool completion output, not a description */
function looksLikeToolOutput(text: string): boolean {
  const trimmed = text.trim();
  if (/^Exit code:\s*\d/i.test(trimmed)) return true;
  if (/^Completed\s*\|/i.test(trimmed)) return true;
  if (/^Exited with code\s/i.test(trimmed)) return true;
  return false;
}

/**
 * Extract the detailed command/path for the expandable section.
 * Returns null if there's nothing meaningful to show beyond the header.
 */
function extractCommandDetail(tool: ToolInvocation, trackedDiffPaths: string[]): string | null {
  // For edit tools, show full file path
  if (trackedDiffPaths.length > 0) {
    return trackedDiffPaths[trackedDiffPaths.length - 1];
  }

  if (tool.raw_input) {
    try {
      const input = JSON.parse(tool.raw_input);

      if (input.command) {
        return String(input.command);
      }

      if (input.file_path || input.filePath || input.path) {
        return String(input.file_path || input.filePath || input.path);
      }

      if (input.pattern) {
        const pattern = String(input.pattern);
        const path = input.path || input.include;
        return path ? `${pattern} in ${path}` : pattern;
      }

      if (input.url) return String(input.url);
      if (input.query) return String(input.query);
    } catch {
      return tool.raw_input;
    }
  }

  // If the name itself looks like a command (backtick-wrapped), show it
  if (tool.name.startsWith("`") && tool.name.endsWith("`")) {
    return tool.name.slice(1, -1);
  }

  return null;
}

/** Extract filename from a full path */
function shortPath(fullPath: string): string {
  const cleaned = fullPath.trim().replace(/^[`'"]+|[`'"]+$/g, "");
  const parts = cleaned.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || cleaned;
}

/** Truncate to max chars, single line */
function truncate(text: string, max: number): string {
  const firstLine = text.split("\n")[0] ?? text;
  if (firstLine.length > max) {
    return firstLine.slice(0, max - 3) + "...";
  }
  return firstLine;
}

type ToolCategory = "exploring" | "editing" | "executing";

function classifyTool(tool: ToolInvocation): ToolCategory {
  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  const subagentType = getSubagentType(tool);

  if (isExplicitEditToolInvocation(tool)) {
    return "editing";
  }

  if (subagentType === "explore") {
    return "exploring";
  }

  if (isCommandTool(tool)) {
    return "executing";
  }
  if (isExploreTool(tool, `${identity} ${tool.summary}`.toLowerCase())) {
    return "exploring";
  }
  return "executing";
}

function isExplicitEditTool(identity: string): boolean {
  return /(^|[\s._:-])(?:edit|write|patch|apply[_-]?patch)([\s._:-]|$)/.test(
    identity
  );
}

function isExplicitEditToolInvocation(tool: ToolInvocation): boolean {
  return isExplicitEditTool(`${tool.kind} ${tool.name}`.toLowerCase());
}

function getTrackedDiffPaths(tool: ToolInvocation): string[] {
  return isExplicitEditToolInvocation(tool) ? tool.diff_paths : [];
}

function getTrackedDiffPreviews(tool: ToolInvocation): ToolDiffPreview[] {
  return isExplicitEditToolInvocation(tool) ? tool.diff_previews ?? [] : [];
}

function getSubagentType(tool: ToolInvocation): string | null {
  if (!tool.is_subagent || !tool.raw_input) return null;
  try {
    const input = JSON.parse(tool.raw_input);
    return typeof input.subagent_type === "string"
      ? input.subagent_type.toLowerCase()
      : null;
  } catch {
    return null;
  }
}

function normalizeComparableText(text: string | null | undefined): string {
  return (text ?? "").replace(/\r\n/g, "\n").trim();
}

function getDetailLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  const detail = normalizeComparableText(tool.detail_text);
  if (!detail) return { lines: [], omitted: 0 };

  const allLines = detail.split("\n");
  if (allLines.length <= MAX_OUTPUT_LINES) {
    return { lines: allLines, omitted: 0 };
  }

  const tail = allLines.slice(-MAX_OUTPUT_LINES);
  return { lines: tail, omitted: allLines.length - MAX_OUTPUT_LINES };
}

function getVisibleLogEntries(tool: ToolInvocation): {
  entries: ToolInvocation["logs"];
  omitted: number;
} {
  const detail = normalizeComparableText(tool.detail_text);
  const rawOutput = normalizeComparableText(tool.raw_output);
  const entries = tool.logs.filter((entry) => {
    const body = normalizeComparableText(entry.body);
    if (!body) return false;
    if (detail && body === detail) return false;
    if (rawOutput && body === rawOutput) return false;
    return true;
  });

  if (entries.length <= MAX_OUTPUT_LINES) {
    return { entries, omitted: 0 };
  }

  const tail = entries.slice(-MAX_OUTPUT_LINES);
  return { entries: tail, omitted: entries.length - MAX_OUTPUT_LINES };
}

function isExploreTool(tool: ToolInvocation, lower: string): boolean {
  if (
    lower.includes("read") ||
    lower.includes("view") ||
    lower.includes("open") ||
    lower.includes("search") ||
    lower.includes("list") ||
    lower.includes("glob") ||
    lower.includes("grep") ||
    lower.includes("find") ||
    lower.includes("webfetch") ||
    lower.includes("fetch")
  ) {
    return true;
  }
  return rawInputHasPath(tool);
}

function rawInputHasPath(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input);
    return !!(input.file_path || input.filePath || input.path);
  } catch {
    return looksLikePath(tool.raw_input) && !looksLikeCommand(tool.raw_input);
  }
}

function rawInputHasCommand(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input);
    return typeof input.command === "string" && input.command.trim().length > 0;
  } catch {
    return looksLikeCommand(tool.raw_input);
  }
}

function isCommandTool(tool: ToolInvocation): boolean {
  const kind = tool.kind.trim().toLowerCase();
  const name = tool.name.trim().toLowerCase();
  return (
    tool.name.trim().startsWith("`") ||
    kind === "bash" ||
    name === "bash" ||
    (kind === "execute" && rawInputHasCommand(tool)) ||
    kind === "command" ||
    kind === "terminal" ||
    name === "command" ||
    name === "terminal"
  );
}

function commandToolLabel(tool: ToolInvocation): string {
  const name = tool.name.trim();
  const kind = tool.kind.trim();
  if (name && !name.startsWith("`") && !isGenericTitle(name)) {
    return truncate(name, 80);
  }
  if (kind && !isGenericTitle(kind) && kind.toLowerCase() !== "execute") {
    return truncate(kind, 80);
  }
  return "Command";
}

function toolVerb(status: ToolStatus, category: ToolCategory): string {
  if (status === "Failed") return "失败";
  if (status === "Interrupted") return "已中断";

  const running = status === "Running" || status === "Pending";
  switch (category) {
    case "exploring":
      return running ? "探索中" : "已探索";
    case "editing":
      return running ? "编辑中" : "已编辑";
    case "executing":
      return running ? "运行中" : "已运行";
  }
}

function statusBullet(
  status: ToolStatus
): { char: string; className: string } {
  switch (status) {
    case "Pending":
    case "Running":
      return { char: "•", className: "tc-bullet-active" };
    case "Succeeded":
      return { char: "•", className: "tc-bullet-ok" };
    case "Failed":
      return { char: "•", className: "tc-bullet-err" };
    case "Interrupted":
      return { char: "•", className: "tc-bullet-warn" };
  }
}

function getOutputLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  if (tool.terminal_output) {
    const raw = tool.terminal_output.output.trim();
    if (!raw) {
      const code = tool.terminal_output.exit_code;
      if (code !== null && code !== 0) {
        return { lines: [`(退出码 ${code})`], omitted: 0 };
      }
      return { lines: [], omitted: 0 };
    }
    const allLines = raw.split("\n");
    if (allLines.length <= MAX_OUTPUT_LINES) {
      return { lines: allLines, omitted: 0 };
    }
    const head = allLines.slice(0, MAX_OUTPUT_LINES);
    return { lines: head, omitted: allLines.length - MAX_OUTPUT_LINES };
  }

  return { lines: [], omitted: 0 };
}

/** Get displayable lines from raw_output (for non-terminal tools) */
function getRawOutputLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  // Skip if terminal_output exists (handled by getOutputLines)
  if (tool.terminal_output) return { lines: [], omitted: 0 };

  const raw = tool.raw_output?.trim();
  if (!raw) return { lines: [], omitted: 0 };
  const normalizedRaw = normalizeComparableText(raw);
  if (normalizedRaw === normalizeComparableText(tool.detail_text)) {
    return { lines: [], omitted: 0 };
  }
  if (tool.logs.some((entry) => normalizeComparableText(entry.body) === normalizedRaw)) {
    return { lines: [], omitted: 0 };
  }

  // Skip vague/unhelpful outputs
  if (isVagueError(raw)) return { lines: [], omitted: 0 };
  // Skip outputs that just repeat the summary
  if (raw === tool.summary) return { lines: [], omitted: 0 };
  // Skip very short outputs that add no value (like "Completed", "OK")
  if (raw.length < 10 && !raw.includes("\n")) return { lines: [], omitted: 0 };

  const allLines = raw.split("\n");
  if (allLines.length <= MAX_OUTPUT_LINES) {
    return { lines: allLines, omitted: 0 };
  }
  const head = allLines.slice(0, MAX_OUTPUT_LINES);
  return { lines: head, omitted: allLines.length - MAX_OUTPUT_LINES };
}

/** Returns true for vague/unhelpful server errors that add no value when displayed */
function isVagueError(error: string): boolean {
  const lower = error.toLowerCase().trim();
  return (
    lower === "internal error" ||
    lower === "error" ||
    lower === "failed" ||
    lower === "tool call failed" ||
    lower === "tool failed" ||
    lower === "unknown error" ||
    lower.startsWith("internal error (tool:")
  );
}
