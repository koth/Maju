import { memo, useState, useMemo, useCallback, useEffect, useRef } from "react";
import type { Dispatch, ReactNode, SetStateAction } from "react";
import { MultiFileDiff } from "@pierre/diffs/react";
import type { FileContents } from "@pierre/diffs/react";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { UiSnapshot, ChangedFile, ChangeSection, DiffStats, FileEntry, ChangeSetSummary, FileChangeSummary, FileChangeRecord, DiffQuality, AppTheme } from "../../types";
import { fsListDir, gitStage, sessionListChangeSets, sessionListChangeSetFiles, sessionGetChangeSetFileDiff } from "../../lib/tauri";
import { DiffTab } from "../editor/DiffTab";
import { FileTree } from "../filetree/FileTree";
import { getFileIcon } from "../filetree/file-icons";
import { useHorizontalScrollControls } from "../../lib/use-horizontal-scroll-controls";
import "./ReviewPanel.css";

export type ReviewPanelTab = "Review" | "Diff" | "Files";
export type ReviewPanelActiveTab =
  | { kind: "base"; tab: ReviewPanelTab }
  | { kind: "file"; path: string }
  | { kind: "diff"; path: string; changeSetId: string };
export type ReviewPanelOpenTab =
  | { kind: "file"; path: string }
  | { kind: "diff"; path: string; changeSetId: string };
type ReviewScope = "last" | "manual";

export interface InlineDiffLine {
  id: string;
  kind: "context" | "added" | "removed" | "gap";
  direction?: "up" | "down" | "middle";
  gapKey?: string;
  oldLine: number | null;
  newLine: number | null;
  text: string;
}

interface TreeNode {
  name: string;
  path: string;
  kind: "file" | "directory";
  children: TreeNode[];
  stats: DiffStats;
  file?: ChangedFile;
}

const MAX_REVIEW_FILES = 300;
const REVIEW_INLINE_DIFF_AUTO_OPEN_FILE_LIMIT = 4;
const REVIEW_INLINE_DIFF_AUTO_OPEN_FILE_LINE_LIMIT = 320;
const REVIEW_INLINE_DIFF_AUTO_OPEN_TOTAL_LINE_LIMIT = 520;
const REVIEW_DIFF_MAX_MATRIX_CELLS = 250_000;
const REVIEW_DIFF_SCROLL_TARGET_SELECTOR = "[data-code], pre, [data-diff], [data-content]";
const REVIEW_DIFF_OPTIONS_BASE = {
  disableFileHeader: true,
  hunkSeparators: "line-info",
  collapsedContextThreshold: 6,
  expansionLineCount: 80,
  lineDiffType: "none",
  overflow: "scroll",
  unsafeCSS: `
    :host {
      --diffs-bg: var(--app-bg) !important;
      --diffs-dark-bg: var(--app-bg) !important;
      --diffs-light-bg: var(--app-bg) !important;
      --diffs-bg-context: var(--app-bg) !important;
      --diffs-bg-buffer: var(--app-bg) !important;
      --review-diff-scrollbar-thumb: color-mix(in srgb, var(--app-bg) 82%, var(--text-soft)) !important;
      --review-diff-scrollbar-thumb-active: color-mix(in srgb, var(--app-bg) 56%, var(--text-muted)) !important;
      --review-diff-scrollbar-thumb-hover: color-mix(in srgb, var(--app-bg) 34%, var(--text-muted)) !important;
      background-color: var(--app-bg) !important;
    }

    pre,
    code,
    [data-code],
    [data-diff],
    [data-file],
    [data-gutter],
    [data-content] {
      background-color: var(--diffs-bg) !important;
    }

    :where([data-background]) [data-line-type="context"],
    :where([data-background]) [data-line-type="context-expanded"],
    :where([data-background]) [data-gutter-buffer],
    :where([data-background]) [data-column-number]:not([data-line-type="change-addition"]):not([data-line-type="change-deletion"]),
    :where([data-background]) [data-line]:not([data-line-type="change-addition"]):not([data-line-type="change-deletion"]),
    :where([data-background]) [data-no-newline]:not([data-line-type="change-addition"]):not([data-line-type="change-deletion"]) {
      --diffs-computed-decoration-bg: var(--diffs-bg) !important;
      --diffs-computed-diff-line-bg: var(--diffs-bg) !important;
      --diffs-computed-selected-line-bg: var(--diffs-bg) !important;
      --diffs-line-bg: var(--diffs-bg) !important;
      background-color: var(--diffs-bg) !important;
    }

    [data-line-type="context"],
    [data-line-type="context-expanded"],
    [data-line-annotation],
    [data-gutter-buffer="annotation"] {
      --diffs-line-bg: var(--diffs-bg) !important;
      background-color: var(--diffs-bg) !important;
    }

    [data-content-buffer],
    [data-gutter-buffer="buffer"] {
      --diffs-line-bg: var(--diffs-bg) !important;
      background-color: var(--diffs-bg) !important;
      background-image: none !important;
    }

    [data-overflow="scroll"] [data-code] {
      overflow-x: auto !important;
      overflow-y: clip !important;
      scrollbar-color: var(--review-diff-scrollbar-thumb) transparent !important;
      scrollbar-width: thin !important;
    }

    :host(:hover) [data-overflow="scroll"] [data-code] {
      scrollbar-color: var(--review-diff-scrollbar-thumb-active) transparent !important;
    }

    [data-overflow="scroll"] [data-code]::-webkit-scrollbar {
      width: 0 !important;
      height: 9px !important;
    }

    [data-overflow="scroll"] [data-code]::-webkit-scrollbar-track {
      background: transparent !important;
    }

    [data-overflow="scroll"] [data-code]::-webkit-scrollbar-thumb {
      min-width: 36px !important;
      border: 2px solid transparent !important;
      border-radius: 999px !important;
      background-color: var(--review-diff-scrollbar-thumb) !important;
      background-clip: content-box !important;
    }

    :host(:hover) [data-overflow="scroll"] [data-code]::-webkit-scrollbar-thumb {
      background-color: var(--review-diff-scrollbar-thumb-active) !important;
    }

    [data-overflow="scroll"] [data-code]::-webkit-scrollbar-thumb:hover {
      background-color: var(--review-diff-scrollbar-thumb-hover) !important;
    }
  `,
} as const;

function buildFileTree(files: ChangedFile[]): TreeNode[] {
  interface DirEntry {
    node: TreeNode;
    children: Map<string, DirEntry>;
  }

  const root = new Map<string, DirEntry>();

  for (const file of files) {
    const segments = file.path.replace(/\\/g, "/").split("/");
    let currentMap = root;

    for (let i = 0; i < segments.length; i++) {
      const seg = segments[i];
      const isLeaf = i === segments.length - 1;
      const nodePath = segments.slice(0, i + 1).join("/");

      if (isLeaf) {
        currentMap.set(seg, {
          node: {
            name: seg,
            path: file.path,
            kind: "file",
            children: [],
            stats: file.stats,
            file,
          },
          children: new Map(),
        });
      } else {
        if (!currentMap.has(seg)) {
          currentMap.set(seg, {
            node: {
              name: seg,
              path: nodePath,
              kind: "directory",
              children: [],
              stats: { added: 0, removed: 0 },
            },
            children: new Map(),
          });
        }
        currentMap = currentMap.get(seg)!.children;
      }
    }
  }

  function flatten(map: Map<string, DirEntry>): TreeNode[] {
    const result: TreeNode[] = [];
    for (const [, entry] of map) {
      if (entry.node.kind === "directory") {
        entry.node.children = flatten(entry.children);
        entry.node.stats = entry.node.children.reduce(
          (acc, child) => ({
            added: acc.added + child.stats.added,
            removed: acc.removed + child.stats.removed,
          }),
          { added: 0, removed: 0 },
        );
      }
      result.push(entry.node);
    }
    result.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "directory" ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    return result;
  }

  return flatten(root);
}

interface Props {
  snapshot: UiSnapshot;
  refreshing: boolean;
  hydrated: boolean;
  appTheme?: AppTheme;
  panelExpanded?: boolean;
  onRefresh: () => void | Promise<void>;
  onFileSelect: (path: string, changeSetId: string) => void;
  onFileOpen: (path: string) => void;
  onAddComposerReference?: (path: string) => void;
  onPanelExpandedChange?: (expanded: boolean) => void;
  onEditorFileTreeVisibleChange?: (visible: boolean) => void;
  renderFileTab?: (
    path: string,
    context: { fileTreeVisible: boolean; onToggleFileTree?: () => void; onUserInteraction?: () => void },
  ) => ReactNode;
  activeTab?: ReviewPanelActiveTab;
  openTabs?: ReviewPanelOpenTab[];
  onActiveTabChange?: Dispatch<SetStateAction<ReviewPanelActiveTab>>;
  onOpenTabsChange?: Dispatch<SetStateAction<ReviewPanelOpenTab[]>>;
  focusRequest?: { changeSetId: string; token: number } | null;
}

