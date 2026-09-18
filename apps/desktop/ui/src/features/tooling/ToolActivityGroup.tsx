// Collapsed activity run in the conversation timeline (Codex-style): one
// summary row per contiguous run of tool calls, expanding to the individual
// rows on click.

import { memo, useState } from "react";
import type { ReactNode } from "react";
import type { ToolInvocation } from "../../types";
import { statusBullet } from "./tool-card-analysis";
import type { ToolActivityGroup } from "./tool-activity";
import "./ToolActivityGroup.css";

interface Props {
  group: ToolActivityGroup;
  /// Renders one member row. Supplied by the timeline so the expanded group
  /// produces exactly the rows it would have rendered on its own.
  renderTool: (tool: ToolInvocation) => ReactNode;
}

function ToolActivityGroupImpl({ group, renderTool }: Props) {
  const [expanded, setExpanded] = useState(false);
  const running = group.tools.some(
    (tool) => tool.status === "Running" || tool.status === "Pending",
  );
  // Finished runs get no marker: the summary text ("已运行 ×3 · 已编辑 1 个
  // 文件") already carries the state, so a dot in front of every collapsed run
  // is noise. A run still in flight keeps the live bullet.
  const bullet = statusBullet(running ? "Running" : "Succeeded");

  return (
    <div className={`tool-activity-group${expanded ? " is-expanded" : ""}`}>
      <button
        type="button"
        className="tool-activity-summary"
        aria-expanded={expanded}
        aria-label={expanded ? `收起${group.summary}` : `展开${group.summary}`}
        onClick={() => setExpanded((value) => !value)}
      >
        {bullet && (
          <span className={`tc-bullet ${bullet.className}`} aria-hidden="true">
            {bullet.char}
          </span>
        )}
        <span className="tool-activity-label">{group.summary}</span>
        <span className="tool-activity-chevron" aria-hidden="true">
          {"\u203A"}
        </span>
      </button>
      {expanded && (
        <div className="tool-activity-content">
          {group.tools.map((tool) => (
            <div key={tool.id} className="tool-activity-row">
              {renderTool(tool)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export const ToolActivityGroupRow = memo(ToolActivityGroupImpl);
