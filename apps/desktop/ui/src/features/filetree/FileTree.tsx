import { useState, useEffect, useCallback } from "react";
import type { FileEntry } from "../../types";
import { fsListDir } from "../../lib/tauri";
import { getFileIcon, getFolderIcon } from "./file-icons";
import "./FileTree.css";

interface FileTreeProps {
  onFileOpen: (filePath: string) => void;
  refreshSignal?: number;
}

interface SelectedEntry {
  path: string;
  kind: FileEntry["kind"];
}

interface TreeNodeProps {
  entry: FileEntry;
  depth: number;
  expandedDirs: Set<string>;
  childrenCache: Map<string, FileEntry[]>;
  selectedPath: string | null;
  onToggleDir: (path: string) => void;
  onSelect: (entry: FileEntry) => void;
  onFileOpen: (filePath: string) => void;
}

function TreeNode({
  entry,
  depth,
  expandedDirs,
  childrenCache,
  selectedPath,
  onToggleDir,
  onSelect,
  onFileOpen,
}: TreeNodeProps) {
  const isDir = entry.kind === "Directory";
  const isExpanded = expandedDirs.has(entry.path);
  const isSelected = selectedPath === entry.path;
  const children = childrenCache.get(entry.path);
  const icon = isDir ? getFolderIcon(entry.name, isExpanded) : getFileIcon(entry.path);

  const handleClick = useCallback(() => {
    onSelect(entry);
    if (isDir) {
      onToggleDir(entry.path);
    } else {
      onFileOpen(entry.path);
    }
  }, [entry, isDir, onFileOpen, onSelect, onToggleDir]);

  return (
    <>
      <div
        className={`filetree-node ${isDir ? "filetree-dir" : "filetree-file"} ${isSelected ? "is-selected" : ""}`}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={handleClick}
        title={entry.path}
      >
        <span className="filetree-chevron">
          {isDir ? (isExpanded ? "\u25BE" : "\u25B8") : " "}
        </span>
        <img className="filetree-icon" src={icon} alt="" />
        <span className="filetree-name">{entry.name}</span>
      </div>
      {isDir && isExpanded && children && (
        <div className="filetree-children">
          {children.map((child) => (
            <TreeNode
              key={child.path}
              entry={child}
              depth={depth + 1}
              expandedDirs={expandedDirs}
              childrenCache={childrenCache}
              selectedPath={selectedPath}
              onToggleDir={onToggleDir}
              onSelect={onSelect}
              onFileOpen={onFileOpen}
            />
          ))}
        </div>
      )}
    </>
  );
}

export function FileTree({ onFileOpen, refreshSignal = 0 }: FileTreeProps) {
  const [rootEntries, setRootEntries] = useState<FileEntry[]>([]);
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = useState<Map<string, FileEntry[]>>(
    new Map()
  );
  const [selectedEntry, setSelectedEntry] = useState<SelectedEntry | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshDirectory = useCallback(async (dirPath: string) => {
    setError(null);
    const entries = await fsListDir(dirPath);
    if (dirPath) {
      setChildrenCache((prev) => new Map(prev).set(dirPath, entries));
    } else {
      setRootEntries(entries);
    }
  }, []);

  useEffect(() => {
    refreshDirectory("").catch((e) => setError(String(e)));
  }, [refreshDirectory]);

  useEffect(() => {
    if (refreshSignal === 0) return;

    const dirPath = selectedEntry
      ? selectedEntry.kind === "Directory"
        ? selectedEntry.path
        : parentDirectory(selectedEntry.path)
      : "";

    refreshDirectory(dirPath)
      .then(() => {
        if (dirPath) {
          setExpandedDirs((prev) => new Set(prev).add(dirPath));
        }
      })
      .catch((e) => setError(String(e)));
  }, [refreshDirectory, refreshSignal, selectedEntry]);

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

  if (error) {
    return <div className="filetree-error">{error}</div>;
  }

  return (
    <div className="filetree">
      <div className="filetree-header">所有文件</div>
      <div className="filetree-list">
        {rootEntries.map((entry) => (
          <TreeNode
            key={entry.path}
            entry={entry}
            depth={0}
            expandedDirs={expandedDirs}
            childrenCache={childrenCache}
            selectedPath={selectedEntry?.path ?? null}
            onToggleDir={handleToggleDir}
            onSelect={handleSelect}
            onFileOpen={onFileOpen}
          />
        ))}
      </div>
    </div>
  );
}

function parentDirectory(path: string) {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}