export function ReviewPanel({
  snapshot,
  refreshing,
  hydrated,
  appTheme = "graphite",
  panelExpanded = false,
  onRefresh,
  onFileOpen,
  onAddComposerReference,
  onPanelExpandedChange,
  onEditorFileTreeVisibleChange,
  renderFileTab,
  activeTab: controlledActiveTab,
  openTabs: controlledOpenTabs,
  onActiveTabChange,
  onOpenTabsChange,
  focusRequest,
}: Props) {
  const [internalActiveTab, setInternalActiveTab] = useState<ReviewPanelActiveTab>({
    kind: "base",
    tab: "Review",
  });
  const [internalOpenTabs, setInternalOpenTabs] = useState<ReviewPanelOpenTab[]>([]);
  const [filter, setFilter] = useState("");
  const [fileTreeRefreshSignal, setFileTreeRefreshSignal] = useState(0);
  const [editorFileTreeVisible, setEditorFileTreeVisible] = useState(false);
  const [changeSetState, setChangeSetState] = useState<{
    summaries: ChangeSetSummary[];
    filesById: Record<string, FileChangeSummary[]>;
  }>({ summaries: [], filesById: {} });
  const activeTab = controlledActiveTab ?? internalActiveTab;
  const setActiveTab = onActiveTabChange ?? setInternalActiveTab;
  const openTabs = controlledOpenTabs ?? internalOpenTabs;
  const setOpenTabs = onOpenTabsChange ?? setInternalOpenTabs;
  const resetKeyRef = useRef(`${snapshot.session.id}:${snapshot.workspace.root}`);
  const panelRef = useRef<HTMLDivElement>(null);
  const focusRequestKey = focusRequest
    ? `${focusRequest.changeSetId}:${focusRequest.token}`
    : null;
  const handledFocusRequestKeyRef = useRef<string | null>(focusRequestKey);
  const activeBaseTab = activeTab.kind === "base" ? activeTab.tab : null;
  const activeFilePath = activeTab.kind === "file" ? activeTab.path : null;
  const activeDiffTab = activeTab.kind === "diff" ? activeTab : null;
  const activeSideTreeKeyRef = useRef<string | null>(null);
  const workspaceConnected = snapshot.workspace_connected !== false;
  const hasOpenFileTab = openTabs.some((tab) => tab.kind === "file");
  const hasOpenDiffTab = openTabs.some((tab) => tab.kind === "diff");
  const activeSideTreeKey = activeFilePath
    ? `file:${activeFilePath}`
    : activeDiffTab
      ? `diff:${activeDiffTab.changeSetId}:${activeDiffTab.path}`
      : null;

  // Track which file tabs have been "pinned" by user interaction
  // (scroll, click, type).  Unpinned file tabs are replaced when
  // a new file is opened from the tree.
  const pinnedFilePathsRef = useRef<Set<string>>(new Set());
  const handleFileInteraction = useCallback((path: string) => {
    pinnedFilePathsRef.current.add(path);
  }, []);

  const handleRefresh = useCallback(() => {
    if (!workspaceConnected) return;
    if (activeBaseTab === "Diff") {
      onRefresh();
      return;
    }

    setFileTreeRefreshSignal((signal) => signal + 1);
  }, [activeBaseTab, onRefresh, workspaceConnected]);

  const handlePanelExpandedToggle = useCallback(() => {
    onPanelExpandedChange?.(!panelExpanded);
  }, [onPanelExpandedChange, panelExpanded]);

  const handleEditorFileTreeToggle = useCallback(() => {
    setEditorFileTreeVisible((visible) => !visible);
  }, []);

  const handleOpenReviewTab = useCallback((tab: ReviewPanelOpenTab) => {
    const tabId = reviewOpenTabId(tab);
    setOpenTabs((current) => {
      if (current.some((openTab) => reviewOpenTabId(openTab) === tabId)) {
        return current;
      }
      // Ephemeral file tabs: when opening a new file, close all existing
      // unpinned file tabs so rapid browsing doesn't stack.
      if (tab.kind === "file") {
        return [...current.filter((t) => t.kind !== "file" || pinnedFilePathsRef.current.has(t.path)), tab];
      }
      return [...current, tab];
    });
    setActiveTab(tab);
  }, [setActiveTab, setOpenTabs]);

  const handleFileTreeOpen = useCallback((path: string) => {
    if (!renderFileTab) {
      onFileOpen(path);
      return;
    }

    handleOpenReviewTab({ kind: "file", path });
  }, [handleOpenReviewTab, onFileOpen, renderFileTab]);

  const handleGitFileOpen = useCallback((path: string, changeSetId: string) => {
    handleOpenReviewTab({ kind: "diff", path, changeSetId });
  }, [handleOpenReviewTab]);

  const handleOpenTabClose = useCallback((tab: ReviewPanelOpenTab) => {
    const closingTabId = reviewOpenTabId(tab);
    const remainingTabs = openTabs.filter((openTab) => reviewOpenTabId(openTab) !== closingTabId);
    setOpenTabs(remainingTabs);
    setActiveTab((current) =>
      reviewActiveTabMatchesOpenTab(current, tab)
        ? remainingTabs[remainingTabs.length - 1] ?? { kind: "base", tab: tab.kind === "diff" ? "Diff" : "Files" }
        : current,
    );
  }, [openTabs, setActiveTab, setOpenTabs]);

  const filteredFiles = useMemo(() => {
    const lowerFilter = filter.toLowerCase();
    return snapshot.repository.changed_files.filter(
      (f) => !filter || f.path.toLowerCase().includes(lowerFilter)
    );
  }, [snapshot.repository.changed_files, filter]);

  const visibleFiles = useMemo(
    () => filteredFiles.slice(0, MAX_REVIEW_FILES),
    [filteredFiles],
  );
  const inActiveTurn =
    snapshot.session.status === "Streaming" || snapshot.session.status === "WaitingForTool";
  const activeTurnOwner = useMemo(
    () => activeTurnOwnerKey(snapshot, inActiveTurn),
    [inActiveTurn, snapshot.messages, snapshot.timeline],
  );
  const reviewTargetAssistantMessageId = useMemo(
    () => lastReviewableAssistantMessageId(snapshot.messages, snapshot.timeline, inActiveTurn),
    [inActiveTurn, snapshot.messages, snapshot.timeline],
  );
  const messageOrder = useMemo(
    () => timelineMessageOrder(snapshot.messages, snapshot.timeline),
    [snapshot.messages, snapshot.timeline],
  );

  useEffect(() => {
    if (!focusRequestKey) return;
    if (handledFocusRequestKeyRef.current === focusRequestKey) return;

    handledFocusRequestKeyRef.current = focusRequestKey;
    setActiveTab({ kind: "base", tab: "Review" });
  }, [focusRequestKey, setActiveTab]);

  useEffect(() => {
    const nextResetKey = `${snapshot.session.id}:${snapshot.workspace.root}`;
    if (resetKeyRef.current === nextResetKey) return;

    resetKeyRef.current = nextResetKey;
    setOpenTabs([]);
    setActiveTab({ kind: "base", tab: "Review" });
    setEditorFileTreeVisible(false);
  }, [setActiveTab, setOpenTabs, snapshot.session.id, snapshot.workspace.root]);

  useEffect(() => {
    if (!activeSideTreeKey) {
      activeSideTreeKeyRef.current = null;
      setEditorFileTreeVisible(false);
      return;
    }
    if (activeSideTreeKeyRef.current === activeSideTreeKey) return;
    activeSideTreeKeyRef.current = activeSideTreeKey;
    setEditorFileTreeVisible(true);
  }, [activeSideTreeKey]);

  useEffect(() => {
    onEditorFileTreeVisibleChange?.(Boolean(activeSideTreeKey && editorFileTreeVisible));
  }, [activeSideTreeKey, editorFileTreeVisible, onEditorFileTreeVisibleChange]);

  const grouped = useMemo(() => {
    const groups: Record<ChangeSection, ChangedFile[]> = {
      Unstaged: [],
      Staged: [],
      Untracked: [],
    };
    visibleFiles.forEach((f) => groups[f.section].push(f));
    return groups;
  }, [visibleFiles]);

  useEffect(() => {
    if (!workspaceConnected || !hydrated) {
      setChangeSetState({ summaries: [], filesById: {} });
      return;
    }

    let cancelled = false;
    sessionListChangeSets({
      session_id: snapshot.session.id,
      workspace_root: snapshot.workspace.root,
    })
      .then(async (summaries) => {
        const relevant = summaries.filter((summary) =>
          summary.source === "AgentTurn" || summary.source === "ManualEdit"
        );
        const fileEntries = await Promise.all(
          relevant.map(async (summary) => {
            try {
              const response = await sessionListChangeSetFiles({
                change_set_id: summary.id,
              });
              return [summary.id, response.files] as const;
            } catch {
              return [summary.id, []] as const;
            }
          }),
        );
        if (cancelled) return;
        setChangeSetState({
          summaries: relevant,
          filesById: Object.fromEntries(fileEntries),
        });
      })
      .catch(() => {
        if (!cancelled) setChangeSetState({ summaries: [], filesById: {} });
      });

    return () => {
      cancelled = true;
    };
  }, [focusRequestKey, hydrated, snapshot.session.id, snapshot.workspace.root, snapshot.revision, workspaceConnected]);

  return (
    <div ref={panelRef} className="review-panel">
      <div className="review-tabs">
        <button
          type="button"
          className={`review-tab ${activeBaseTab === "Review" ? "review-tab-active" : ""}`}
          onClick={() => setActiveTab({ kind: "base", tab: "Review" })}
          title="审查"
          aria-label="审查"
        >
          <ReviewTabIcon />
          <span className="review-tab-label">审查</span>
        </button>
        {!hasOpenDiffTab && (
          <button
            type="button"
            className={`review-tab ${activeBaseTab === "Diff" ? "review-tab-active" : ""}`}
            onClick={() => setActiveTab({ kind: "base", tab: "Diff" })}
            title="Git"
            aria-label="Git"
          >
            <GitTabIcon />
            <span className="review-tab-label">Git</span>
          </button>
        )}
        {!hasOpenFileTab && (
          <button
            type="button"
            className={`review-tab ${activeBaseTab === "Files" ? "review-tab-active" : ""}`}
            onClick={() => setActiveTab({ kind: "base", tab: "Files" })}
            title="所有文件"
            aria-label="所有文件"
          >
            <FolderTreeIcon className="review-tab-icon" />
            <span className="review-tab-label">文件</span>
          </button>
        )}
        {openTabs.map((tab) => {
          const tabId = reviewOpenTabId(tab);
          const fileName = fileNameFromPath(tab.path);
          const isActive = reviewActiveTabMatchesOpenTab(activeTab, tab);
          return (
            <div
              key={tabId}
              className={`review-open-file-tab ${isActive ? "review-tab-active" : ""}`}
              title={tab.kind === "diff" ? `差异：${tab.path}` : tab.path}
            >
              <button
                type="button"
                className="review-file-tab-close"
                onClick={() => handleOpenTabClose(tab)}
                aria-label={`关闭 ${fileName}`}
                title={`关闭 ${fileName}`}
              >
                <img className="review-file-tab-icon" src={getFileIcon(tab.path)} alt="" />
                <span className="review-file-tab-x" aria-hidden="true">×</span>
              </button>
              <button
                type="button"
                className="review-file-tab-label-btn"
                onClick={() => setActiveTab(tab)}
                aria-label={tab.kind === "diff" ? `打开差异 ${fileName}` : `打开文件 ${fileName}`}
                title={tab.kind === "diff" ? `差异：${tab.path}` : tab.path}
              >
                {fileName}
              </button>
            </div>
          );
        })}
        <div className="review-tabs-spacer" />
        {activeBaseTab !== null && activeBaseTab !== "Review" && (
          <button
            type="button"
            className="review-refresh-btn"
            onClick={handleRefresh}
            disabled={!workspaceConnected || (activeBaseTab === "Diff" && refreshing)}
            title={activeBaseTab === "Diff" ? "刷新 Git 状态" : "刷新选中的文件目录"}
            aria-label={activeBaseTab === "Diff" ? "刷新 Git 状态" : "刷新选中的文件目录"}
          >
            <RefreshIcon />
          </button>
        )}
        {onPanelExpandedChange && (
          <button
            type="button"
            className={`review-panel-toggle-btn ${panelExpanded ? "is-active" : ""}`}
            onClick={handlePanelExpandedToggle}
            title={panelExpanded ? "还原审查面板" : "展开审查面板"}
            aria-label={panelExpanded ? "还原审查面板" : "展开审查面板"}
            aria-pressed={panelExpanded}
          >
            <PanelExpandIcon expanded={panelExpanded} />
          </button>
        )}
      </div>

      <div className="review-tab-panel review-tab-panel-review" hidden={activeBaseTab !== "Review"}>
        <ReviewChangesView
          changeSetState={changeSetState}
          lastAssistantMessageId={reviewTargetAssistantMessageId}
          activeTurnOwnerKey={activeTurnOwner}
          appTheme={appTheme}
          preferredChangeSetId={focusRequest?.changeSetId ?? null}
          preferredChangeSetToken={focusRequest?.token ?? null}
          messageOrder={messageOrder}
        />
      </div>

      <div className="review-tab-panel review-tab-panel-files" hidden={activeBaseTab !== "Files"}>
        {workspaceConnected ? (
          <FileTree
            workspaceRoot={snapshot.workspace.root}
            onFileOpen={handleFileTreeOpen}
            refreshSignal={fileTreeRefreshSignal}
            onAddComposerReference={onAddComposerReference}
            composerReferenceEnabled={snapshot.prompt_capabilities?.embedded_context === true}
          />
        ) : (
          <div className="review-empty-state">远程工作区未连接</div>
        )}
      </div>

      <div className="review-tab-panel review-tab-panel-git" hidden={activeBaseTab !== "Diff"}>
        {!hydrated ? (
          <div className="review-loading-state">
            <span className="review-loading-dot" />
            <span>正在加载 Git 变更...</span>
          </div>
        ) : (
        <>
          <div className="review-meta">
            {snapshot.repository.changed_files.length} 个变更文件
            {filteredFiles.length > MAX_REVIEW_FILES && (
              <span> / 显示前 {MAX_REVIEW_FILES} 个</span>
            )}
          </div>

          <div className="review-filter">
            <input
              type="text"
              className="review-filter-input"
              placeholder="过滤文件..."
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>

          <FileGroup
            title="未暂存"
            files={grouped.Unstaged}
            changeSetId="git-worktree:unstaged"
            onFileSelect={handleGitFileOpen}
          />
          <FileGroup
            title="已暂存"
            files={grouped.Staged}
            changeSetId="git-worktree:staged"
            onFileSelect={handleGitFileOpen}
          />
          <UntrackedTree
            files={grouped.Untracked}
            onFileSelect={handleGitFileOpen}
            onRefresh={onRefresh}
          />
        </>
        )}
      </div>
      {activeFilePath && renderFileTab && (
        <div className={`review-tab-panel review-tab-panel-editor review-tab-panel-file-editor ${editorFileTreeVisible && workspaceConnected ? "has-filetree" : ""}`}>
          <div className="review-editor-main">
            {renderFileTab(activeFilePath, {
              fileTreeVisible: editorFileTreeVisible,
              onToggleFileTree: handleEditorFileTreeToggle,
              onUserInteraction: () => handleFileInteraction(activeFilePath),
            })}
          </div>
          {editorFileTreeVisible && workspaceConnected && (
            <aside className="review-editor-filetree" aria-label="文件树">
              <FileTree
                workspaceRoot={snapshot.workspace.root}
                onFileOpen={handleFileTreeOpen}
                refreshSignal={fileTreeRefreshSignal}
                activePath={activeFilePath}
                variant="inline"
              />
            </aside>
          )}
        </div>
      )}
      {activeDiffTab && (
        <div className={`review-tab-panel review-tab-panel-editor review-tab-panel-diff-editor ${editorFileTreeVisible ? "has-filetree" : ""}`}>
          <div className="review-editor-main">
            <ReviewDiffTab
              path={activeDiffTab.path}
              changeSetId={activeDiffTab.changeSetId}
              appTheme={appTheme}
              workspaceName={snapshot.workspace.name}
              fileTreeVisible={editorFileTreeVisible}
              onToggleFileTree={handleEditorFileTreeToggle}
            />
          </div>
          {editorFileTreeVisible && (
            <aside className="review-editor-filetree review-git-editor-filetree" aria-label="Git 文件树">
              <GitChangesTree
                grouped={grouped}
                filter={filter}
                onFilterChange={setFilter}
                activePath={activeDiffTab.path}
                onFileSelect={handleGitFileOpen}
                onRefresh={onRefresh}
              />
            </aside>
          )}
        </div>
      )}
    </div>
  );
}

