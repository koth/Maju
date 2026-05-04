import { useMemo } from "react";
import type { SessionFileChange, FileChangeType } from "../../types";
import "./ChangesPanel.css";

interface Props {
  changes: SessionFileChange[];
  onFileSelect: (path: string) => void;
}

export function ChangesPanel({ changes, onFileSelect }: Props) {
  const sorted = useMemo(
    () => [...changes].sort((a, b) => a.path.localeCompare(b.path)),
    [changes],
  );

  return (
    <div className="changes-panel">
      <div className="changes-header">
        Changes ({sorted.length})
      </div>

      {sorted.length === 0 ? (
        <div className="changes-empty">
          No files changed in this session
        </div>
      ) : (
        <div className="changes-list">
          {sorted.map((change) => (
            <div
              key={change.path}
              className="changes-row"
              onClick={() => onFileSelect(change.path)}
            >
              <ChangeTypeBadge type={change.change_type} />
              <span className="changes-path">{fileName(change.path)}</span>
              <div className="changes-stats">
                {change.added_lines > 0 && (
                  <span className="changes-stat-added">+{change.added_lines}</span>
                )}
                {change.removed_lines > 0 && (
                  <span className="changes-stat-removed">-{change.removed_lines}</span>
                )}
              </div>
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
  return <span className={`changes-badge ${className}`}>{label}</span>;
}

function fileName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}
