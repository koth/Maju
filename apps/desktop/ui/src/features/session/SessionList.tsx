import { useState, useEffect, useCallback } from "react";
import type { KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import type { AgentCliId, AgentSettingsSnapshot, SessionListItem, SessionStatus, UiSnapshot, WorkspaceSessionList } from "../../types";
import {
  sessionList,
  sessionSwitch,
  sessionCreate,
  sessionDelete,
  sessionCancel,
  settingsGetAgentSnapshot,
  workspaceOpen,
  workspaceSetActive,
} from "../../lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import "./SessionList.css";

interface Props {
  activeSessionId: string;
  activeWorkspaceRoot: string;
  currentSessionStatus: SessionStatus;
  onSessionChanged: () => void;
  onWorkspaceChanged: (snapshot: UiSnapshot) => void;
}

interface SessionGroup {
  label: string;
  sessions: SessionListItem[];
}

type AgentModalMode = "workspace" | "session";

interface PendingSwitch {
  id: string;
  workspaceRoot: string;
}

export function SessionList({
  activeSessionId,
  activeWorkspaceRoot,
  currentSessionStatus,
  onSessionChanged,
  onWorkspaceChanged,
}: Props) {
  const [workspaceSessions, setWorkspaceSessions] = useState<WorkspaceSessionList[]>([]);
  const [agentSnapshot, setAgentSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [selectedAgent, setSelectedAgent] = useState<AgentCliId | null>(null);
  const [pendingWorkspaceRoot, setPendingWorkspaceRoot] = useState<string | null>(null);
  const [pendingWorkspacePath, setPendingWorkspacePath] = useState<string | null>(null);
  const [modalMode, setModalMode] = useState<AgentModalMode | null>(null);
  const [modalError, setModalError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [pendingSwitch, setPendingSwitch] = useState<PendingSwitch | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [isSwitching, setIsSwitching] = useState(false);
  const [agentDropdownOpen, setAgentDropdownOpen] = useState(false);

  const refresh = useCallback(() => {
    sessionList().then(setWorkspaceSessions).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 3000);
    return () => clearInterval(interval);
  }, [refresh]);

  const openAgentModal = useCallback(async (mode: AgentModalMode) => {
    setModalError(null);
    setAgentDropdownOpen(false);
    const snapshot = await settingsGetAgentSnapshot();
    const defaultAgent = snapshot.settings.selected_agent;
    const defaultInstalled = snapshot.agents.some((agent) => agent.id === defaultAgent && agent.installed);
    const fallbackAgent = snapshot.agents.find((agent) => agent.installed)?.id ?? defaultAgent;
    setAgentSnapshot(snapshot);
    setSelectedAgent(defaultInstalled ? defaultAgent : fallbackAgent);
    setModalMode(mode);
    setAgentDropdownOpen(false);
  }, []);

  const handleCreateWorkspace = useCallback(async () => {
    try {
      setPendingWorkspacePath(null);
      setPendingWorkspaceRoot(null);
      await openAgentModal("workspace");
    } catch (error) {
      setModalError(String(error));
    }
  }, [openAgentModal]);

  const handleChooseWorkspaceDirectory = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected) return;
      setPendingWorkspacePath(selected as string);
    } catch (error) {
      setModalError(String(error));
    }
  }, []);

  const handleActivateWorkspace = useCallback(
    async (workspaceRoot: string) => {
      try {
        const nextSnapshot = await workspaceSetActive(workspaceRoot);
        onWorkspaceChanged(nextSnapshot);
        refresh();
      } catch {
        // ignore
      }
    },
    [onWorkspaceChanged, refresh],
  );

  const handleSwitch = useCallback(
    async (id: string, workspaceRoot: string) => {
      if (id === activeSessionId && workspaceRoot === activeWorkspaceRoot) return;
      const sameWorkspace = workspaceRoot === activeWorkspaceRoot;
      if (sameWorkspace && currentSessionStatus !== "Idle") {
        setPendingSwitch({ id, workspaceRoot });
        setSwitchError(null);
        return;
      }
      try {
        await sessionSwitch(id, workspaceRoot);
        onSessionChanged();
        refresh();
      } catch {
        // ignore
      }
    },
    [activeSessionId, activeWorkspaceRoot, currentSessionStatus, onSessionChanged, refresh],
  );

  const handleOpenAgentPicker = useCallback(
    async (workspaceRoot: string) => {
      try {
        setPendingWorkspaceRoot(workspaceRoot);
        setPendingWorkspacePath(null);
        await openAgentModal("session");
      } catch (error) {
        setModalError(String(error));
      }
    },
    [openAgentModal],
  );

  const closeAgentPicker = useCallback(() => {
    if (isSubmitting) return;
    setPendingWorkspaceRoot(null);
    setPendingWorkspacePath(null);
    setAgentSnapshot(null);
    setSelectedAgent(null);
    setModalMode(null);
    setModalError(null);
    setAgentDropdownOpen(false);
  }, [isSubmitting]);

  const handleConfirmAgentModal = useCallback(async () => {
    if (!selectedAgent || !modalMode) return;
    try {
      setIsSubmitting(true);
      setModalError(null);
      if (modalMode === "workspace") {
        if (!pendingWorkspacePath) return;
        const nextSnapshot = await workspaceOpen(pendingWorkspacePath, selectedAgent);
        onWorkspaceChanged(nextSnapshot);
      } else {
        if (!pendingWorkspaceRoot) return;
        await sessionCreate(pendingWorkspaceRoot, selectedAgent);
        onSessionChanged();
      }
      setPendingWorkspaceRoot(null);
      setPendingWorkspacePath(null);
      setAgentSnapshot(null);
      setSelectedAgent(null);
      setModalMode(null);
      setAgentDropdownOpen(false);
      refresh();
    } catch (error) {
      setModalError(String(error));
    } finally {
      setIsSubmitting(false);
    }
  }, [modalMode, onSessionChanged, onWorkspaceChanged, pendingWorkspacePath, pendingWorkspaceRoot, refresh, selectedAgent]);

  const handleDelete = useCallback(
    async (id: string, workspaceRoot: string) => {
      try {
        await sessionDelete(id, workspaceRoot);
        refresh();
      } catch {
        // ignore
      }
    },
    [refresh],
  );

  const closeSwitchConfirm = useCallback(() => {
    if (isSwitching) return;
    setPendingSwitch(null);
    setSwitchError(null);
  }, [isSwitching]);

  const confirmStopAndSwitch = useCallback(async () => {
    if (!pendingSwitch) return;
    try {
      setIsSwitching(true);
      setSwitchError(null);
      await sessionCancel();
      await sessionSwitch(pendingSwitch.id, pendingSwitch.workspaceRoot);
      setPendingSwitch(null);
      onSessionChanged();
      refresh();
    } catch (error) {
      setSwitchError(String(error));
    } finally {
      setIsSwitching(false);
    }
  }, [onSessionChanged, pendingSwitch, refresh]);

  const modalTitle = modalMode === "workspace" ? "创建工作区" : "创建会话";
  const modalButtonText = modalMode === "workspace" ? "创建工作区" : "创建会话";
  const loadingText = modalMode === "workspace" ? "正在创建工作区..." : "正在创建会话...";
  const selectedAgentStatus = agentSnapshot?.agents.find((agent) => agent.id === selectedAgent) ?? null;

  return (
    <div className="session-list">
      <div className="sl-header">
        <div className="sl-heading">
          <span className="sl-kicker">导航</span>
          <span className="sl-title">工作区</span>
        </div>
        <button className="sl-new-btn" type="button" onClick={handleCreateWorkspace}>
          新建
        </button>
      </div>

      <div className="sl-workspaces">
        {workspaceSessions.length === 0 && (
          <div className="sl-empty">
            <span className="sl-empty-title">暂无工作区</span>
            <span className="sl-empty-copy">点击新建打开一个工作区。</span>
          </div>
        )}

        {workspaceSessions.map((workspaceItem) => (
          <WorkspaceSection
            key={workspaceItem.workspace.root}
            item={workspaceItem}
            activeSessionId={activeSessionId}
            onActivateWorkspace={handleActivateWorkspace}
            onCreateSession={handleOpenAgentPicker}
            onSwitch={handleSwitch}
            onDelete={handleDelete}
          />
        ))}
      </div>

      {pendingSwitch && createPortal(
        <div className="sl-agent-modal-backdrop" role="presentation" onClick={closeSwitchConfirm}>
          <div className="sl-agent-modal sl-switch-modal" role="dialog" aria-modal="true" onClick={(event) => event.stopPropagation()}>
            <div className="sl-agent-modal-header">
              <span>当前会话仍在运行</span>
              <button type="button" className="sl-agent-close" onClick={closeSwitchConfirm} disabled={isSwitching}>x</button>
            </div>
            <div className="sl-switch-copy">
              切换同一工作区内的其他会话会中断当前任务。你可以继续等待，或停止当前任务并切换。
            </div>
            {switchError && <div className="sl-agent-error">{switchError}</div>}
            <div className="sl-agent-actions">
              <button type="button" className="sl-agent-cancel" onClick={closeSwitchConfirm} disabled={isSwitching}>继续等待</button>
              <button type="button" className="sl-agent-create sl-switch-danger" onClick={confirmStopAndSwitch} disabled={isSwitching}>
                {isSwitching && <span className="sl-agent-spinner" aria-hidden="true" />}
                {isSwitching ? "正在停止..." : "停止并切换"}
              </button>
            </div>
          </div>
        </div>,
        document.body,
      )}

      {modalMode && agentSnapshot && createPortal(
        <div className="sl-agent-modal-backdrop" role="presentation" onClick={closeAgentPicker}>
          <div className="sl-agent-modal" role="dialog" aria-modal="true" onClick={(event) => event.stopPropagation()}>
            <div className="sl-agent-modal-header">
              <span>{modalTitle}</span>
              <button type="button" className="sl-agent-close" onClick={closeAgentPicker} disabled={isSubmitting}>x</button>
            </div>

            {modalMode === "workspace" && (
              <div className="sl-workspace-create-form">
                <label className="sl-form-field">
                  <span className="sl-form-label">目录</span>
                  <span className="sl-directory-row">
                    <span className={`sl-directory-value ${pendingWorkspacePath ? "" : "is-placeholder"}`} title={pendingWorkspacePath ?? ""}>
                      {pendingWorkspacePath ?? "请选择工作区目录"}
                    </span>
                    <button type="button" className="sl-directory-btn" onClick={handleChooseWorkspaceDirectory} disabled={isSubmitting}>
                      选择...
                    </button>
                  </span>
                </label>
                <label className="sl-form-field">
                  <span className="sl-form-label">Agent</span>
                  <div className="sl-agent-dropdown">
                    <button
                      type="button"
                      className={`sl-agent-select-btn ${agentDropdownOpen ? "is-open" : ""}`}
                      disabled={isSubmitting}
                      onClick={() => setAgentDropdownOpen((open) => !open)}
                    >
                      <span className="sl-agent-select-main">
                        {selectedAgentStatus?.label ?? "请选择 Agent"}
                        {selectedAgentStatus?.id === agentSnapshot.settings.selected_agent && <span>Settings 默认</span>}
                      </span>
                      <span className="sl-agent-select-chevron">⌄</span>
                    </button>
                    {agentDropdownOpen && (
                      <div className="sl-agent-dropdown-menu">
                        {agentSnapshot.agents.map((agent) => (
                          <button
                            type="button"
                            className={`sl-agent-dropdown-item ${selectedAgent === agent.id ? "is-selected" : ""}`}
                            disabled={!agent.installed}
                            key={agent.id}
                            onClick={() => {
                              setSelectedAgent(agent.id);
                              setAgentDropdownOpen(false);
                            }}
                          >
                            <span>{agent.label}</span>
                            <small>
                              {agent.id === agentSnapshot.settings.selected_agent ? "Settings 默认" : agent.binary}
                              {!agent.installed ? " · 未安装" : ""}
                            </small>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                </label>
              </div>
            )}

            {modalMode === "session" && (
              <div className="sl-agent-options">
                {agentSnapshot.agents.map((agent) => (
                  <label className={`sl-agent-option ${selectedAgent === agent.id ? "is-selected" : ""} ${!agent.installed ? "is-disabled" : ""}`} key={agent.id}>
                    <input
                      type="radio"
                      name="agent"
                      value={agent.id}
                      checked={selectedAgent === agent.id}
                      disabled={!agent.installed || isSubmitting}
                      onChange={() => setSelectedAgent(agent.id)}
                    />
                    <span className="sl-agent-label">{agent.label}</span>
                    <span className="sl-agent-meta">
                      {agent.id === agentSnapshot.settings.selected_agent ? "Settings 默认" : agent.binary}
                      {!agent.installed ? " · 未安装" : ""}
                    </span>
                  </label>
                ))}
              </div>
            )}
            {agentSnapshot.env_override && (
              <div className="sl-agent-note">已设置 ACP_AGENT_COMMAND；本次选择会直接使用所选 Agent。</div>
            )}
            {modalError && <div className="sl-agent-error">{modalError}</div>}
            <div className="sl-agent-actions">
              <button type="button" className="sl-agent-cancel" onClick={closeAgentPicker} disabled={isSubmitting}>取消</button>
              <button type="button" className="sl-agent-create" onClick={handleConfirmAgentModal} disabled={!selectedAgent || (modalMode === "workspace" && !pendingWorkspacePath) || isSubmitting}>
                {isSubmitting && <span className="sl-agent-spinner" aria-hidden="true" />}
                {isSubmitting ? loadingText : modalButtonText}
              </button>
            </div>
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}

function WorkspaceSection({
  item,
  activeSessionId,
  onActivateWorkspace,
  onCreateSession,
  onSwitch,
  onDelete,
}: {
  item: WorkspaceSessionList;
  activeSessionId: string;
  onActivateWorkspace: (workspaceRoot: string) => void;
  onCreateSession: (workspaceRoot: string) => void;
  onSwitch: (id: string, workspaceRoot: string) => void;
  onDelete: (id: string, workspaceRoot: string) => void;
}) {
  const groups = groupSessions(item.sessions);
  const workspaceRoot = item.workspace.root;

  return (
    <section className={`sl-workspace-section ${item.is_active ? "is-active" : ""} ${item.connected ? "is-connected" : "is-dormant"}`}>
      <button className="sl-workspace-node" type="button" aria-current={item.is_active ? "true" : undefined} onClick={() => onActivateWorkspace(workspaceRoot)}>
        <span className="sl-workspace-mark">W</span>
        <span className="sl-workspace-copy">
          <span className="sl-workspace-name" title={item.workspace.name}>{item.workspace.name}</span>
          <span className="sl-workspace-path" title={workspaceRoot}>{workspaceRoot}</span>
        </span>
        <span className="sl-workspace-side">
          <span className="sl-workspace-state">{item.connected ? "已连接" : "未连接"}</span>
          <span className="sl-workspace-count">{item.sessions.length}</span>
        </span>
      </button>

      <div className="sl-thread-branch">
        <div className="sl-thread-branch-title">
          <span>会话</span>
          <button className="sl-session-add" type="button" onClick={() => onCreateSession(workspaceRoot)} title="新建会话">
            +
          </button>
        </div>

        <div className="sl-items">
          {item.sessions.length === 0 && (
            <div className="sl-empty sl-session-empty">
              <span className="sl-empty-title">暂无会话</span>
              <span className="sl-empty-copy">在此工作区中开始一个会话。</span>
            </div>
          )}

          {groups.map((group) => (
            <section className="sl-section" key={group.label}>
              <div className="sl-section-title">{group.label}</div>
              {group.sessions.map((session) => (
                <ThreadRow
                  key={session.id}
                  session={session}
                  active={session.id === activeSessionId && item.is_active}
                  onSwitch={(id) => onSwitch(id, workspaceRoot)}
                  onDelete={(id) => onDelete(id, workspaceRoot)}
                />
              ))}
            </section>
          ))}
        </div>
      </div>
    </section>
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
          <span>{session.message_count} 条消息</span>
          {session.agent_cli && <span>{session.agent_cli}</span>}
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
            title="删除会话"
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
    { label: "今天", sessions: [] },
    { label: "过去 7 天", sessions: [] },
    { label: "更早", sessions: [] },
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
  if (!timestamp) return "最近";

  const diffMs = Date.now() - timestamp;
  const minutes = Math.max(0, Math.floor(diffMs / 60000));
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  if (hours < 48) return "昨天";

  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} 天前`;

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