function reviewOpenTabId(tab: ReviewPanelOpenTab | Extract<ReviewPanelActiveTab, { kind: "file" | "diff" }>) {
  return tab.kind === "diff" ? `diff:${tab.changeSetId}:${tab.path}` : `file:${tab.path}`;
}

function reviewActiveTabMatchesOpenTab(activeTab: ReviewPanelActiveTab, openTab: ReviewPanelOpenTab) {
  if (activeTab.kind !== openTab.kind) return false;
  if (activeTab.path !== openTab.path) return false;
  if (activeTab.kind !== "diff") return true;
  return openTab.kind === "diff" && activeTab.changeSetId === openTab.changeSetId;
}

function fileNameFromPath(path: string) {
  const segments = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return segments[segments.length - 1] ?? path;
}

function ReviewDiffTab({
  path,
  changeSetId,
  appTheme,
  workspaceName,
  fileTreeVisible,
  onToggleFileTree,
}: {
  path: string;
  changeSetId: string;
  appTheme: AppTheme;
  workspaceName?: string;
  fileTreeVisible: boolean;
  onToggleFileTree: () => void;
}) {
  const [change, setChange] = useState<FileChangeRecord | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setChange(null);
    setFailed(false);
    sessionGetChangeSetFileDiff({ change_set_id: changeSetId, path })
      .then((nextChange) => {
        if (!cancelled) setChange(nextChange);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });

    return () => {
      cancelled = true;
    };
  }, [changeSetId, path]);

  return (
    <div className="review-tab-panel review-tab-panel-editor">
      {failed ? (
        <div className="review-loading-state">这个差异暂时无法加载</div>
      ) : change ? (
        <DiffTab
          change={change}
          appTheme={appTheme}
          toolbarMode="breadcrumbs"
          workspaceName={workspaceName}
          fileTreeVisible={fileTreeVisible}
          onToggleFileTree={onToggleFileTree}
        />
      ) : (
        <div className="review-loading-state">正在加载差异...</div>
      )}
    </div>
  );
}

