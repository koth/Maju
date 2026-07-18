import { memo, useState } from "react";
import { PatchDiff } from "@pierre/diffs/react";
import type { DiffHunk, ToolDiffPreview, ToolInvocation } from "../../types";
import { useHorizontalScrollControls } from "../../lib/use-horizontal-scroll-controls";
import { deriveToolPresentation, type ToolPresentation } from "./tool-presentation";
import { getDiffStats, previewToCompactPatch } from "./compact-patch";
import {
  classifyCommandPresentation,
  classifyTool,
  commandHeaderTitle,
  diffPreviewsFromApplyPatchCommand,
  extractCommandDetail,
  extractHeaderTitle,
  filterCompletedCommandEditPaths,
  getCommandMutationDiffPaths,
  getDetailLines,
  getExplorationResult,
  getOutputLines,
  getRawOutputLines,
  getTrackedDiffPaths,
  getTrackedDiffPreviews,
  getVisibleLogEntries,
  isVagueError,
  rawInputHasEditPayload,
  rawInputHasReadOnlyParsedCommand,
  sameOrNestedPath,
  statusBullet,
  toolVerb,
  uniqueStrings,
  type ToolCategory,
} from "./tool-card-analysis";
import "./ToolCallCard.css";

const DIFF_OPTIONS = {
  diffStyle: "unified",
  disableFileHeader: true,
  hunkSeparators: "metadata",
  lineDiffType: "word",
  overflow: "wrap",
  themeType: "dark",
} as const;

export { previewToCompactPatch } from "./compact-patch";

interface Props {
  tool: ToolInvocation;
  childToolsByParent?: Map<string, ToolInvocation[]>;
  nested: boolean;
  onPermissionSelect: (requestId: string, optionId: string | null, guidance?: string | null) => void;
  hiddenPermissionRequestIds?: ReadonlySet<string>;
  onCancelTurn?: () => Promise<void> | void;
  onStopTool?: (toolCallId: string) => Promise<void> | void;
}

