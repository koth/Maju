import { useState, useMemo } from "react";
import type { FileChangeSummary } from "../../types";
import "./ChangesBar.css";

interface Props {
  changeSetId: string;
  changes: FileChangeSummary[];
  onFileSelect: (path: string, changeSetId: string) => void;
  onReviewClick?: (changeSetId: string) => void;
}

export function ChangesBar({ changeSetId, changes, onFileSelect, onReviewClick }: Props) {
  const [expanded, setExpanded] = useState(true);

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
      <div className="changes-bar-header">
        <span className="changes-bar-icon" aria-hidden="true">
          <svg viewBox="0 0 20 20" focusable="false">
            <path d="M4.5 3.5h11v13h-11z" />
            <path d="M7.2 8.2h5.6M10 5.8v5.6M7.6 13.2h4.8" />
          </svg>
        </span>
        <div className="changes-bar-summary">
          <span className="changes-bar-label">
            已编辑 {sorted.length} 个文件
          </span>
          <span className="changes-bar-totals">
            <span className="changes-bar-added">+{totalAdded}</span>
            <span className="changes-bar-removed">-{totalRemoved}</span>
          </span>
        </div>
        <div className="changes-bar-actions">
        <button
          type="button"
          className="changes-bar-action changes-bar-action-muted"
          title="撤销功能暂未接入"
          disabled
        >
          撤销 ↶
        </button>
        <button
          type="button"
          className="changes-bar-action changes-bar-review-action"
          title="打开审查"
          onClick={() => onReviewClick?.(changeSetId)}
        >
          审核
        </button>
        <button
          type="button"
          className="changes-bar-action changes-bar-icon-action"
          title={expanded ? "收起更改列表" : "展开更改列表"}
          onClick={() => setExpanded((v) => !v)}
        >
          <span className={`changes-bar-chevron ${expanded ? "open" : ""}`}>
            ›
          </span>
        </button>
        </div>
      </div>

      {expanded && (
        <div className="changes-bar-list">
          {sorted.map((change) => (
            <div
              key={change.path}
              className="changes-bar-row"
              onClick={() => onFileSelect(change.path, changeSetId)}
            >
              <span className="changes-bar-path">{change.path}</span>
              <span className="changes-bar-stats">
                <span className="changes-bar-added">+{change.added_lines}</span>
                <span className="changes-bar-removed">-{change.removed_lines}</span>
              </span>
              <span className="changes-bar-row-chevron">⌄</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
