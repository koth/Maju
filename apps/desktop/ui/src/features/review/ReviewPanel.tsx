import { useState, useMemo, useCallback } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { UiSnapshot, ChangedFile, ChangeSection, InspectorTab, FileEntry } from "../../types";
import { fsListDir, gitStage } from "../../lib/tauri";
import { FileTree } from "../filetree/FileTree";
import { getFileIcon, getFolderIcon } from "../filetree/file-icons";
import "./ReviewPanel.css";

const MAX_REVIEW_FILES = 300;

interface Props {
  snapshot: UiSnapshot;
  refreshing: boolean;
  onRefresh: () => void | Promise<void>;
  onFileSelect: (path: string) => void;
  onFileOpen: (path: string) => void;
}

export function ReviewPanel({ snapshot, refreshing, onRefresh, onFileSelect, onFileOpen }: Props) {
  const [tab, setTab] = useState<InspectorTab>("Diff");
  const [filter, setFilter] = useState("");
  const [fileTreeRefreshSignal, setFileTreeRefreshSignal] = useState(0);

  const handleRefresh = useCallback(() => {
    if (tab === "Diff") {
      onRefresh();
      return;
    }

    setFileTreeRefreshSignal((signal) => signal + 1);
  }, [onRefresh, tab]);

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

  const grouped = useMemo(() => {
    const groups: Record<ChangeSection, ChangedFile[]> = {
      Unstaged: [],
      Staged: [],
      Untracked: [],
    };
    visibleFiles.forEach((f) => groups[f.section].push(f));
    return groups;
  }, [visibleFiles]);

  return (
    <div className="review-panel">
      <div className="review-tabs">
        <button
          className={`review-tab ${tab === "Diff" ? "review-tab-active" : ""}`}
          onClick={() => setTab("Diff")}
        >
          Diff
        </button>
        <button
          className={`review-tab ${tab === "Files" ? "review-tab-active" : ""}`}
          onClick={() => setTab("Files")}
        >
          All Files
        </button>
        <div className="review-tabs-spacer" />
        <button
          type="button"
          className="review-refresh-btn"
          onClick={handleRefresh}
          disabled={tab === "Diff" && refreshing}
          title={tab === "Diff" ? "Refresh Git status" : "Refresh selected file tree directory"}
          aria-label={tab === "Diff" ? "Refresh Git status" : "Refresh selected file tree directory"}
        >
          <RefreshIcon />
        </button>
      </div>

      <div className="review-tab-panel" hidden={tab !== "Files"}>
        <FileTree
          onFileOpen={onFileOpen}
          refreshSignal={fileTreeRefreshSignal}
        />
      </div>

      <div className="review-tab-panel" hidden={tab !== "Diff"}>
          <div className="review-meta">
            {snapshot.repository.changed_files.length} changed files
            {filteredFiles.length > MAX_REVIEW_FILES && (
              <span> / showing first {MAX_REVIEW_FILES}</span>
            )}
          </div>

          <div className="review-filter">
            <input
              type="text"
              className="review-filter-input"
              placeholder="Filter files..."
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>

          <FileGroup
            title="Unstaged"
            files={grouped.Unstaged}
            onFileSelect={onFileSelect}
          />
          <FileGroup
            title="Staged"
            files={grouped.Staged}
            onFileSelect={onFileSelect}
          />
          <UntrackedTree
            files={grouped.Untracked}
            onFileOpen={onFileOpen}
            onRefresh={onRefresh}
          />
      </div>
    </div>
  );
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

function FileGroup({
  title,
  files,
  onFileSelect,
}: {
  title: string;
  files: ChangedFile[];
  onFileSelect: (path: string) => void;
}) {
  const [collapsed, setCollapsed] = useState(false);

  if (files.length === 0) return null;

  return (
    <div className="review-group">
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

      {!collapsed &&
        files.map((file) => (
          <div key={file.path} className="review-file">
            <div
              className="review-file-row"
              onClick={() => onFileSelect(file.path)}
            >
              <img className="review-file-icon" src={getFileIcon(file.path)} alt="" />
              <span className="review-file-path">{file.path}</span>
              <div className="review-file-stats">
                <span className="review-stat-added">
                  +{file.stats.added}
                </span>
                <span className="review-stat-removed">
                  -{file.stats.removed}
                </span>
              </div>
            </div>
          </div>
        ))}
    </div>
  );
}

function UntrackedTree({
  files,
  onFileOpen,
  onRefresh,
}: {
  files: ChangedFile[];
  onFileOpen: (path: string) => void;
  onRefresh: () => void | Promise<void>;
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
    <div className="review-group review-untracked-group">
      <div
        className="review-group-header"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="review-group-arrow">
          {collapsed ? "\u25b6" : "\u25bc"}
        </span>
        <span className="review-group-title">
          Untracked ({files.length})
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
              onFileOpen={onFileOpen}
              onTrackFile={handleTrackFile}
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
  onFileOpen,
  onTrackFile,
}: {
  entry: FileEntry;
  depth: number;
  expandedDirs: Set<string>;
  childrenCache: Map<string, FileEntry[]>;
  onToggleDir: (path: string) => void;
  onFileOpen: (path: string) => void;
  onTrackFile: (path: string) => void;
}) {
  const isDir = entry.kind === "Directory";
  const isExpanded = expandedDirs.has(entry.path);
  const children = childrenCache.get(entry.path);
  const icon = isDir ? getFolderIcon(entry.name, isExpanded) : getFileIcon(entry.path);

  return (
    <>
      <div
        className={`review-untracked-row ${isDir ? "review-untracked-dir" : "review-untracked-file"}`}
        style={{ paddingLeft: `${depth * 14 + 8}px` }}
        onClick={() => (isDir ? onToggleDir(entry.path) : onFileOpen(entry.path))}
        onContextMenu={(event) => {
          if (isDir) return;
          event.preventDefault();
          event.stopPropagation();
          onTrackFile(entry.path);
        }}
        title={isDir ? entry.path : `${entry.path} - right click to track`}
      >
        <span className="review-tree-arrow">
          {isDir ? (isExpanded ? "v" : ">") : ""}
        </span>
        <img className="review-tree-icon" src={icon} alt="" />
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
              onFileOpen={onFileOpen}
              onTrackFile={onTrackFile}
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
