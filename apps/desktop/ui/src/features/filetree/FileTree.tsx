import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { FileEntry } from "../../types";
import { fsDeleteFile, fsListDir, fsRename, fsReveal } from "../../lib/tauri";
import { getFileIcon } from "./file-icons";
import "./FileTree.css";

interface FileTreeProps {
  workspaceRoot: string;
  onFileOpen: (filePath: string) => void;
  refreshSignal?: number;
  onAddComposerReference?: (filePath: string) => void;
  composerReferenceEnabled?: boolean;
  activePath?: string | null;
  variant?: "panel" | "inline";
}

interface SelectedEntry {
  path: string;
  kind: FileEntry["kind"];
}

interface ContextMenuState {
  entry: FileEntry;
  x: number;
  y: number;
}

interface RenameState {
  entry: FileEntry;
  value: string;
}

interface TreeNodeProps {
  entry: FileEntry;
  depth: number;
  expandedDirs: Set<string>;
  childrenCache: Map<string, FileEntry[]>;
  selectedPath: string | null;
  renamingPath: string | null;
  renameValue: string;
  onToggleDir: (path: string) => void;
  onSelect: (entry: FileEntry) => void;
  onFileOpen: (filePath: string) => void;
  onContextMenu: (entry: FileEntry, x: number, y: number) => void;
  onRenameValueChange: (value: string) => void;
  onRenameCommit: () => void;
  onRenameCancel: () => void;
  filterText: string;
}

function TreeNode({
  entry,
  depth,
  expandedDirs,
  childrenCache,
  selectedPath,
  renamingPath,
  renameValue,
  onToggleDir,
  onSelect,
  onFileOpen,
  onContextMenu,
  onRenameValueChange,
  onRenameCommit,
  onRenameCancel,
  filterText,
}: TreeNodeProps) {
  const isDir = entry.kind === "Directory";
  const isExpanded = expandedDirs.has(entry.path);
  const isSelected = selectedPath === entry.path;
  const isRenaming = renamingPath === entry.path;
  const children = childrenCache.get(entry.path);
  const hasFilter = filterText.trim().length > 0;

  if (!treeNodeMatches(entry, childrenCache, filterText)) {
    return null;
  }

  const handleClick = useCallback(() => {
    if (isRenaming) return;
    onSelect(entry);
    if (isDir) {
      onToggleDir(entry.path);
    } else {
      onFileOpen(entry.path);
    }
  }, [entry, isDir, isRenaming, onFileOpen, onSelect, onToggleDir]);

  const handleContextMenu = useCallback((event: ReactMouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    onSelect(entry);
    onContextMenu(entry, event.clientX, event.clientY);
  }, [entry, onContextMenu, onSelect]);

  return (
    <>
      <div
        className={`filetree-node ${isDir ? "filetree-dir" : "filetree-file"} ${isSelected ? "is-selected" : ""}`}
        style={{
          paddingLeft: `calc(${depth} * var(--filetree-indent-step, 16px) + var(--filetree-indent-base, 8px))`,
        }}
        onClick={handleClick}
        onContextMenu={handleContextMenu}
        title={entry.path}
      >
        <span className="filetree-chevron">
          {isDir ? (isExpanded ? "\u25BE" : "\u25B8") : " "}
        </span>
        {isDir ? (
          <FolderTreeIcon className="filetree-icon filetree-folder-icon" />
        ) : (
          <img className="filetree-icon" src={getFileIcon(entry.path)} alt="" />
        )}
        {isRenaming ? (
          <input
            className="filetree-rename-input"
            value={renameValue}
            autoFocus
            onClick={(event) => event.stopPropagation()}
            onChange={(event) => onRenameValueChange(event.target.value)}
            onBlur={onRenameCommit}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                onRenameCommit();
              } else if (event.key === "Escape") {
                event.preventDefault();
                onRenameCancel();
              }
            }}
          />
        ) : (
          <span className="filetree-name">{entry.name}</span>
        )}
      </div>
      {isDir && (isExpanded || hasFilter) && children && (
        <div className="filetree-children">
          {children.map((child) => (
            <TreeNode
              key={child.path}
              entry={child}
              depth={depth + 1}
              expandedDirs={expandedDirs}
              childrenCache={childrenCache}
              selectedPath={selectedPath}
              renamingPath={renamingPath}
              renameValue={renameValue}
              onToggleDir={onToggleDir}
              onSelect={onSelect}
              onFileOpen={onFileOpen}
              onContextMenu={onContextMenu}
              onRenameValueChange={onRenameValueChange}
              onRenameCommit={onRenameCommit}
              onRenameCancel={onRenameCancel}
              filterText={filterText}
            />
          ))}
        </div>
      )}
    </>
  );
}