function GitChangesTree({
  grouped,
  filter,
  onFilterChange,
  activePath,
  onFileSelect,
  onRefresh,
}: {
  grouped: Record<ChangeSection, ChangedFile[]>;
  filter: string;
  onFilterChange: (value: string) => void;
  activePath: string;
  onFileSelect: (path: string, changeSetId: string) => void;
  onRefresh: () => void | Promise<void>;
}) {
  return (
    <div className="review-git-filetree">
      <label className="review-git-filetree-search">
        <SearchIcon />
        <input
          className="review-git-filetree-search-input"
          value={filter}
          placeholder="筛选文件..."
          onChange={(event) => onFilterChange(event.target.value)}
        />
      </label>
      <div className="review-git-filetree-list">
        <FileGroup
          title="未暂存"
          files={grouped.Unstaged}
          changeSetId="git-worktree:unstaged"
          onFileSelect={onFileSelect}
          activePath={activePath}
          compact
        />
        <FileGroup
          title="已暂存"
          files={grouped.Staged}
          changeSetId="git-worktree:staged"
          onFileSelect={onFileSelect}
          activePath={activePath}
          compact
        />
        <UntrackedTree
          files={grouped.Untracked}
          onFileSelect={onFileSelect}
          onRefresh={onRefresh}
          activePath={activePath}
          compact
        />
      </div>
    </div>
  );
}

function ReviewTabIcon() {
  return (
    <svg className="review-tab-icon" viewBox="0 0 20 20" aria-hidden="true">
      <rect x="4" y="3.5" width="12" height="13" rx="2" />
      <path d="M7.2 7.2h5.6" />
      <path d="M10 10v4" />
      <path d="M8 12h4" />
    </svg>
  );
}

function PanelExpandIcon({ expanded }: { expanded: boolean }) {
  if (expanded) {
    return (
      <svg viewBox="0 0 20 20" aria-hidden="true">
        <path d="M8 4.5v3.5H4.5" />
        <path d="M12 15.5v-3.5h3.5" />
      </svg>
    );
  }

  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M12.5 5.5h3v3" />
      <path d="M7.5 14.5h-3v-3" />
    </svg>
  );
}

