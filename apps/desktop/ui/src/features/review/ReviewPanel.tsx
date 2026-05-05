import { useState, useMemo, useCallback } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { UiSnapshot, ChangedFile, ChangeSection, DiffStats, InspectorTab, FileEntry } from "../../types";
import { fsListDir, gitStage } from "../../lib/tauri";
import { FileTree } from "../filetree/FileTree";
import { getFileIcon, getFolderIcon } from "../filetree/file-icons";
import "./ReviewPanel.css";

interface TreeNode {
  name: string;
  path: string;
  kind: "file" | "directory";
  children: TreeNode[];
  stats: DiffStats;
  file?: ChangedFile;
}

const MAX_REVIEW_FILES = 300;

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
          Git
        </button>
        <button
          className={`review-tab ${tab === "Files" ? "review-tab-active" : ""}`}
          onClick={() => setTab("Files")}
        >
          所有文件
        </button>
        <div className="review-tabs-spacer" />
        <button
          type="button"
          className="review-refresh-btn"
          onClick={handleRefresh}
          disabled={tab === "Diff" && refreshing}
          title={tab === "Diff" ? "刷新 Git 状态" : "刷新选中的文件目录"}
          aria-label={tab === "Diff" ? "刷新 Git 状态" : "刷新选中的文件目录"}
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
            onFileSelect={onFileSelect}
          />
          <FileGroup
            title="已暂存"
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
}: {
  node: TreeNode;
  depth: number;
  expandedDirs: Set<string>;
  onToggleDir: (path: string) => void;
  onFileSelect: (path: string) => void;
}) {
  const isDir = node.kind === "directory";
  const isExpanded = expandedDirs.has(node.path);

  if (isDir) {
    return (
      <div className="review-diff-tree-branch">
        <div
          className="review-diff-row review-diff-dir"
          style={{ paddingLeft: `${depth * 14 + 8}px` }}
          onClick={() => onToggleDir(node.path)}
        >
          <span className="review-tree-arrow">
            {isExpanded ? "\u25bc" : "\u25b6"}
          </span>
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
        className="review-diff-row review-diff-file"
        style={{ paddingLeft: `${depth * 14 + 8}px` }}
        onClick={() => onFileSelect(node.path)}
      >
        <span className="review-tree-arrow" />
        <img className="review-tree-icon" src={getFileIcon(node.path)} alt="" />
        <span className="review-diff-name">{node.name}</span>
        <div className="review-diff-stats">
          {node.stats.added > 0 && (
            <span className="review-stat-added">+{node.stats.added}</span>
          )}
          {node.stats.removed > 0 && (
            <span className="review-stat-removed">-{node.stats.removed}</span>
          )}
        </div>
      </div>
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
        title={isDir ? entry.path : `${entry.path} - 右键点击以跟踪`}
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