export function FileTree({
  workspaceRoot,
  onFileOpen,
  refreshSignal = 0,
  onAddComposerReference,
  composerReferenceEnabled = false,
  activePath = null,
  variant = "panel",
}: FileTreeProps) {
  const [rootEntries, setRootEntries] = useState<FileEntry[]>([]);
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = useState<Map<string, FileEntry[]>>(
    new Map()
  );
  const [selectedEntry, setSelectedEntry] = useState<SelectedEntry | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [renameState, setRenameState] = useState<RenameState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filterText, setFilterText] = useState("");
  const [loadingPaths, setLoadingPaths] = useState<Set<string>>(new Set());
  const committingRenameRef = useRef(false);
  const workspaceRootRef = useRef(workspaceRoot);
  const consumedRefreshSignalRef = useRef(refreshSignal);
  workspaceRootRef.current = workspaceRoot;

  const refreshDirectory = useCallback(
    async (dirPath: string, requestWorkspaceRoot = workspaceRootRef.current) => {
      if (workspaceRootRef.current === requestWorkspaceRoot) {
        setError(null);
        setLoadingPaths((prev) => {
          const next = new Set(prev);
          next.add(dirPath);
          return next;
        });
      }
      try {
        const entries = await fsListDir(dirPath);
        if (workspaceRootRef.current !== requestWorkspaceRoot) {
          return false;
        }
        if (dirPath) {
          setChildrenCache((prev) => new Map(prev).set(dirPath, entries));
        } else {
          setRootEntries(entries);
        }
        return true;
      } finally {
        if (workspaceRootRef.current === requestWorkspaceRoot) {
          setLoadingPaths((prev) => {
            if (!prev.has(dirPath)) return prev;
            const next = new Set(prev);
            next.delete(dirPath);
            return next;
          });
        }
      }
    },
    [],
  );

  useEffect(() => {
    const requestWorkspaceRoot = workspaceRoot;
    setRootEntries([]);
    setExpandedDirs(new Set());
    setChildrenCache(new Map());
    setSelectedEntry(null);
    setContextMenu(null);
    setRenameState(null);
    setError(null);
    setLoadingPaths(new Set());
    refreshDirectory("", requestWorkspaceRoot).catch((e) => {
      if (workspaceRootRef.current === requestWorkspaceRoot) {
        setError(String(e));
      }
    });
  }, [refreshDirectory, workspaceRoot]);

  useEffect(() => {
    if (!activePath) return;
    setSelectedEntry({ path: activePath, kind: "File" });
  }, [activePath]);

  const activeParentDirs = useMemo(() => {
    if (!activePath) return [];
    const parts = activePath.replace(/\\/g, "/").split("/").filter(Boolean);
    parts.pop();
    return parts.map((_, index) => parts.slice(0, index + 1).join("/"));
  }, [activePath]);

  useEffect(() => {
    if (activeParentDirs.length === 0) return;
    let cancelled = false;
    const requestWorkspaceRoot = workspaceRoot;

    setExpandedDirs((prev) => {
      const next = new Set(prev);
      activeParentDirs.forEach((dirPath) => next.add(dirPath));
      return next;
    });

    (async () => {
      for (const dirPath of activeParentDirs) {
        if (cancelled || workspaceRootRef.current !== requestWorkspaceRoot) return;
        try {
          await refreshDirectory(dirPath, requestWorkspaceRoot);
        } catch (e) {
          if (!cancelled && workspaceRootRef.current === requestWorkspaceRoot) {
            setError(String(e));
          }
          return;
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [activeParentDirs, refreshDirectory, workspaceRoot]);

  useEffect(() => {
    if (refreshSignal === 0 || refreshSignal === consumedRefreshSignalRef.current) return;
    consumedRefreshSignalRef.current = refreshSignal;

    const dirPath = selectedEntry
      ? selectedEntry.kind === "Directory"
        ? selectedEntry.path
        : parentDirectory(selectedEntry.path)
      : "";
    const requestWorkspaceRoot = workspaceRoot;

    refreshDirectory(dirPath, requestWorkspaceRoot)
      .then((applied) => {
        if (applied && dirPath) {
          setExpandedDirs((prev) => new Set(prev).add(dirPath));
        }
      })
      .catch((e) => {
        if (workspaceRootRef.current === requestWorkspaceRoot) {
          setError(String(e));
        }
      });
  }, [refreshDirectory, refreshSignal, selectedEntry, workspaceRoot]);

  useEffect(() => {
    if (!contextMenu) return;
    const close = () => setContextMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("keydown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", close);
      window.removeEventListener("blur", close);
    };
  }, [contextMenu]);

  const handleToggleDir = useCallback(
    async (dirPath: string) => {
      if (expandedDirs.has(dirPath)) {
        setExpandedDirs((prev) => {
          const next = new Set(prev);
          next.delete(dirPath);
          return next;
        });
      } else {
        if (!childrenCache.has(dirPath)) {
          try {
            await refreshDirectory(dirPath);
          } catch (e) {
            setError(String(e));
            return;
          }
        }
        setExpandedDirs((prev) => new Set(prev).add(dirPath));
      }
    },
    [childrenCache, expandedDirs, refreshDirectory]
  );

  const handleSelect = useCallback((entry: FileEntry) => {
    setSelectedEntry({ path: entry.path, kind: entry.kind });
  }, []);

  const handleContextMenu = useCallback((entry: FileEntry, x: number, y: number) => {
    const width = 190;
    const height = entry.kind === "File" && composerReferenceEnabled && onAddComposerReference ? 148 : entry.kind === "File" ? 118 : 82;
    setContextMenu({
      entry,
      x: Math.min(x, window.innerWidth - width - 8),
      y: Math.min(y, window.innerHeight - height - 8),
    });
  }, [composerReferenceEnabled, onAddComposerReference]);

  const startRename = useCallback((entry: FileEntry) => {
    setContextMenu(null);
    setRenameState({ entry, value: entry.name });
  }, []);

  const cancelRename = useCallback(() => {
    committingRenameRef.current = true;
    setRenameState(null);
    requestAnimationFrame(() => {
      committingRenameRef.current = false;
    });
  }, []);

  const commitRename = useCallback(async () => {
    if (!renameState || committingRenameRef.current) return;
    const nextName = renameState.value.trim();
    if (!nextName || nextName === renameState.entry.name) {
      cancelRename();
      return;
    }
    if (nextName.includes("/") || nextName.includes("\\") || nextName === "." || nextName === "..") {
      setError("名称只能是单个文件或文件夹名");
      return;
    }

    committingRenameRef.current = true;
    try {
      const renamed = await fsRename(renameState.entry.path, nextName);
      const parent = parentDirectory(renameState.entry.path);
      const wasExpanded = expandedDirs.has(renameState.entry.path);
      setSelectedEntry({ path: renamed.path, kind: renamed.kind });
      if (renameState.entry.kind === "Directory") {
        setChildrenCache((prev) => {
          const next = new Map(prev);
          next.delete(renameState.entry.path);
          return next;
        });
        setExpandedDirs((prev) => {
          const next = new Set(prev);
          if (next.delete(renameState.entry.path)) {
            next.add(renamed.path);
          }
          return next;
        });
      }
      setRenameState(null);
      await refreshDirectory(parent);
      if (renamed.kind === "Directory" && wasExpanded) {
        await refreshDirectory(renamed.path);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      committingRenameRef.current = false;
    }
  }, [cancelRename, expandedDirs, refreshDirectory, renameState]);

  const handleDeleteFile = useCallback(async (entry: FileEntry) => {
    if (entry.kind !== "File") return;
    setContextMenu(null);
    const accepted = await confirm(`确定删除文件 ${entry.path}？`);
    if (!accepted) return;

    try {
      await fsDeleteFile(entry.path);
      setSelectedEntry((current) => (current?.path === entry.path ? null : current));
      await refreshDirectory(parentDirectory(entry.path));
    } catch (e) {
      setError(String(e));
    }
  }, [refreshDirectory]);

  const handleReveal = useCallback(async (entry: FileEntry) => {
    setContextMenu(null);
    try {
      await fsReveal(entry.path, entry.kind === "File");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleAddReference = useCallback((entry: FileEntry) => {
    if (entry.kind !== "File" || !composerReferenceEnabled || !onAddComposerReference) return;
    setContextMenu(null);
    onAddComposerReference(entry.path);
  }, [composerReferenceEnabled, onAddComposerReference]);

  const rootLoading = loadingPaths.has("");

  return (
    <div className={`filetree filetree-${variant}`}>
      {variant === "panel" ? (
        <div className="filetree-header">所有文件</div>
      ) : (
        <label className="filetree-search">
          <SearchIcon />
          <input
            className="filetree-search-input"
            value={filterText}
            placeholder="筛选文件..."
            onChange={(event) => setFilterText(event.target.value)}
          />
        </label>
      )}
      {error && <div className="filetree-inline-error">{error}</div>}
      <div className="filetree-list">
        {rootLoading && rootEntries.length === 0 ? (
          <div className="filetree-loading" role="status" aria-live="polite">
            <span className="filetree-spinner" aria-hidden="true" />
            <span>正在载入文件树</span>
          </div>
        ) : rootEntries.length === 0 && !error ? (
          <div className="filetree-empty">目录为空</div>
        ) : (
          rootEntries.map((entry) => (
            <TreeNode
              key={entry.path}
              entry={entry}
              depth={0}
              expandedDirs={expandedDirs}
              childrenCache={childrenCache}
              selectedPath={selectedEntry?.path ?? null}
              renamingPath={renameState?.entry.path ?? null}
              renameValue={renameState?.value ?? ""}
              onToggleDir={handleToggleDir}
              onSelect={handleSelect}
              onFileOpen={onFileOpen}
              onContextMenu={handleContextMenu}
              onRenameValueChange={(value) =>
                setRenameState((current) => current ? { ...current, value } : current)
              }
              onRenameCommit={commitRename}
              onRenameCancel={cancelRename}
              filterText={filterText}
            />
          ))
        )}
      </div>
      {contextMenu && (
        <div
          className="filetree-context-menu"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          role="menu"
          onClick={(event) => event.stopPropagation()}
        >
          <button type="button" role="menuitem" onClick={() => startRename(contextMenu.entry)}>
            重命名
          </button>
          <button type="button" role="menuitem" onClick={() => handleReveal(contextMenu.entry)}>
            {contextMenu.entry.kind === "Directory" ? "在文件浏览器中打开" : "打开所在位置"}
          </button>
          {contextMenu.entry.kind === "File" && composerReferenceEnabled && onAddComposerReference && (
            <button
              type="button"
              role="menuitem"
              title="发送到上下文"
              onClick={() => handleAddReference(contextMenu.entry)}
            >
              发送到上下文
            </button>
          )}
          {contextMenu.entry.kind === "File" && (
            <button
              type="button"
              role="menuitem"
              className="filetree-menu-danger"
              onClick={() => handleDeleteFile(contextMenu.entry)}
            >
              删除文件
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function parentDirectory(path: string) {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}

function treeNodeMatches(entry: FileEntry, childrenCache: Map<string, FileEntry[]>, filterText: string): boolean {
  const query = filterText.trim().toLowerCase();
  if (!query) return true;
  if (entry.name.toLowerCase().includes(query) || entry.path.toLowerCase().includes(query)) {
    return true;
  }
  const children = childrenCache.get(entry.path);
  return children?.some((child) => treeNodeMatches(child, childrenCache, filterText)) ?? false;
}

function SearchIcon() {
  return (
    <svg className="filetree-search-icon" viewBox="0 0 20 20" aria-hidden="true">
      <circle cx="8.5" cy="8.5" r="5.2" />
      <path d="m12.4 12.4 4 4" />
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
