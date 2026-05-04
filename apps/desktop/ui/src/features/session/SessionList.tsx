import { useState, useEffect, useCallback } from "react";
import type { KeyboardEvent } from "react";
import type { SessionListItem, WorkspaceDescriptor } from "../../types";
import { sessionList, sessionSwitch, sessionCreate, sessionDelete } from "../../lib/tauri";
import "./SessionList.css";

interface Props {
  activeSessionId: string;
  workspace: WorkspaceDescriptor;
  onSessionChanged: () => void;
}

interface SessionGroup {
  label: string;
  sessions: SessionListItem[];
}

export function SessionList({ activeSessionId, workspace, onSessionChanged }: Props) {
  const [sessions, setSessions] = useState<SessionListItem[]>([]);

  const refresh = useCallback(() => {
    sessionList().then(setSessions).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 3000);
    return () => clearInterval(interval);
  }, [refresh]);

  const handleSwitch = useCallback(
    async (id: string) => {
      if (id === activeSessionId) return;
      try {
        await sessionSwitch(id);
        onSessionChanged();
      } catch {
        // ignore
      }
    },
    [activeSessionId, onSessionChanged],
  );

  const handleCreate = useCallback(async () => {
    try {
      await sessionCreate();
      onSessionChanged();
      refresh();
    } catch {
      // ignore
    }
  }, [onSessionChanged, refresh]);

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await sessionDelete(id);
        refresh();
      } catch {
        // ignore
      }
    },
    [refresh],
  );

  const groups = groupSessions(sessions);

  return (
    <div className="session-list">
      <div className="sl-header">
        <div className="sl-heading">
          <span className="sl-kicker">Navigator</span>
          <span className="sl-title">Workspaces</span>
        </div>
        <button className="sl-new-btn" type="button" onClick={handleCreate}>
          New
        </button>
      </div>

      <div className="sl-workspace-node" aria-current="true">
        <span className="sl-workspace-mark">W</span>
        <div className="sl-workspace-copy">
          <span className="sl-workspace-name" title={workspace.name}>{workspace.name}</span>
          <span className="sl-workspace-path" title={workspace.root}>{workspace.root}</span>
        </div>
        <span className="sl-workspace-count">{sessions.length}</span>
      </div>

      <div className="sl-thread-branch">
        <div className="sl-thread-branch-title">
          <span>Threads</span>
          <span>Updated by recent activity</span>
        </div>

        <div className="sl-items">
        {sessions.length === 0 && (
          <div className="sl-empty">
            <span className="sl-empty-title">No threads yet</span>
            <span className="sl-empty-copy">Start a thread inside this workspace.</span>
          </div>
        )}

        {groups.map((group) => (
          <section className="sl-section" key={group.label}>
            <div className="sl-section-title">{group.label}</div>
            {group.sessions.map((session) => (
              <ThreadRow
                key={session.id}
                session={session}
                active={session.id === activeSessionId}
                onSwitch={handleSwitch}
                onDelete={handleDelete}
              />
            ))}
          </section>
        ))}
        </div>
      </div>
    </div>
  );
}

function ThreadRow({
  session,
  active,
  onSwitch,
  onDelete,
}: {
  session: SessionListItem;
  active: boolean;
  onSwitch: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onSwitch(session.id);
    }
  };

  return (
    <div
      className={`sl-item ${active ? "sl-active" : ""}`}
      onClick={() => onSwitch(session.id)}
      onKeyDown={handleKeyDown}
      role="button"
      tabIndex={0}
      aria-current={active ? "page" : undefined}
    >
      <div className="sl-item-main">
        <div className="sl-item-title" title={session.title}>{session.title}</div>
        <div className="sl-item-meta">
          <span>{formatRelativeTime(session.updated_at || session.created_at)}</span>
          <span>{session.message_count} messages</span>
        </div>
      </div>
      <div className="sl-item-side">
        <span className={`sl-status-dot sl-status-${session.status.toLowerCase()}`} />
        {!active && (
          <button
            className="sl-delete-btn"
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              onDelete(session.id);
            }}
            title="Delete session"
          >
            x
          </button>
        )}
      </div>
    </div>
  );
}

function groupSessions(sessions: SessionListItem[]): SessionGroup[] {
  const sorted = [...sessions].sort((a, b) => {
    return getTimestamp(b.updated_at || b.created_at) - getTimestamp(a.updated_at || a.created_at);
  });
  const buckets: SessionGroup[] = [
    { label: "Today", sessions: [] },
    { label: "Previous 7 days", sessions: [] },
    { label: "Earlier", sessions: [] },
  ];

  sorted.forEach((session) => {
    const days = daysSince(session.updated_at || session.created_at);
    if (days < 1) {
      buckets[0].sessions.push(session);
    } else if (days <= 7) {
      buckets[1].sessions.push(session);
    } else {
      buckets[2].sessions.push(session);
    }
  });

  return buckets.filter((bucket) => bucket.sessions.length > 0);
}

function formatRelativeTime(value: string) {
  const timestamp = getTimestamp(value);
  if (!timestamp) return "Recently";

  const diffMs = Date.now() - timestamp;
  const minutes = Math.max(0, Math.floor(diffMs / 60000));
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  if (hours < 48) return "Yesterday";

  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;

  return new Date(timestamp).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function daysSince(value: string) {
  const timestamp = getTimestamp(value);
  if (!timestamp) return 0;
  return Math.floor((Date.now() - timestamp) / 86400000);
}

function getTimestamp(value: string) {
  const timestamp = new Date(value).getTime();
  return Number.isFinite(timestamp) ? timestamp : 0;
}
