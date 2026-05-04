import type { SessionSummary, WorkspaceDescriptor } from "../../types";

interface Props {
  session: SessionSummary;
  workspace: WorkspaceDescriptor;
  activeTabLabel: string;
  changeCount: number;
}

export function ThreadHeader({
  session,
  workspace,
  activeTabLabel,
  changeCount,
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
        <span className="thread-header-count">{changeCount} changes</span>
        <StatusPill status={session.status} />
      </div>
    </header>
  );
}

function StatusPill({ status }: { status: string }) {
  const config: Record<string, { label: string; className: string }> = {
    Idle: { label: "Idle", className: "status-idle" },
    Streaming: { label: "Replying", className: "status-streaming" },
    WaitingForTool: { label: "Using tools", className: "status-waiting" },
    Interrupted: { label: "Interrupted", className: "status-interrupted" },
  };
  const { label, className } = config[status] || {
    label: status,
    className: "status-idle",
  };

  return <span className={`status-pill ${className}`}>{label}</span>;
}
