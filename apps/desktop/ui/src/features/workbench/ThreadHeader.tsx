import type { ReactNode } from "react";
import type { SessionSummary } from "../../types";

interface Props {
  session: SessionSummary;
  planToggle?: ReactNode;
}

export function ThreadHeader({
  session,
  planToggle,
}: Props) {
  return (
    <header className="thread-header">
      <div className="thread-header-main">
        <h1 className="thread-header-title" title={session.title}>
          {session.title}
        </h1>
      </div>
      {planToggle && <div className="thread-header-actions">{planToggle}</div>}
    </header>
  );
}
