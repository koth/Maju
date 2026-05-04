import { useState, useMemo } from "react";
import type { SessionFileChange, FileChangeType } from "../../types";
import "./ChangesBar.css";

interface Props {
  changes: SessionFileChange[];
  onFileSelect: (path: string) => void;
}

export function ChangesBar({ changes, onFileSelect }: Props) {
  const [expanded, setExpanded] = useState(false);

  const sorted = useMemo(
    () => [...changes].sort((a, b) => a.path.localeCompare(b.path)),
    [changes],
  );

  const totalAdded = useMemo(
    () => sorted.reduce((sum, c) => sum + c.added_lines, 0),
    [sorted],
  );
  const totalRemoved = useMemo(
    () => sorted.reduce((sum, c) => sum + c.removed_lines, 0),
    [sorted],
  );

  if (sorted.length === 0) return null;

  return (
    <div className="changes-bar">
      <div
        className="changes-bar-header"
        onClick={() => setExpanded((v) => !v)}
      >
        <span className={`changes-bar-chevron ${expanded ? "open" : ""}`}>
          ›
        </span>
        <span className="changes-bar-label">
          Changes: {sorted.length} file{sorted.length !== 1 ? "s" : ""}
        </span>
        <span className="changes-bar-totals">
          {totalAdded > 0 && (
            <span className="changes-bar-added">+{totalAdded}</span>
          )}
          {totalRemoved > 0 && (
            <span className="changes-bar-removed">-{totalRemoved}</span>
          )}
        </span>
      </div>

      {expanded && (
        <div className="changes-bar-list">
          {sorted.map((change) => (
            <div
              key={change.path}
              className="changes-bar-row"
              onClick={() => onFileSelect(change.path)}
            >
              <ChangeTypeBadge type={change.change_type} />
              <span className="changes-bar-path">{fileName(change.path)}</span>
              <span className="changes-bar-stats">
                {change.added_lines > 0 && (
                  <span className="changes-bar-added">+{change.added_lines}</span>
                )}
                {change.removed_lines > 0 && (
                  <span className="changes-bar-removed">-{change.removed_lines}</span>
                )}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function ChangeTypeBadge({ type }: { type: FileChangeType }) {
  const config: Record<FileChangeType, { label: string; className: string }> = {
    Created: { label: "A", className: "badge-created" },
    Modified: { label: "M", className: "badge-modified" },
    Deleted: { label: "D", className: "badge-deleted" },
  };
  const { label, className } = config[type];
  return <span className={`changes-bar-badge ${className}`}>{label}</span>;
}

function fileName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}
