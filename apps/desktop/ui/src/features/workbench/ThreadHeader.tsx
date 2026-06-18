import type { ReactNode } from "react";
import type { SessionSummary, WorkspaceDescriptor } from "../../types";

interface Props {
  session: SessionSummary;
  workspace: WorkspaceDescriptor;
  activeTabLabel: string;
  changeCount: number;
  planToggle?: ReactNode;
}

export function ThreadHeader({
  session,
  workspace,
  activeTabLabel,
  changeCount,
  planToggle,
}: Props) {
  return (
    <header className="thread-header">
      <div className="thread-header-main">
        <div className="thread-header-kicker">
          <span>{workspace.name}</span>
          <span>{session.model}</span>
          {session.mode && <span>{session.mode}</span>}
          <span>{activeTabLabel}</span>
        </div>
        <h1 className="thread-header-title" title={session.title}>
          {session.title}
        </h1>
      </div>
      <div className="thread-header-actions">
        <span className="thread-header-count">{changeCount} 处更改</span>
        {planToggle}
        <StatusPill status={session.status} />
      </div>
    </header>
  );
}

function StatusPill({ status }: { status: string }) {
  const config: Record<string, { label: string; className: string }> = {
    Idle: { label: "空闲", className: "status-idle" },
    Streaming: { label: "回复中", className: "status-streaming" },
    WaitingForTool: { label: "使用工具中", className: "status-waiting" },
    Interrupted: { label: "已中断", className: "status-interrupted" },
  };
  const { label, className } = config[status] || {
    label: status,
    className: "status-idle",
  };

  return <span className={`status-pill ${className}`}>{label}</span>;
}
