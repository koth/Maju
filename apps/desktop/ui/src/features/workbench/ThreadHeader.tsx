import type { ReactNode } from "react";
import type { SessionSummary } from "../../types";
import { formatAgentLabel } from "../../lib/agent-label";

interface Props {
  session: SessionSummary;
  actions?: ReactNode;
}

export function ThreadHeader({
  session,
  actions,
}: Props) {
  // The agent lives here rather than on every sidebar row: one quiet mention
  // next to the title is enough to say which agent this conversation runs with.
  const agentLabel = formatAgentLabel(session.agent_cli);
  return (
    <header className="thread-header">
      <div className="thread-header-main">
        <h1
          className="thread-header-title"
          title={agentLabel ? `${session.title} (${agentLabel})` : session.title}
        >
          {session.title}
          {agentLabel && <span className="thread-header-agent">({agentLabel})</span>}
        </h1>
      </div>
      {actions && <div className="thread-header-actions">{actions}</div>}
    </header>
  );
}