function ToolCallCardImpl({
  tool,
  childToolsByParent,
  nested,
  onPermissionSelect,
  hiddenPermissionRequestIds,
  onCancelTurn,
  onStopTool,
}: Props) {
  const hiddenPermissionRequest =
    hiddenPermissionRequestIds?.has(tool.call_id);

  const [expanded, setExpanded] = useState(false);
  const [rawDetailsOpen, setRawDetailsOpen] = useState(false);
  const [stopRequested, setStopRequested] = useState(false);

  const children = childToolsByParent?.get(tool.call_id) ?? [];

  const [childrenCollapsed, setChildrenCollapsed] = useState(true);

  if (hiddenPermissionRequest) {
    return null;
  }

  const presentation = deriveToolPresentation(tool);
  const commandApplyPatchPreviews =
    presentation.presentationKind === "command"
      ? diffPreviewsFromApplyPatchCommand(presentation.command)
      : [];
  const rawCommandEditPaths =
    presentation.presentationKind === "command"
      ? uniqueStrings([
          ...getCommandMutationDiffPaths(tool, presentation.command),
          ...commandApplyPatchPreviews.map((preview) => preview.path),
        ])
      : [];
  const commandEditPaths =
    presentation.presentationKind === "command"
      ? filterCompletedCommandEditPaths(tool, presentation.command, rawCommandEditPaths)
      : [];
  const trackedDiffPaths = getTrackedDiffPaths(tool, commandEditPaths);
  const trackedDiffPreviews = getTrackedDiffPreviews(tool, commandEditPaths);
  const diffPreviews =
    trackedDiffPreviews.length > 0
      ? trackedDiffPreviews
      : commandApplyPatchPreviews.filter((preview) =>
          commandEditPaths.some((path) => sameOrNestedPath(path, preview.path)),
        );
  const readOnlyParsedCommand = rawInputHasReadOnlyParsedCommand(tool);
  const category: ToolCategory =
    rawInputHasEditPayload(tool) || commandEditPaths.length > 0
      ? "editing"
      : readOnlyParsedCommand
      ? "exploring"
      : presentation.presentationKind === "command"
      ? classifyCommandPresentation(presentation.command)
      : classifyTool(tool);
  const bullet = statusBullet(tool.status);
  const verb = toolVerb(tool.status, category);
  const headerTitle =
    category === "editing"
      ? extractHeaderTitle(tool, trackedDiffPaths)
      : presentation.presentationKind === "command"
      ? commandHeaderTitle(presentation.command, category, tool)
      : extractHeaderTitle(tool, trackedDiffPaths);
  const cmdDetail = extractCommandDetail(tool, trackedDiffPaths);
  const outputLines = getOutputLines(tool);
  const detailLines = getDetailLines(tool);
  const logEntries = getVisibleLogEntries(tool);
  const errorLine =
    tool.error && !isVagueError(tool.error) ? tool.error : null;
  const diffStats = getDiffStats(diffPreviews);

  // raw_output as expandable content (for non-terminal tools like Read, Search, etc.)
  const rawOutputLines = getRawOutputLines(tool);
  const explorationResult =
    presentation.presentationKind !== "command" && category === "exploring"
      ? getExplorationResult(tool, cmdDetail, detailLines.lines, outputLines.lines, rawOutputLines.lines)
      : null;
  const shellPresentation =
    presentation.presentationKind === "command" && category !== "editing"
      ? presentation
      : category === "exploring"
        ? deriveExplorationShellPresentation(
            tool,
            presentation,
            explorationResult,
            cmdDetail,
            errorLine,
            logEntries.entries,
            detailLines.lines,
            outputLines.lines,
            rawOutputLines.lines,
          )
        : null;
  const needsPermission =
    tool.status === "Running" &&
    tool.permission_options.length > 0 &&
    !tool.permission_decision;
  const canStopTool =
    !!onStopTool &&
    tool.can_stop &&
    (tool.status === "Pending" || tool.status === "Running");
  const handleStopTool = async () => {
    if (!onStopTool || stopRequested) return;
    setStopRequested(true);
    try {
      await onStopTool(tool.call_id);
    } catch (_error) {
      // The session state poll after stop is the user-visible error path.
    } finally {
      setStopRequested(false);
    }
  };

// Editing cards with a real diff should expand to the patch only.
  // Extra path/output/log noise makes the review view hard to scan.
  const showEditingDiffOnly = category === "editing" && diffPreviews.length > 0;

  // Does this card have expandable content?
  const hasDetail = showEditingDiffOnly
    ? true
    : !!errorLine ||
      !!cmdDetail ||
      detailLines.lines.length > 0 ||
      logEntries.entries.length > 0 ||
      outputLines.lines.length > 0 ||
      rawOutputLines.lines.length > 0 ||
      presentation.command != null ||
      presentation.primaryOutput != null ||
      presentation.rawDetails.length > 0 ||
      trackedDiffPaths.length > 0;

  return (
    <div className={`tc ${nested ? "tc-nested" : ""}`}>
      {/* Header line: bullet + verb + title + expand chevron on hover */}
      <div className="tc-line-wrap">
        <button
          type="button"
          className={`tc-line tc-header-line ${hasDetail ? "tc-expandable" : ""}`}
          onClick={hasDetail ? () => setExpanded((v) => !v) : undefined}
          aria-expanded={hasDetail ? expanded : undefined}
          disabled={!hasDetail}
        >
          <span className={`tc-bullet ${bullet.className}`}>{bullet.char}</span>
          <span className="tc-verb">{verb}</span>
          <span className="tc-cmd">{headerTitle}</span>
          {category === "editing" && (diffStats.added > 0 || diffStats.removed > 0) && (
            <span className="tc-diff-stats" aria-label={`${diffStats.added} 处添加，${diffStats.removed} 处删除`}>
              <span className="tc-diff-added">+{diffStats.added}</span>
              <span className="tc-diff-removed">-{diffStats.removed}</span>
            </span>
          )}
          {hasDetail && (
            <span className={`tc-chevron ${expanded ? "tc-chevron-open" : ""}`}>
              ›
            </span>
          )}
        </button>
        {canStopTool && (
          <button
            className="tc-stop-turn-btn"
            type="button"
            onClick={handleStopTool}
            disabled={stopRequested}
            aria-label="停止工具调用"
            title="停止工具调用"
          >
            {stopRequested ? "停止中" : "停止"}
          </button>
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
          {showEditingDiffOnly ? (
            <div className="tc-diff-list">
              {diffPreviews.map((preview) => (
                <ToolDiffPreviewCard key={preview.path} preview={preview} />
              ))}
            </div>
          ) : (
            <>
              {shellPresentation && (
                <ShellToolPanel
                  presentation={shellPresentation}
                  rawDetailsOpen={rawDetailsOpen}
                  onRawDetailsToggle={() => setRawDetailsOpen((value) => !value)}
                  onStopTool={canStopTool ? handleStopTool : undefined}
                  stopRequested={stopRequested}
                />
              )}

              {/* Command detail (actual command or file path) */}
              {!shellPresentation &&
                (presentation.presentationKind !== "command" || category === "editing") &&
                cmdDetail && (
                <div className="tc-output-block">
                  <div className="tc-output-line">
                    <span className="tc-output-prefix">└ </span>
                    <span className="tc-cmd-detail">{cmdDetail}</span>
                  </div>
                </div>
              )}

              {!shellPresentation && presentation.presentationKind !== "command" && detailLines.lines.length > 0 && (
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

              {!shellPresentation && presentation.presentationKind !== "command" && logEntries.entries.length > 0 && (
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
              {!shellPresentation && presentation.presentationKind !== "command" && errorLine && (
                <div className="tc-output-block">
                  <div className="tc-output-line tc-output-error">
                    <span className="tc-output-prefix">└ </span>
                    {errorLine}
                  </div>
                </div>
              )}

              {/* Output lines (max 5, only for terminal/command tools) */}
              {!shellPresentation && presentation.presentationKind !== "command" && !errorLine && outputLines.lines.length > 0 && (
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
              {!shellPresentation && presentation.presentationKind !== "command" && !errorLine && outputLines.lines.length === 0 && rawOutputLines.lines.length > 0 && (
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

              {!shellPresentation && diffPreviews.length === 0 && trackedDiffPaths.length > 0 && (
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
              childToolsByParent={childToolsByParent}
              nested
              onPermissionSelect={onPermissionSelect}
              hiddenPermissionRequestIds={hiddenPermissionRequestIds}
              onCancelTurn={onCancelTurn}
              onStopTool={onStopTool}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface ShellToolPanelProps {
  presentation: ToolPresentation;
  rawDetailsOpen: boolean;
  onRawDetailsToggle: () => void;
  onStopTool?: () => Promise<void> | void;
  stopRequested?: boolean;
}

function ShellToolPanel({
  presentation,
  rawDetailsOpen,
  onRawDetailsToggle,
  onStopTool,
  stopRequested = false,
}: ShellToolPanelProps) {
  const hasRawDetails = presentation.rawDetails.length > 0;
  return (
    <div className="tc-shell-panel">
      <div className="tc-shell-header">
        <div className="tc-shell-label">{presentation.toolLabel}</div>
        <div className="tc-shell-header-actions">
          {hasRawDetails && (
            <button
              className="tc-raw-toggle"
              type="button"
              onClick={onRawDetailsToggle}
              aria-expanded={rawDetailsOpen}
            >
              原始详情
            </button>
          )}
          {onStopTool && (
            <button
              className="tc-stop-turn-btn tc-shell-stop-btn"
              type="button"
              onClick={onStopTool}
              disabled={stopRequested}
            >
              {stopRequested ? "停止中" : "停止工具"}
            </button>
          )}
          <span className={`tc-shell-status tc-shell-status-${presentation.footerStatus.tone}`}>
            {presentation.footerStatus.tone === "success" ? "✓ " : ""}
            {presentation.footerStatus.label}
          </span>
        </div>
      </div>

      <div className="tc-shell-body">
        {presentation.command && (
          <pre className="tc-shell-command">
            <span className="tc-shell-prompt">$ </span>
            {presentation.command}
          </pre>
        )}
        {presentation.primaryOutput && (
          <pre className="tc-shell-output">{presentation.primaryOutput}</pre>
        )}
        {!presentation.primaryOutput && !presentation.command && (
          <div className="tc-shell-empty">没有可显示的输出</div>
        )}
      </div>

      {hasRawDetails && rawDetailsOpen && (
        <div className="tc-raw-details">
          {presentation.rawDetails.map((detail) => (
            <div className="tc-raw-detail" key={detail.title}>
              <div className="tc-raw-title">{detail.title}</div>
              <pre className="tc-raw-body">{detail.body}</pre>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function ToolDiffPreviewCard({ preview }: { preview: ToolDiffPreview }) {
  const horizontalScroll = useHorizontalScrollControls<HTMLDivElement>();

  return (
    <div className="tc-diff-preview">
      <div className="tc-diff-path">{preview.path}</div>
      <div
        {...horizontalScroll.scrollControlProps}
        className="tc-pierre-diff-scroll"
      >
        <PatchDiff
          patch={previewToCompactPatch(preview)}
          className="tc-pierre-diff"
          options={DIFF_OPTIONS}
          disableWorkerPool
        />
      </div>
    </div>
  );
}

function deriveExplorationShellPresentation(
  tool: ToolInvocation,
  presentation: ToolPresentation,
  result: {
    root: string | null;
    items: string[];
    omitted: number;
  } | null,
  cmdDetail: string | null,
  errorLine: string | null,
  logEntries: Array<{ title: string; body: string }>,
  detailLines: string[],
  outputLines: string[],
  rawOutputLines: string[],
): ToolPresentation {
  const command =
    presentation.command ??
    cmdDetail ??
    result?.root ??
    extractHeaderTitle(tool, []) ??
    tool.summary ??
    null;

  const logLines = logEntries
    .map((entry) => {
      const title = entry.title.trim();
      const body = entry.body.trim();
      if (!title && !body) return null;
      if (!title) return body;
      if (!body) return title;
      return `${title} ${body}`;
    })
    .filter((line): line is string => !!line);

  const outputParts = [
    result && result.items.length > 0
      ? result.items.join("\n")
      : uniqueStrings([
          ...detailLines,
          ...logLines,
          ...outputLines,
          ...rawOutputLines,
        ]).join("\n"),
    result && result.omitted > 0 ? `… +${result.omitted} 项` : null,
    errorLine,
  ].filter((part): part is string => !!part && part.trim().length > 0);

  return {
    ...presentation,
    presentationKind: "command",
    toolLabel: "Explore",
    command,
    primaryOutput: outputParts.length > 0 ? outputParts.join("\n\n") : presentation.primaryOutput,
  };
}

export const ToolCallCard = memo(ToolCallCardImpl, areToolCardPropsEqual);

function areToolCardPropsEqual(prev: Props, next: Props) {
  if (prev.nested !== next.nested) return false;
  if (prev.onPermissionSelect !== next.onPermissionSelect) return false;
  if (prev.onCancelTurn !== next.onCancelTurn) return false;
  if (prev.onStopTool !== next.onStopTool) return false;
  if (
    !sameReadonlySet(
      prev.hiddenPermissionRequestIds,
      next.hiddenPermissionRequestIds,
    )
  )
    return false;
  if (!sameToolForRender(prev.tool, next.tool)) return false;
  return sameChildToolsForRender(
    prev.childToolsByParent?.get(prev.tool.call_id) ?? [],
    next.childToolsByParent?.get(next.tool.call_id) ?? [],
  );
}

function sameReadonlySet<T>(
  prev: ReadonlySet<T> | undefined,
  next: ReadonlySet<T> | undefined,
) {
  if (prev === next) return true;
  const prevSize = prev?.size ?? 0;
  const nextSize = next?.size ?? 0;
  if (prevSize !== nextSize) return false;
  if (!prev || !next) return prevSize === 0 && nextSize === 0;
  for (const value of prev) {
    if (!next.has(value)) return false;
  }
  return true;
}

function sameChildToolsForRender(prev: ToolInvocation[], next: ToolInvocation[]) {
  if (prev === next) return true;
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (prev[i] === next[i]) continue;
    if (!sameToolForRender(prev[i], next[i])) return false;
  }
  return true;
}

function sameToolForRender(prev: ToolInvocation, next: ToolInvocation) {
  if (prev === next) return true;
  return (
    prev.id === next.id &&
    prev.call_id === next.call_id &&
    prev.parent_call_id === next.parent_call_id &&
    prev.name === next.name &&
    prev.kind === next.kind &&
    prev.summary === next.summary &&
    prev.status === next.status &&
    prev.is_subagent === next.is_subagent &&
    prev.detail_text === next.detail_text &&
    prev.raw_input === next.raw_input &&
    prev.raw_output === next.raw_output &&
    prev.error === next.error &&
    prev.permission_decision === next.permission_decision &&
    prev.can_stop === next.can_stop &&
    prev.stop_kind === next.stop_kind &&
    prev.stop_status === next.stop_status &&
    sameStringArray(prev.diff_paths, next.diff_paths) &&
    sameLogs(prev.logs, next.logs) &&
    samePermissionOptions(prev.permission_options, next.permission_options) &&
    samePermissionInput(prev.permission_input, next.permission_input) &&
    sameTerminalOutput(prev.terminal_output, next.terminal_output) &&
    sameDiffPreviews(prev.diff_previews, next.diff_previews)
  );
}

function sameStringArray(prev: string[], next: string[]) {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (prev[i] !== next[i]) return false;
  }
  return true;
}

function sameLogs(prev: ToolInvocation["logs"], next: ToolInvocation["logs"]) {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (prev[i].title !== next[i].title || prev[i].body !== next[i].body) {
      return false;
    }
  }
  return true;
}

function samePermissionOptions(
  prev: ToolInvocation["permission_options"],
  next: ToolInvocation["permission_options"],
) {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (
      prev[i].id !== next[i].id ||
      prev[i].label !== next[i].label ||
      prev[i].kind !== next[i].kind
    ) {
      return false;
    }
  }
  return true;
}

function samePermissionInput(
  prev: ToolInvocation["permission_input"],
  next: ToolInvocation["permission_input"],
) {
  if (prev === next) return true;
  if (!prev || !next) return false;
  return JSON.stringify(prev) === JSON.stringify(next);
}

function sameTerminalOutput(
  prev: ToolInvocation["terminal_output"],
  next: ToolInvocation["terminal_output"],
) {
  if (prev === next) return true;
  if (!prev || !next) return false;
  return prev.exit_code === next.exit_code && prev.output === next.output;
}

function sameDiffPreviews(prev: ToolDiffPreview[], next: ToolDiffPreview[]) {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (prev[i].path !== next[i].path) return false;
    if (!sameDiffHunks(prev[i].hunks, next[i].hunks)) return false;
  }
  return true;
}

function sameDiffHunks(prev: DiffHunk[], next: DiffHunk[]) {
  if (prev.length !== next.length) return false;
  for (let i = 0; i < prev.length; i += 1) {
    if (prev[i].heading !== next[i].heading) return false;
    const prevLines = prev[i].lines;
    const nextLines = next[i].lines;
    if (prevLines.length !== nextLines.length) return false;
    for (let j = 0; j < prevLines.length; j += 1) {
      if (prevLines[j].kind !== nextLines[j].kind || prevLines[j].content !== nextLines[j].content) {
        return false;
      }
    }
  }
  return true;
}

function permissionTone(kind: string) {
  const normalized = kind.toLowerCase();
  if (normalized.includes("reject")) return "reject";
  if (normalized.includes("always")) return "always";
  return "allow";
}
