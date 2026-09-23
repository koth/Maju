import { memo, useEffect, useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { PatchDiff } from "@pierre/diffs/react";
import type { AppTheme, DiffHunk, ToolDiffPreview, ToolInvocation } from "../../types";
import { sessionGetToolDetail } from "../../lib/tauri";
import { useCurrentAppTheme } from "../../lib/use-app-theme";
import { useHorizontalScrollControls } from "../../lib/use-horizontal-scroll-controls";
import { deriveToolPresentation, type ToolPresentation } from "./tool-presentation";
import { getDiffStats, previewToCompactPatch } from "./compact-patch";
import {
  classifyCommandPresentation,
  classifyTool,
  commandHeaderTitle,
  diffPreviewsFromApplyPatchCommand,
  displayPath,
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
  imageGenerationImages,
  imageGenerationMode,
  imageGenerationPromptTitle,
  imageGenerationVerb,
  isVagueError,
  rawInputHasEditPayload,
  rawInputHasReadOnlyParsedCommand,
  sameOrNestedPath,
  statusBullet,
  toolVerb,
  type ImageGenerationMode,
  type StatusBullet,
  uniqueStrings,
  type ToolCategory,
} from "./tool-card-analysis";
import "./ToolCallCard.css";

const DIFF_OPTIONS_BASE = {
  diffStyle: "unified",
  disableFileHeader: true,
  hunkSeparators: "metadata",
  lineDiffType: "word",
  overflow: "wrap",
} as const;

/**
 * Diff options for the transcript's inline patch.
 *
 * pierre resolves `theme` / `themeType` in JavaScript before its shadow DOM
 * paints, so the light theme cannot reach it through CSS variables. This card
 * was the last place asking for the dark palette unconditionally, which left a
 * dark diff block (dark rows, dark syntax) inside the light-theme transcript.
 * Every other surface here is token-driven and flips on its own.
 */
function toolDiffOptions(appTheme: AppTheme) {
  return {
    ...DIFF_OPTIONS_BASE,
    theme: appTheme === "light" ? "pierre-light" : "pierre-dark",
    themeType: appTheme === "light" ? "light" : "dark",
  } as const;
}

// Snapshots cap large tool text (raw_output at 8K chars, raw_input/detail at
// 4K). When a capped card is expanded we fetch the uncapped stored detail
// once and cache it per tool id, so full fidelity stays available without
// keeping the heavy text resident in every snapshot.
const SNAPSHOT_RAW_INPUT_CAP_CHARS = 4 * 1024;
const SNAPSHOT_RAW_OUTPUT_CAP_CHARS = 8 * 1024;

const toolDetailCache = new Map<string, ToolInvocation>();
const toolDetailInflight = new Map<string, Promise<void>>();

/**
 * The backend marks truncated snapshot payloads so short-but-incomplete
 * copies still get rehydrated on expand:
 *   - `raw_input` keeps priority fields and is re-serialized with a
 *     `"_truncated": true` marker (see `cap_snapshot_tool_raw_input`), so a
 *     file-create payload that dropped its `file_text` can be far shorter
 *     than the 4K length gate yet still be incomplete.
 *   - `raw_output` / `detail_text` are plain char-capped and carry
 *     `"... omitted N chars ..."` or a trailing `"\n..."` sentinel.
 */
function snapshotPayloadLooksTruncated(value: string | null | undefined): boolean {
  if (!value) return false;
  return value.includes('"_truncated"') || value.includes("omitted ") || value.endsWith("\n...");
}

function toolDetailLooksCapped(tool: ToolInvocation): boolean {
  return (
    (tool.raw_input?.length ?? 0) >= SNAPSHOT_RAW_INPUT_CAP_CHARS ||
    (tool.raw_output?.length ?? 0) >= SNAPSHOT_RAW_OUTPUT_CAP_CHARS ||
    tool.detail_text.length >= SNAPSHOT_RAW_INPUT_CAP_CHARS ||
    // The length gates above miss smart-compacted payloads that are short but
    // still incomplete (e.g. a create call whose `file_text` was stripped).
    // Honor the truncation markers so expanding still pulls the full record
    // and the diff surface can rebuild the whole-file preview.
    snapshotPayloadLooksTruncated(tool.raw_input) ||
    snapshotPayloadLooksTruncated(tool.raw_output) ||
    snapshotPayloadLooksTruncated(tool.detail_text)
  );
}

function mergeToolDetail(base: ToolInvocation, detail: ToolInvocation): ToolInvocation {
  return {
    ...base,
    // The lazily-loaded detail comes from the store (persisted before the
    // current session's mapping fixes); keep the live snapshot's kind/name so
    // a stale persisted record cannot revert the tool's classification.
    kind: base.kind || detail.kind,
    name: base.name || detail.name,
    summary: base.summary || detail.summary,
    raw_input: detail.raw_input ?? base.raw_input,
    raw_output: detail.raw_output ?? base.raw_output,
    error: detail.error ?? base.error,
    diff_paths:
      detail.diff_paths.length > 0 ? detail.diff_paths : base.diff_paths,
    diff_previews:
      detail.diff_previews.length > 0 ? detail.diff_previews : base.diff_previews,
  };
}

export function clearToolDetailCacheForTests(): void {
  toolDetailCache.clear();
  toolDetailInflight.clear();
}

export { previewToCompactPatch } from "./compact-patch";

// ── Per-version render model ────────────────────────────────────────────────
//
// Everything the card renders is derived from the tool object. The expanded
// card merges the UNCAPPED stored detail, so these derivations are potential
// multi-megabyte string scans (regex classification, JSON.parse of raw_input,
// line splitting) — running them on EVERY render (i.e. on every streaming
// patch) froze the UI on tool-heavy sessions. Tool objects are immutable per
// version, so the whole chain is computed once per version into a cached
// model; every re-render is a field read.
interface ToolRenderModel {
  presentation: ToolPresentation;
  commandEditPaths: string[];
  trackedDiffPaths: string[];
  trackedDiffPreviews: ToolDiffPreview[];
  diffPreviews: ToolDiffPreview[];
  hasReviewableDiff: boolean;
  effectiveCategory: ToolCategory;
  verb: string;
  /// `null` for rows that need no marker (a call that finished successfully).
  bullet: StatusBullet | null;
  outputLines: { lines: string[]; omitted: number };
  detailLines: { lines: string[]; omitted: number };
  logEntries: { entries: ToolInvocation["logs"]; omitted: number };
  rawOutputLines: { lines: string[]; omitted: number };
  errorLine: string | null;
  diffStats: { added: number; removed: number };
  showEditingDiffOnly: boolean;
  cmdDetail: string | null;
  headerTitle: string;
  explorationResult: ReturnType<typeof getExplorationResult> | null;
  shellPresentation: ToolPresentation | null;
  needsPermission: boolean;
  hasDetail: boolean;
  /// 生图/改图工具身份；`null` 表示普通工具。
  imageGeneration: ImageGenerationMode | null;
  /// 生图/改图结果里的图片引用（原始形态，显示 URL 在渲染层转换）。
  imageRefs: string[];
}

const toolRenderModelCache = new WeakMap<ToolInvocation, ToolRenderModel>();

function toolRenderModel(tool: ToolInvocation): ToolRenderModel {
  const cached = toolRenderModelCache.get(tool);
  if (cached) return cached;
  const model = computeToolRenderModel(tool);
  toolRenderModelCache.set(tool, model);
  return model;
}

function computeToolRenderModel(tool: ToolInvocation): ToolRenderModel {
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
  const outputLines = getOutputLines(tool);
  const detailLines = getDetailLines(tool);
  const logEntries = getVisibleLogEntries(tool);
  const errorLine =
    tool.error && !isVagueError(tool.error) ? tool.error : null;
  const diffStats = getDiffStats(diffPreviews);
  // Completed "edit" tools with no reviewable patch should not claim 已编辑.
  // Keep 编辑中 while running; only the finished verb falls back to 已运行.
  const hasReviewableDiff =
    diffPreviews.length > 0 && (diffStats.added > 0 || diffStats.removed > 0);
  const showAsExecutedWithoutDiff =
    category === "editing" &&
    !hasReviewableDiff &&
    (tool.status === "Succeeded" ||
      tool.status === "Failed" ||
      tool.status === "Interrupted");
  const verbCategory: ToolCategory = showAsExecutedWithoutDiff
    ? "executing"
    : category;
  // 生图/改图工具用专属动词（已生图 / 已改图 / 生图中…），并在卡片下方
  // 直接展示生成结果的大图预览。
  const imageGeneration = imageGenerationMode(tool);
  const imageRefs = imageGeneration ? imageGenerationImages(tool) : [];
  const verb = imageGeneration
    ? imageGenerationVerb(tool.status, imageGeneration)
    : toolVerb(tool.status, verbCategory);
  // When we demote a finished edit to "已运行", also leave the edit-only
  // expand path so the shell/run panel can show the actual command.
  const effectiveCategory: ToolCategory = showAsExecutedWithoutDiff
    ? "executing"
    : category;
  const bullet = statusBullet(tool.status);
  const cmdDetail = extractCommandDetail(
    tool,
    effectiveCategory === "editing" ? trackedDiffPaths : [],
  );
  const headerTitle =
    (imageGeneration ? imageGenerationPromptTitle(tool) : null) ??
    (effectiveCategory === "editing"
      ? extractHeaderTitle(tool, trackedDiffPaths)
      : showAsExecutedWithoutDiff
      ? executedEditHeaderTitle(tool, cmdDetail, trackedDiffPaths) ??
        extractHeaderTitle(tool, trackedDiffPaths)
      : presentation.presentationKind === "command"
      ? commandHeaderTitle(presentation.command, effectiveCategory, tool)
      : extractHeaderTitle(tool, trackedDiffPaths));

  // raw_output as expandable content (for non-terminal tools like Read, Search, etc.)
  const rawOutputLines = getRawOutputLines(tool);
  const explorationResult =
    presentation.presentationKind !== "command" && effectiveCategory === "exploring"
      ? getExplorationResult(tool, cmdDetail, detailLines.lines, outputLines.lines, rawOutputLines.lines)
      : null;
  const shellPresentation =
    presentation.presentationKind === "command" && effectiveCategory !== "editing"
      ? presentation
      : effectiveCategory === "exploring"
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
        : showAsExecutedWithoutDiff
          ? deriveExecutedEditShellPresentation(
              tool,
              presentation,
              cmdDetail,
              trackedDiffPaths,
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

  // Editing cards with a real diff should expand to the patch only.
  // Extra path/output/log noise makes the review view hard to scan.
  const showEditingDiffOnly = effectiveCategory === "editing" && hasReviewableDiff;

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

  return {
    presentation,
    commandEditPaths,
    trackedDiffPaths,
    trackedDiffPreviews,
    diffPreviews,
    hasReviewableDiff,
    effectiveCategory,
    verb,
    bullet,
    outputLines,
    detailLines,
    logEntries,
    rawOutputLines,
    errorLine,
    diffStats,
    showEditingDiffOnly,
    cmdDetail,
    headerTitle,
    explorationResult,
    shellPresentation,
    needsPermission,
    hasDetail,
    imageGeneration,
    imageRefs,
  };
}

interface Props {
  tool: ToolInvocation;
  childToolsByParent?: Map<string, ToolInvocation[]>;
  nested: boolean;
  onPermissionSelect: (requestId: string, optionId: string | null, guidance?: string | null) => void;
  hiddenPermissionRequestIds?: ReadonlySet<string>;
  onCancelTurn?: () => Promise<void> | void;
  onStopTool?: (toolCallId: string) => Promise<void> | void;
}

/// 把生图结果的原始引用（data URL / `file://` / `asset://` / 本地路径）转换
/// 为 webview 可渲染的显示 URL。Windows 路径统一为**原生反斜杠形态**再走
/// `convertFileSrc`：正斜杠形态解析出的 asset URL 在本机 webview 里取不到
/// 文件（坏图），与 companion 预览等既有可用路径的形态保持一致。
function resolveImageDisplaySrc(ref: string): string {
  if (/^(data:image\/|asset:\/\/)/i.test(ref)) return ref;
  let path = ref;
  if (path.startsWith("file://")) {
    path = path.slice("file://".length);
    // file:///C:/x → C:/x（POSIX 的 file:///home/x 保留为 /home/x）
    if (path.startsWith("/") && /^[A-Za-z]:/.test(path.slice(1))) {
      path = path.slice(1);
    }
  }
  if (/^[A-Za-z]:[\\/]/.test(path)) {
    path = path.replace(/\//g, "\\");
  }
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
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
  const [previewImage, setPreviewImage] = useState<string | null>(null);
  const [detailedTool, setDetailedTool] = useState<ToolInvocation | null>(
    () => toolDetailCache.get(tool.id) ?? null,
  );

  // Lazy-load the uncapped stored detail on first expand when the snapshot's
  // copy looks truncated. Terminal tools stream their full output already.
  useEffect(() => {
    if (!expanded || detailedTool || tool.terminal_output) return;
    if (!toolDetailLooksCapped(tool)) return;
    const cached = toolDetailCache.get(tool.id);
    if (cached) {
      setDetailedTool(cached);
      return;
    }
    let cancelled = false;
    const inflight =
      toolDetailInflight.get(tool.id) ??
      sessionGetToolDetail(tool.id)
        .then((detail) => {
          toolDetailCache.set(tool.id, detail);
        })
        .catch(() => {
          // Keep the capped snapshot copy; the next expand retries.
        })
        .finally(() => {
          toolDetailInflight.delete(tool.id);
        });
    toolDetailInflight.set(tool.id, inflight);
    void inflight.then(() => {
      if (cancelled) return;
      const detail = toolDetailCache.get(tool.id);
      if (detail) setDetailedTool(detail);
    });
    return () => {
      cancelled = true;
    };
  }, [expanded, detailedTool, tool]);

  const children = childToolsByParent?.get(tool.call_id) ?? [];

  const [childrenCollapsed, setChildrenCollapsed] = useState(true);

  if (hiddenPermissionRequest) {
    return null;
  }

  // Prefer the lazily fetched full detail for rendering; fall back to the
  // (possibly capped) snapshot copy while the fetch is in flight.
  tool = detailedTool ? mergeToolDetail(tool, detailedTool) : tool;

  const {
    presentation,
    trackedDiffPaths,
    diffPreviews,
    effectiveCategory,
    verb,
    bullet,
    outputLines,
    detailLines,
    logEntries,
    rawOutputLines,
    errorLine,
    diffStats,
    hasReviewableDiff,
    showEditingDiffOnly,
    cmdDetail,
    headerTitle,
    shellPresentation,
    needsPermission,
    hasDetail,
    imageRefs,
  } = toolRenderModel(tool);
  const imagePreviews = imageRefs.map(resolveImageDisplaySrc);
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

  return (
    <div className={`tc ${nested ? "tc-nested" : ""}`}>
      {/* Header line: optional status bullet + verb + title + expand chevron */}
      <div className="tc-line-wrap">
        <button
          type="button"
          className={`tc-line tc-header-line ${hasDetail ? "tc-expandable" : ""}`}
          onClick={hasDetail ? () => setExpanded((v) => !v) : undefined}
          aria-expanded={hasDetail ? expanded : undefined}
          disabled={!hasDetail}
        >
          {bullet && (
            <span className={`tc-bullet ${bullet.className}`} aria-hidden="true">
              {bullet.char}
            </span>
          )}
          <span className="tc-verb">{verb}</span>
          <span className="tc-cmd">{headerTitle}</span>
          {effectiveCategory === "editing" && hasReviewableDiff && (
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

      {/* 生图/改图结果大图预览：紧跟卡片，不折叠进展开区 */}
      {imagePreviews.length > 0 && (
        <div className="tc-image-previews">
          {imagePreviews.map((src, index) => (
            <button
              key={imageRefs[index] ?? src}
              type="button"
              className="tc-image-preview"
              onClick={() => setPreviewImage(src)}
              aria-label={`预览 生成的图片 ${index + 1}`}
            >
              <img
                src={src}
                alt={`生成的图片 ${index + 1}`}
                onError={(event) => {
                  // 转换后的 URL 取不到文件时回退原始引用（file:// / 原生
                  // 路径），绝不留坏图占位。
                  const img = event.currentTarget;
                  const fallback = imageRefs[index];
                  if (!fallback || img.dataset.fallbackApplied === "1") return;
                  img.dataset.fallbackApplied = "1";
                  img.src = fallback;
                }}
              />
            </button>
          ))}
        </div>
      )}
      {previewImage && (
        <div
          className="tc-image-lightbox"
          role="dialog"
          aria-label="图片预览"
          onClick={() => setPreviewImage(null)}
        >
          <img src={previewImage} alt="生成的图片" />
        </div>
      )}

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
                (presentation.presentationKind !== "command" || effectiveCategory === "editing") &&
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
  const appTheme = useCurrentAppTheme();
  const diffOptions = useMemo(() => toolDiffOptions(appTheme), [appTheme]);

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
          options={diffOptions}
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

function deriveExecutedEditShellPresentation(
  tool: ToolInvocation,
  presentation: ToolPresentation,
  cmdDetail: string | null,
  trackedDiffPaths: string[],
  errorLine: string | null,
  logEntries: Array<{ title: string; body: string }>,
  detailLines: string[],
  outputLines: string[],
  rawOutputLines: string[],
): ToolPresentation {
  const command =
    presentation.command ??
    executedEditInvocationSummary(tool, cmdDetail, trackedDiffPaths);

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
    uniqueStrings([
      ...trackedDiffPaths,
      ...detailLines,
      ...logLines,
      ...outputLines,
      ...rawOutputLines,
    ]).join("\n"),
    errorLine,
  ].filter((part): part is string => !!part && part.trim().length > 0);

  return {
    ...presentation,
    presentationKind: "command",
    toolLabel: executedEditToolLabel(tool, presentation),
    command,
    primaryOutput:
      outputParts.length > 0 ? outputParts.join("\n\n") : presentation.primaryOutput,
  };
}

function executedEditHeaderTitle(
  tool: ToolInvocation,
  cmdDetail: string | null,
  trackedDiffPaths: string[],
): string | null {
  // Keep the collapsed header path-focused; the expand panel shows the
  // actual invocation (Edit/Write/command) so the card is less mysterious.
  if (trackedDiffPaths.length > 0) {
    return displayPath(trackedDiffPaths[trackedDiffPaths.length - 1]);
  }
  if (cmdDetail && looksLikePathOnlyTitle(cmdDetail)) {
    return displayPath(cmdDetail);
  }
  if (
    tool.summary?.trim() &&
    !isGenericToolName(tool.summary) &&
    looksLikePathOnlyTitle(tool.summary)
  ) {
    return displayPath(tool.summary.trim());
  }
  return extractHeaderTitle(tool, trackedDiffPaths);
}

function executedEditInvocationSummary(
  tool: ToolInvocation,
  cmdDetail: string | null,
  trackedDiffPaths: string[],
): string | null {
  if (cmdDetail && !looksLikePathOnlyTitle(cmdDetail)) {
    return cmdDetail;
  }

  const path =
    trackedDiffPaths[trackedDiffPaths.length - 1] ??
    (cmdDetail && looksLikePathOnlyTitle(cmdDetail) ? cmdDetail : null) ??
    extractHeaderTitle(tool, trackedDiffPaths);

  const toolName = tool.name?.trim();
  if (toolName && !isGenericToolName(toolName)) {
    return path ? `${toolName} ${path}` : toolName;
  }

  const kind = tool.kind?.trim();
  if (kind && !isGenericToolName(kind)) {
    return path ? `${kind} ${path}` : kind;
  }

  return path;
}

function executedEditToolLabel(
  tool: ToolInvocation,
  presentation: ToolPresentation,
): string {
  const toolName = tool.name?.trim();
  if (toolName && !isGenericToolName(toolName)) return toolName;
  const kind = tool.kind?.trim();
  if (kind && !isGenericToolName(kind)) return kind;
  if (presentation.toolLabel && !isGenericToolName(presentation.toolLabel)) {
    return presentation.toolLabel;
  }
  return "Tool";
}

function isGenericToolName(value: string): boolean {
  const normalized = value.trim().toLowerCase();
  return (
    !normalized ||
    normalized === "{" ||
    normalized === "tool" ||
    normalized === "execute" ||
    normalized === "command" ||
    normalized === "generic"
  );
}

function looksLikePathOnlyTitle(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed) return false;
  if (/\s/.test(trimmed) && !trimmed.includes("/") && !trimmed.includes("\\")) {
    return false;
  }
  return (
    trimmed.startsWith("/") ||
    trimmed.startsWith("./") ||
    trimmed.startsWith("../") ||
    /^[A-Za-z]:[\\/]/.test(trimmed) ||
    trimmed.includes("/") ||
    trimmed.includes("\\")
  );
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