function ReviewChangesView({
  changeSetState,
  lastAssistantMessageId,
  activeTurnOwnerKey,
  appTheme,
  preferredChangeSetId,
  preferredChangeSetToken,
  messageOrder,
}: {
  changeSetState: {
    summaries: ChangeSetSummary[];
    filesById: Record<string, FileChangeSummary[]>;
  };
  lastAssistantMessageId: string | null;
  activeTurnOwnerKey: string | null;
  appTheme: AppTheme;
  preferredChangeSetId: string | null;
  preferredChangeSetToken: number | null;
  messageOrder: Map<string, number>;
}) {
  const [scope, setScope] = useState<ReviewScope>("last");
  const [scopeMenuOpen, setScopeMenuOpen] = useState(false);
  const changeSetSignature = useMemo(
    () => reviewChangeSetSignature(changeSetState.summaries, changeSetState.filesById),
    [changeSetState.filesById, changeSetState.summaries],
  );
  const [activePreferredChangeSet, setActivePreferredChangeSet] = useState<{
    id: string;
    token: number;
    consumedSignature: string | null;
  } | null>(null);

  useEffect(() => {
    if (preferredChangeSetId && preferredChangeSetToken !== null) {
      setScope("last");
      setScopeMenuOpen(false);
      setActivePreferredChangeSet({
        id: preferredChangeSetId,
        token: preferredChangeSetToken,
        consumedSignature: null,
      });
    }
  }, [preferredChangeSetId, preferredChangeSetToken]);

  const selectedChangeSet = useMemo(
    () =>
      selectReviewChangeSet(
        changeSetState.summaries,
        changeSetState.filesById,
        scope,
        lastAssistantMessageId,
        activeTurnOwnerKey,
        activePreferredChangeSet?.id ?? null,
        messageOrder,
      ),
    [
      activePreferredChangeSet?.id,
      changeSetState.filesById,
      changeSetState.summaries,
      activeTurnOwnerKey,
      lastAssistantMessageId,
      messageOrder,
      scope,
    ],
  );

  useEffect(() => {
    if (!activePreferredChangeSet) return;
    if (selectedChangeSet?.id !== activePreferredChangeSet.id) return;

    if (activePreferredChangeSet.consumedSignature === null) {
      setActivePreferredChangeSet({
        ...activePreferredChangeSet,
        consumedSignature: changeSetSignature,
      });
      return;
    }

    if (activePreferredChangeSet.consumedSignature !== changeSetSignature) {
      setActivePreferredChangeSet(null);
    }
  }, [activePreferredChangeSet, changeSetSignature, selectedChangeSet?.id]);

  const scopedFiles = selectedChangeSet
    ? changeSetState.filesById[selectedChangeSet.id] ?? []
    : [];
  const activeChangeSetId = selectedChangeSet?.id;
  const sorted = useMemo(
    () => [...scopedFiles].sort(compareReviewFiles),
    [scopedFiles],
  );
  const totals = useMemo(
    () =>
      sorted.reduce(
        (acc, change) => ({
          added: acc.added + change.added_lines,
          removed: acc.removed + change.removed_lines,
        }),
        { added: 0, removed: 0 },
      ),
    [sorted],
  );
  const autoExpandedPaths = useMemo(() => {
    const paths = new Set<string>();
    let openedFiles = 0;
    let openedLines = 0;
    for (const change of sorted) {
      if (openedFiles >= REVIEW_INLINE_DIFF_AUTO_OPEN_FILE_LIMIT) break;
      const changedLines = change.added_lines + change.removed_lines;
      if (changedLines > REVIEW_INLINE_DIFF_AUTO_OPEN_FILE_LINE_LIMIT) continue;
      if (openedLines + changedLines > REVIEW_INLINE_DIFF_AUTO_OPEN_TOTAL_LINE_LIMIT) continue;
      paths.add(change.path);
      openedFiles += 1;
      openedLines += changedLines;
    }
    return paths;
  }, [sorted]);

  return (
    <div className="review-session-changes">
      <div className="review-scope-row">
        <div className="review-scope-menu">
          <button
            type="button"
            className="review-scope-trigger"
            onClick={() => setScopeMenuOpen((open) => !open)}
            aria-haspopup="menu"
            aria-expanded={scopeMenuOpen}
          >
            {scope === "last" ? "上轮对话" : "手工修改"}
            <span className="review-scope-chevron">⌄</span>
          </button>
          {scopeMenuOpen && (
            <div className="review-scope-popover" role="menu">
              <button
                type="button"
                className={`review-scope-option ${scope === "last" ? "is-active" : ""}`}
                onClick={() => {
                  setScope("last");
                  setScopeMenuOpen(false);
                }}
                role="menuitem"
              >
                上轮对话
              </button>
              <button
                type="button"
                className={`review-scope-option ${scope === "manual" ? "is-active" : ""}`}
                onClick={() => {
                  setScope("manual");
                  setScopeMenuOpen(false);
                }}
                role="menuitem"
              >
                手工修改
              </button>
            </div>
          )}
        </div>
        <div className="review-scope-stats" aria-label="变更统计">
          <span className="review-stat-added">+{totals.added}</span>
          <span className="review-stat-removed">-{totals.removed}</span>
        </div>
      </div>

      {sorted.length === 0 ? (
        <div className="review-empty-state">
          {scope === "last" ? "上轮对话暂无文件变化" : "手工修改暂无文件变化"}
        </div>
      ) : (
        <div className="review-session-list">
          {sorted.map((change) => (
            <ReviewChangeCard
              key={`${activeChangeSetId ?? change.change_set_id}:${change.path}`}
              change={change}
              changeSetId={activeChangeSetId ?? change.change_set_id}
              appTheme={appTheme}
              initialCollapsed={!autoExpandedPaths.has(change.path)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function resolveReviewDiffHorizontalScrollTarget(root: HTMLDivElement) {
  const candidates = collectReviewDiffScrollTargets(root).filter(isHorizontallyScrollable);
  if (candidates.length === 0) return root;

  const activeElement = typeof document !== "undefined" ? document.activeElement : null;
  if (activeElement) {
    const activeCandidate = candidates.find((candidate) =>
      candidateContainsActiveElement(candidate, activeElement),
    );
    if (activeCandidate) return activeCandidate;
  }

  const hoveredCandidate = candidates.find(isHoveredElement);
  return hoveredCandidate ?? candidates[0] ?? root;
}

function collectReviewDiffScrollTargets(root: HTMLDivElement) {
  const targets: HTMLElement[] = [];
  const seenTargets = new Set<HTMLElement>();
  const seenScopes = new Set<Document | DocumentFragment | Element>();

  const addTarget = (target: HTMLElement) => {
    if (seenTargets.has(target)) return;
    seenTargets.add(target);
    targets.push(target);
  };

  const collectFromScope = (scope: Document | DocumentFragment | Element) => {
    if (seenScopes.has(scope)) return;
    seenScopes.add(scope);

    if (
      scope instanceof HTMLElement &&
      scope.matches(REVIEW_DIFF_SCROLL_TARGET_SELECTOR)
    ) {
      addTarget(scope);
    }

    for (const element of Array.from(
      scope.querySelectorAll<HTMLElement>(REVIEW_DIFF_SCROLL_TARGET_SELECTOR),
    )) {
      addTarget(element);
    }

    for (const element of Array.from(scope.querySelectorAll<HTMLElement>("*"))) {
      if (element.shadowRoot) collectFromScope(element.shadowRoot);
    }
  };

  collectFromScope(root);
  addTarget(root);
  return targets;
}

function isHorizontallyScrollable(element: HTMLElement) {
  return element.scrollWidth > element.clientWidth + 1;
}

function isHoveredElement(element: HTMLElement) {
  try {
    return element.matches(":hover");
  } catch {
    return false;
  }
}

function candidateContainsActiveElement(candidate: HTMLElement, activeElement: Element) {
  if (candidate === activeElement || candidate.contains(activeElement)) return true;

  const candidateRoot = candidate.getRootNode();
  return Boolean(
    typeof ShadowRoot !== "undefined" &&
      candidateRoot instanceof ShadowRoot &&
      candidateRoot.host === activeElement,
  );
}

const ReviewChangeCard = memo(function ReviewChangeCard({
  change,
  changeSetId,
  appTheme,
  initialCollapsed,
}: {
  change: FileChangeSummary;
  changeSetId: string;
  appTheme: AppTheme;
  initialCollapsed: boolean;
}) {
  const [hydratedChange, setHydratedChange] = useState<FileChangeRecord | null>(null);
  const [hydrationFailed, setHydrationFailed] = useState(false);
  const [collapsed, setCollapsed] = useState(initialCollapsed);
  const displayChange = hydratedChange ?? change;
  const needsHydration = useMemo(() => needsDiffHydration(change), [change]);
  const diffPreview = useMemo(
    () =>
      collapsed
        ? null
        : hydrationFailed
        ? { kind: "message" as const, text: "这个差异记录暂时无法加载" }
        : buildReviewDiff(displayChange),
    [collapsed, displayChange, hydrationFailed],
  );
  const diffOptions = useMemo(
    () => ({
      ...REVIEW_DIFF_OPTIONS_BASE,
      diffStyle: "unified",
      theme: appTheme === "light" ? "pierre-light" : "pierre-dark",
      themeType: appTheme === "light" ? "light" : "dark",
    } as const),
    [appTheme],
  );
  const horizontalScroll = useHorizontalScrollControls<HTMLDivElement>({
    resolveScrollTarget: resolveReviewDiffHorizontalScrollTarget,
  });

  useEffect(() => {
    setCollapsed(initialCollapsed);
    setHydratedChange(null);
    setHydrationFailed(false);
  }, [change.path, change.updated_at, changeSetId, initialCollapsed]);

  useEffect(() => {
    if (collapsed) return;
    if (!needsHydration) {
      setHydratedChange(null);
      setHydrationFailed(false);
      return;
    }

    let cancelled = false;
    setHydrationFailed(false);
    sessionGetChangeSetFileDiff({ change_set_id: changeSetId, path: change.path })
      .then((nextChange) => {
        if (!cancelled) setHydratedChange(nextChange);
      })
      .catch(() => {
        if (!cancelled) {
          setHydratedChange(null);
          setHydrationFailed(true);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [
    change.added_lines,
    collapsed,
    change.path,
    change.removed_lines,
    change.updated_at,
    changeSetId,
    needsHydration,
  ]);

  return (
    <article className={`review-change-card ${collapsed ? "is-collapsed" : ""}`}>
      <button
        type="button"
        className="review-change-header"
        onClick={() => setCollapsed((value) => !value)}
        aria-expanded={!collapsed}
        aria-label={change.path}
      >
        <span className="review-change-path">
          {change.path}
        </span>
        <span className="review-change-stats">
          <span className="review-stat-added">+{displayChange.added_lines}</span>
          <span className="review-stat-removed">-{displayChange.removed_lines}</span>
        </span>
        <span className="review-change-toggle" aria-hidden="true">
          {collapsed ? "⌄" : "⌃"}
        </span>
      </button>
      {!collapsed && (
        <div
          {...horizontalScroll.scrollControlProps}
          className="review-inline-diff"
          aria-label={`${change.path} 差异预览`}
        >
          {diffPreview?.kind === "patch" ? (
            <MultiFileDiff
              oldFile={diffPreview.oldFile}
              newFile={diffPreview.newFile}
              className="review-pierre-diff"
              options={diffOptions}
              disableWorkerPool
            />
          ) : (
            <div className="review-inline-message">{diffPreview?.text ?? "正在准备差异..."}</div>
          )}
        </div>
      )}
    </article>
  );
});

function selectReviewChangeSet(
  summaries: ChangeSetSummary[],
  filesById: Record<string, FileChangeSummary[]>,
  scope: ReviewScope,
  lastAssistantMessageId: string | null,
  activeTurnOwnerKey: string | null,
  preferredChangeSetId: string | null,
  messageOrder: Map<string, number>,
) {
  if (scope === "manual") {
    return summaries
      .filter((summary) => summary.source === "ManualEdit" && changeSetHasFiles(summary, filesById))
      .sort((a, b) => compareChangeSetSummaries(a, b, messageOrder))[0];
  }

  const agentTurns = summaries
    .filter((summary) => summary.source === "AgentTurn")
    .sort((a, b) => compareChangeSetSummaries(a, b, messageOrder));
  const pending = agentTurns.find(
    (summary) =>
      summary.status === "Pending" &&
      summary.owner_key === activeTurnOwnerKey &&
      changeSetHasFiles(summary, filesById),
  );
  if (pending) return pending;
  if (preferredChangeSetId) {
    const preferred = agentTurns.find(
      (summary) => summary.id === preferredChangeSetId && changeSetHasFiles(summary, filesById),
    );
    if (preferred) return preferred;
  }
  if (lastAssistantMessageId) {
    const latestReviewableTurn = agentTurns.find(
      (summary) =>
        summary.message_id === lastAssistantMessageId && changeSetHasFiles(summary, filesById),
    );
    if (latestReviewableTurn) return latestReviewableTurn;
  }
  return agentTurns.find((summary) => changeSetHasFiles(summary, filesById));
}

function compareChangeSetSummaries(
  a: ChangeSetSummary,
  b: ChangeSetSummary,
  messageOrder: Map<string, number>,
) {
  const updatedDelta = timestampValue(b.updated_at) - timestampValue(a.updated_at);
  if (updatedDelta !== 0) return updatedDelta;

  const orderA = a.message_id ? messageOrder.get(a.message_id) ?? -1 : -1;
  const orderB = b.message_id ? messageOrder.get(b.message_id) ?? -1 : -1;
  const orderDelta = orderB - orderA;
  if (orderDelta !== 0) return orderDelta;

  return b.id.localeCompare(a.id);
}

function compareReviewFiles(a: FileChangeSummary, b: FileChangeSummary) {
  const updatedDelta = timestampValue(b.updated_at) - timestampValue(a.updated_at);
  if (updatedDelta !== 0) return updatedDelta;
  return a.path.localeCompare(b.path);
}

function activeTurnOwnerKey(
  snapshot: UiSnapshot,
  inActiveTurn: boolean,
) {
  if (!inActiveTurn) return null;
  const messagesById = new Map(snapshot.messages.map((message) => [message.id, message]));
  for (let index = snapshot.timeline.length - 1; index >= 0; index -= 1) {
    const item = snapshot.timeline[index];
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (message?.role === "User") {
      return `user-message:${message.id}`;
    }
  }
  return null;
}

function reviewChangeSetSignature(
  summaries: ChangeSetSummary[],
  filesById: Record<string, FileChangeSummary[]>,
) {
  return summaries
    .map((summary) =>
      [
        summary.id,
        summary.source,
        summary.status,
        summary.updated_at,
        summary.file_count,
        ...((filesById[summary.id] ?? []).map((file) =>
          [
            file.path,
            file.change_type,
            file.added_lines,
            file.removed_lines,
            file.quality,
            file.updated_at,
          ].join(":"),
        )),
      ].join(":"),
    )
    .sort()
    .join("|");
}

function changeSetHasFiles(
  summary: ChangeSetSummary,
  filesById: Record<string, FileChangeSummary[]>,
) {
  return (filesById[summary.id]?.length ?? summary.file_count) > 0;
}

function lastReviewableAssistantMessageId(
  messages: UiSnapshot["messages"],
  timeline: UiSnapshot["timeline"],
  inActiveTurn: boolean,
) {
  const messageById = new Map(messages.map((message) => [message.id, message]));
  const latestUserIndex = inActiveTurn
    ? findLatestTimelineMessageIndex(timeline, messageById, "User")
    : -1;
  const endIndex = latestUserIndex >= 0 ? latestUserIndex - 1 : timeline.length - 1;

  for (let index = endIndex; index >= 0; index -= 1) {
    const item = timeline[index];
    if (typeof item === "string" || !("Message" in item)) continue;
    const message = messageById.get(item.Message);
    if (message?.role === "Assistant") {
      return message.id;
    }
  }
  return null;
}

function timelineMessageOrder(
  messages: UiSnapshot["messages"],
  timeline: UiSnapshot["timeline"],
) {
  const messageById = new Map(messages.map((message) => [message.id, message]));
  const order = new Map<string, number>();
  timeline.forEach((item, index) => {
    if (typeof item === "string" || !("Message" in item)) return;
    if (messageById.has(item.Message)) {
      order.set(item.Message, index);
    }
  });
  return order;
}

function findLatestTimelineMessageIndex(
  timeline: UiSnapshot["timeline"],
  messageById: Map<string, UiSnapshot["messages"][number]>,
  role: UiSnapshot["messages"][number]["role"],
) {
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    const item = timeline[index];
    if (typeof item === "string" || !("Message" in item)) continue;
    const message = messageById.get(item.Message);
    if (message?.role === role) {
      return index;
    }
  }
  return -1;
}

function timestampValue(value: string | null | undefined) {
  if (!value) return 0;
  const parsed = Date.parse(value);
  if (Number.isFinite(parsed)) return parsed;
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}

function diffQualityMessage(quality: DiffQuality) {
  const messages: Record<DiffQuality, string | null> = {
    Exact: null,
    LargeFileSkipped: "文件太大，已跳过内联差异预览",
    BinarySkipped: "二进制或不可读取文件，无法展示文本差异",
    MissingBaseline: "缺少可比较的基线内容，无法展示可靠差异",
    FragmentRejected: "只捕获到了片段级改动，已拒绝渲染为完整文件差异",
    LegacyIncomplete: "旧历史记录缺少完整快照，无法展示可靠差异",
  };
  return messages[quality] ?? null;
}

type ReviewDiffChange = FileChangeSummary | FileChangeRecord;

function needsDiffHydration(change: ReviewDiffChange) {
  if (!("old_text" in change)) return true;
  const oldText = change.old_text ?? "";
  const newText = change.new_text ?? "";
  return oldText.length === 0 && newText.length === 0 && (
    change.added_lines > 0 ||
    change.removed_lines > 0
  );
}

type ReviewPatchPreview =
  | { kind: "patch"; oldFile: FileContents; newFile: FileContents }
  | { kind: "message"; text: string };

function buildReviewDiff(change: ReviewDiffChange): ReviewPatchPreview {
  const quality = "quality" in change ? change.quality : "Exact";
  const unavailable = diffQualityMessage(quality);
  if (unavailable) {
    return { kind: "message", text: unavailable };
  }
  if (!("old_text" in change) || !("new_text" in change)) {
    return { kind: "message", text: "正在加载差异..." };
  }
  const oldText = change.old_text ?? "";
  const newText = change.new_text ?? "";

  if (oldText === newText) {
    return { kind: "message", text: "暂无可预览的文本差异" };
  }

  return {
    kind: "patch",
    oldFile: {
      name: change.path,
      contents: oldText,
      cacheKey: `${change.path}:old:${oldText.length}:${change.updated_at ?? ""}`,
    },
    newFile: {
      name: change.path,
      contents: newText,
      cacheKey: `${change.path}:new:${newText.length}:${change.updated_at ?? ""}`,
    },
  };
}

export function buildLineDiffRows(oldLines: string[], newLines: string[]): InlineDiffLine[] {
  return buildLineDiffRowsSegment(
    oldLines,
    0,
    oldLines.length,
    newLines,
    0,
    newLines.length,
    0,
  );
}

function buildLineDiffRowsSegment(
  oldLines: string[],
  oldStart: number,
  oldEnd: number,
  newLines: string[],
  newStart: number,
  newEnd: number,
  depth: number,
): InlineDiffLine[] {
  const oldLength = oldEnd - oldStart;
  const newLength = newEnd - newStart;

  if (oldLength === 0) {
    return newLines
      .slice(newStart, newEnd)
      .map((line, index) => makeDiffLine("added", null, newStart + index + 1, line));
  }
  if (newLength === 0) {
    return oldLines
      .slice(oldStart, oldEnd)
      .map((line, index) => makeDiffLine("removed", oldStart + index + 1, null, line));
  }

  if ((oldLength + 1) * (newLength + 1) <= REVIEW_DIFF_MAX_MATRIX_CELLS) {
    return buildLcsDiffRows(oldLines, oldStart, oldEnd, newLines, newStart, newEnd);
  }

  const rows: InlineDiffLine[] = [];
  while (
    oldStart < oldEnd &&
    newStart < newEnd &&
    oldLines[oldStart] === newLines[newStart]
  ) {
    rows.push(makeDiffLine("context", oldStart + 1, newStart + 1, oldLines[oldStart] ?? ""));
    oldStart += 1;
    newStart += 1;
  }

  let suffix = 0;
  while (
    oldStart + suffix < oldEnd &&
    newStart + suffix < newEnd &&
    oldLines[oldEnd - suffix - 1] === newLines[newEnd - suffix - 1]
  ) {
    suffix += 1;
  }

  const middleOldEnd = oldEnd - suffix;
  const middleNewEnd = newEnd - suffix;
  const anchors = depth > 16
    ? []
    : findUniqueOrderedLineAnchors(
      oldLines,
      oldStart,
      middleOldEnd,
      newLines,
      newStart,
      middleNewEnd,
    );

  if (anchors.length === 0) {
    rows.push(...buildContiguousFallbackRows(oldLines, oldStart, middleOldEnd, newLines, newStart, middleNewEnd));
  } else {
    let previousOld = oldStart;
    let previousNew = newStart;
    for (const anchor of anchors) {
      rows.push(
        ...buildLineDiffRowsSegment(
          oldLines,
          previousOld,
          anchor.oldIndex,
          newLines,
          previousNew,
          anchor.newIndex,
          depth + 1,
        ),
      );
      rows.push(
        makeDiffLine(
          "context",
          anchor.oldIndex + 1,
          anchor.newIndex + 1,
          oldLines[anchor.oldIndex] ?? "",
        ),
      );
      previousOld = anchor.oldIndex + 1;
      previousNew = anchor.newIndex + 1;
    }
    rows.push(
      ...buildLineDiffRowsSegment(
        oldLines,
        previousOld,
        middleOldEnd,
        newLines,
        previousNew,
        middleNewEnd,
        depth + 1,
      ),
    );
  }

  for (let index = suffix; index > 0; index -= 1) {
    const oldIndex = oldEnd - index;
    const newIndex = newEnd - index;
    rows.push(makeDiffLine("context", oldIndex + 1, newIndex + 1, oldLines[oldIndex] ?? ""));
  }

  return rows;
}

function buildLcsDiffRows(
  oldLines: string[],
  oldStart: number,
  oldEnd: number,
  newLines: string[],
  newStart: number,
  newEnd: number,
): InlineDiffLine[] {
  const oldLength = oldEnd - oldStart;
  const newLength = newEnd - newStart;
  const lcs = Array.from(
    { length: oldLength + 1 },
    () => new Uint32Array(newLength + 1),
  );

  for (let oldOffset = oldLength - 1; oldOffset >= 0; oldOffset -= 1) {
    for (let newOffset = newLength - 1; newOffset >= 0; newOffset -= 1) {
      lcs[oldOffset][newOffset] =
        oldLines[oldStart + oldOffset] === newLines[newStart + newOffset]
          ? lcs[oldOffset + 1][newOffset + 1] + 1
          : Math.max(lcs[oldOffset + 1][newOffset], lcs[oldOffset][newOffset + 1]);
    }
  }

  const rows: InlineDiffLine[] = [];
  let oldOffset = 0;
  let newOffset = 0;
  while (oldOffset < oldLength && newOffset < newLength) {
    const oldIndex = oldStart + oldOffset;
    const newIndex = newStart + newOffset;
    if (oldLines[oldIndex] === newLines[newIndex]) {
      rows.push(makeDiffLine("context", oldIndex + 1, newIndex + 1, oldLines[oldIndex] ?? ""));
      oldOffset += 1;
      newOffset += 1;
    } else if (lcs[oldOffset + 1][newOffset] >= lcs[oldOffset][newOffset + 1]) {
      rows.push(makeDiffLine("removed", oldIndex + 1, null, oldLines[oldIndex] ?? ""));
      oldOffset += 1;
    } else {
      rows.push(makeDiffLine("added", null, newIndex + 1, newLines[newIndex] ?? ""));
      newOffset += 1;
    }
  }
  while (oldOffset < oldLength) {
    const oldIndex = oldStart + oldOffset;
    rows.push(makeDiffLine("removed", oldIndex + 1, null, oldLines[oldIndex] ?? ""));
    oldOffset += 1;
  }
  while (newOffset < newLength) {
    const newIndex = newStart + newOffset;
    rows.push(makeDiffLine("added", null, newIndex + 1, newLines[newIndex] ?? ""));
    newOffset += 1;
  }
  return rows;
}

function buildContiguousFallbackRows(
  oldLines: string[],
  oldStart: number,
  oldEnd: number,
  newLines: string[],
  newStart: number,
  newEnd: number,
) {
  const rows: InlineDiffLine[] = [];
  for (let index = oldStart; index < oldEnd; index += 1) {
    rows.push(makeDiffLine("removed", index + 1, null, oldLines[index] ?? ""));
  }
  for (let index = newStart; index < newEnd; index += 1) {
    rows.push(makeDiffLine("added", null, index + 1, newLines[index] ?? ""));
  }
  return rows;
}

function findUniqueOrderedLineAnchors(
  oldLines: string[],
  oldStart: number,
  oldEnd: number,
  newLines: string[],
  newStart: number,
  newEnd: number,
) {
  const oldUnique = collectUniqueLineIndexes(oldLines, oldStart, oldEnd);
  const newUnique = collectUniqueLineIndexes(newLines, newStart, newEnd);
  const candidates: Array<{ oldIndex: number; newIndex: number }> = [];

  for (const [line, oldIndex] of oldUnique) {
    const newIndex = newUnique.get(line);
    if (newIndex !== undefined) {
      candidates.push({ oldIndex, newIndex });
    }
  }

  candidates.sort((a, b) => a.oldIndex - b.oldIndex || a.newIndex - b.newIndex);
  return longestIncreasingNewLineSubsequence(candidates);
}

function collectUniqueLineIndexes(lines: string[], start: number, end: number) {
  const seen = new Map<string, number>();
  const repeated = new Set<string>();

  for (let index = start; index < end; index += 1) {
    const line = lines[index] ?? "";
    if (repeated.has(line)) continue;
    if (seen.has(line)) {
      seen.delete(line);
      repeated.add(line);
    } else {
      seen.set(line, index);
    }
  }

  return seen;
}

function longestIncreasingNewLineSubsequence(
  candidates: Array<{ oldIndex: number; newIndex: number }>,
) {
  if (candidates.length <= 1) return candidates;

  const previous = new Array<number>(candidates.length).fill(-1);
  const tailCandidateIndexes: number[] = [];
  const tailNewIndexes: number[] = [];

  for (let index = 0; index < candidates.length; index += 1) {
    const value = candidates[index].newIndex;
    let low = 0;
    let high = tailNewIndexes.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (tailNewIndexes[mid] < value) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    if (low > 0) {
      previous[index] = tailCandidateIndexes[low - 1];
    }
    tailNewIndexes[low] = value;
    tailCandidateIndexes[low] = index;
  }

  const result: Array<{ oldIndex: number; newIndex: number }> = [];
  let cursor = tailCandidateIndexes[tailCandidateIndexes.length - 1];
  while (cursor !== undefined && cursor >= 0) {
    result.push(candidates[cursor]);
    cursor = previous[cursor];
  }
  result.reverse();
  return result;
}

function makeDiffLine(
  kind: InlineDiffLine["kind"],
  oldLine: number | null,
  newLine: number | null,
  text: string,
): InlineDiffLine {
  return {
    id: "",
    kind,
    oldLine,
    newLine,
    text,
  };
}

function RefreshIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M20 11a8 8 0 0 0-14.7-4.4" />
      <path d="M4 7V3h4" />
      <path d="M4 13a8 8 0 0 0 14.7 4.4" />
      <path d="M20 17v4h-4" />
    </svg>
  );
}

function SearchIcon() {
  return (
    <svg className="review-git-filetree-search-icon" viewBox="0 0 20 20" aria-hidden="true">
      <circle cx="8.5" cy="8.5" r="5.2" />
      <path d="m12.4 12.4 4 4" />
    </svg>
  );
}

function GitTabIcon() {
  return (
    <svg className="review-tab-icon" viewBox="0 0 20 20" aria-hidden="true">
      <path d="M5.8 3.9v7.4a3 3 0 0 0 3 3h2.4" />
      <path d="M14.2 5.2a3 3 0 0 1-3 3H5.8" />
      <circle cx="5.8" cy="3.9" r="1.5" />
      <circle cx="5.8" cy="16.1" r="1.5" />
      <circle cx="14.2" cy="5.2" r="1.5" />
    </svg>
  );
}

function FolderTreeIcon({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 20 20" aria-hidden="true">
      <path d="M2.5 6.2c0-1 .8-1.8 1.8-1.8h3.4l1.5 1.6h6.5c1 0 1.8.8 1.8 1.8v6.7c0 1-.8 1.8-1.8 1.8H4.3c-1 0-1.8-.8-1.8-1.8V6.2Z" />
      <path d="M2.5 8.2h15" />
    </svg>
  );
}

function FileGroup({
  title,
  files,
  changeSetId,
  onFileSelect,
  activePath,
  compact = false,
}: {
  title: string;
  files: ChangedFile[];
  changeSetId: string;
  onFileSelect: (path: string, changeSetId: string) => void;
  activePath?: string;
  compact?: boolean;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [collapsedDirs, setCollapsedDirs] = useState<Set<string>>(new Set());

  const tree = useMemo(() => buildFileTree(files), [files]);

  const expandedDirs = useMemo(() => {
    const all = new Set<string>();
    function walk(nodes: TreeNode[]) {
      for (const n of nodes) {
        if (n.kind === "directory") {
          all.add(n.path);
          walk(n.children);
        }
      }
    }
    walk(tree);
    for (const p of collapsedDirs) all.delete(p);
    return all;
  }, [tree, collapsedDirs]);

  const handleToggleDir = useCallback((dirPath: string) => {
    setCollapsedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(dirPath)) next.delete(dirPath);
      else next.add(dirPath);
      return next;
    });
  }, []);

  if (files.length === 0) return null;

  return (
    <div className={`review-group ${compact ? "review-group-compact" : ""}`}>
      <div
        className="review-group-header"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="review-group-arrow">
          {collapsed ? "\u25b6" : "\u25bc"}
        </span>
        <span className="review-group-title">
          {title} ({files.length})
        </span>
      </div>

      {!collapsed && (
        <div className="review-diff-tree">
          {tree.map((node) => (
            <DiffTreeNode
              key={node.path}
              node={node}
              depth={0}
              expandedDirs={expandedDirs}
              onToggleDir={handleToggleDir}
              onFileSelect={onFileSelect}
              changeSetId={changeSetId}
              activePath={activePath}
              compact={compact}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function DiffTreeNode({
  node,
  depth,
  expandedDirs,
  onToggleDir,
  onFileSelect,
  changeSetId,
  activePath,
  compact,
}: {
  node: TreeNode;
  depth: number;
  expandedDirs: Set<string>;
  onToggleDir: (path: string) => void;
  onFileSelect: (path: string, changeSetId: string) => void;
  changeSetId: string;
  activePath?: string;
  compact: boolean;
}) {
  const isDir = node.kind === "directory";
  const isExpanded = expandedDirs.has(node.path);
  const indent = compact ? `${depth * 12 + 4}px` : `${depth * 14 + 8}px`;

  if (isDir) {
    return (
      <div className="review-diff-tree-branch">
        <div
          className="review-diff-row review-diff-dir"
          style={{ paddingLeft: indent }}
          onClick={() => onToggleDir(node.path)}
        >
          <span className="review-tree-arrow">
            {isExpanded ? "\u25bc" : "\u25b6"}
          </span>
          <FolderTreeIcon className="review-tree-icon review-folder-icon" />
          <span className="review-diff-name">{node.name}</span>
        </div>
        {isExpanded && (
          <div className="review-diff-children">
            {node.children.map((child) => (
              <DiffTreeNode
                key={child.path}
                node={child}
                depth={depth + 1}
                expandedDirs={expandedDirs}
                onToggleDir={onToggleDir}
                onFileSelect={onFileSelect}
                changeSetId={changeSetId}
                activePath={activePath}
                compact={compact}
              />
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="review-diff-tree-leaf">
      <div
        className={`review-diff-row review-diff-file ${activePath === node.path ? "is-active" : ""}`}
        style={{ paddingLeft: indent }}
        onClick={() => onFileSelect(node.path, changeSetId)}
      >
        <span className="review-tree-arrow" />
        <img className="review-tree-icon" src={getFileIcon(node.path)} alt="" />
        <span className="review-diff-name">{node.name}</span>
        <div className="review-diff-stats">
          <span className="review-stat-added">+{node.stats.added}</span>
          <span className="review-stat-removed">-{node.stats.removed}</span>
        </div>
      </div>
    </div>
  );
}

function UntrackedTree({
  files,
  onFileSelect,
  onRefresh,
  activePath,
  compact = false,
}: {
  files: ChangedFile[];
  onFileSelect: (path: string, changeSetId: string) => void;
  onRefresh: () => void | Promise<void>;
  activePath?: string;
  compact?: boolean;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = useState<Map<string, FileEntry[]>>(new Map());
  const [error, setError] = useState<string | null>(null);

  const rootEntries = useMemo(() => files.map(changedFileToEntry), [files]);

  const handleToggleDir = useCallback(
    async (dirPath: string) => {
      if (expandedDirs.has(dirPath)) {
        setExpandedDirs((prev) => {
          const next = new Set(prev);
          next.delete(dirPath);
          return next;
        });
        return;
      }

      if (!childrenCache.has(dirPath)) {
        try {
          const children = await fsListDir(dirPath);
          setChildrenCache((prev) => new Map(prev).set(dirPath, children));
        } catch (e) {
          setError(String(e));
          return;
        }
      }

      setExpandedDirs((prev) => new Set(prev).add(dirPath));
    },
    [childrenCache, expandedDirs],
  );

  const handleTrackFile = useCallback(
    async (path: string) => {
      const accepted = await confirm(`是否跟踪文件 ${path}？`);
      if (!accepted) return;

      try {
        await gitStage([path]);
        await onRefresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [onRefresh],
  );

  if (files.length === 0) return null;

  return (
    <div className={`review-group review-untracked-group ${compact ? "review-group-compact" : ""}`}>
      <div
        className="review-group-header"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="review-group-arrow">
          {collapsed ? "\u25b6" : "\u25bc"}
        </span>
        <span className="review-group-title">
          未跟踪 ({files.length})
        </span>
      </div>

      {!collapsed && (
        <div className="review-untracked-tree">
          {error && <div className="review-tree-error">{error}</div>}
          {rootEntries.map((entry) => (
            <UntrackedTreeNode
              key={entry.path}
              entry={entry}
              depth={0}
              expandedDirs={expandedDirs}
              childrenCache={childrenCache}
              onToggleDir={handleToggleDir}
              onFileSelect={onFileSelect}
              onTrackFile={handleTrackFile}
              activePath={activePath}
              compact={compact}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function UntrackedTreeNode({
  entry,
  depth,
  expandedDirs,
  childrenCache,
  onToggleDir,
  onFileSelect,
  onTrackFile,
  activePath,
  compact,
}: {
  entry: FileEntry;
  depth: number;
  expandedDirs: Set<string>;
  childrenCache: Map<string, FileEntry[]>;
  onToggleDir: (path: string) => void;
  onFileSelect: (path: string, changeSetId: string) => void;
  onTrackFile: (path: string) => void;
  activePath?: string;
  compact: boolean;
}) {
  const isDir = entry.kind === "Directory";
  const isExpanded = expandedDirs.has(entry.path);
  const children = childrenCache.get(entry.path);
  const indent = compact ? `${depth * 12 + 4}px` : `${depth * 14 + 8}px`;

  return (
    <>
      <div
        className={`review-untracked-row ${isDir ? "review-untracked-dir" : "review-untracked-file"} ${activePath === entry.path ? "is-active" : ""}`}
        style={{ paddingLeft: indent }}
        onClick={() =>
          isDir
            ? onToggleDir(entry.path)
            : onFileSelect(entry.path, "git-worktree:untracked")
        }
        onContextMenu={(event) => {
          if (isDir) return;
          event.preventDefault();
          event.stopPropagation();
          onTrackFile(entry.path);
        }}
        title={isDir ? entry.path : `${entry.path} - 右键点击以跟踪`}
      >
        <span className="review-tree-arrow">
          {isDir ? (isExpanded ? "v" : ">") : ""}
        </span>
        {isDir ? (
          <FolderTreeIcon className="review-tree-icon review-folder-icon" />
        ) : (
          <img className="review-tree-icon" src={getFileIcon(entry.path)} alt="" />
        )}
        <span className="review-tree-name">{entry.name}</span>
      </div>
      {isDir && isExpanded && children && (
        <div className="review-tree-children">
          {children.map((child) => (
            <UntrackedTreeNode
              key={child.path}
              entry={child}
              depth={depth + 1}
              expandedDirs={expandedDirs}
              childrenCache={childrenCache}
              onToggleDir={onToggleDir}
              onFileSelect={onFileSelect}
              onTrackFile={onTrackFile}
              activePath={activePath}
              compact={compact}
            />
          ))}
        </div>
      )}
    </>
  );
}

function changedFileToEntry(file: ChangedFile): FileEntry {
  const path = normalizeEntryPath(file.path);
  return {
    name: entryName(path),
    kind: isDirectoryEntry(file.path) ? "Directory" : "File",
    path,
  };
}

function normalizeEntryPath(path: string) {
  return path.replace(/\\/g, "/").replace(/\/$/, "");
}

function isDirectoryEntry(path: string) {
  return path.endsWith("/") || path.endsWith("\\");
}

function entryName(path: string) {
  return path.split("/").filter(Boolean).pop() ?? path;
}
