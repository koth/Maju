import { useState, useEffect, useCallback, useRef } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import { createPortal } from "react-dom";
import type { AgentCliId, AgentSettingsSnapshot, RemoteLinuxWorkspace, SessionListItem, SessionStatus, UiSnapshot, WorkspaceSessionList } from "../../types";
import {
  sessionList,
  sessionSwitch,
  sessionCreate,
  sessionArchive,
  sessionCancel,
  settingsGetAgentSnapshot,
  workspaceArchive,
  workspaceOpen,
  workspaceSetActive,
} from "../../lib/tauri";
import { onSessionStatus } from "../../lib/events";
import { open } from "@tauri-apps/plugin-dialog";
import { RemoteOpenPanel } from "../workbench/RemoteOpenPanel";
import "./SessionList.css";

interface Props {
  activeSessionId: string;
  activeSessionTitle: string;
  activeWorkspaceRoot: string;
  currentSessionStatus: SessionStatus;
  activeConversationVisible?: boolean;
  refreshToken?: number;
  onOpenSettings: () => void;
  onSessionChanged: () => void;
  onWorkspaceChanged: (snapshot: UiSnapshot) => void;
  onWorkspaceArchived?: (snapshot: UiSnapshot | null) => void;
  onSessionArchived?: (session: ArchivedSessionNotice) => void;
}

type AgentModalMode = "workspace" | "session";

export interface ArchivedSessionNotice {
  id: string;
  title: string;
  workspaceRoot: string;
}

interface PendingSwitch {
  id: string;
  workspaceRoot: string;
}

export function SessionList({
  activeSessionId,
  activeSessionTitle,
  activeWorkspaceRoot,
  currentSessionStatus,
  activeConversationVisible = true,
  refreshToken,
  onOpenSettings,
  onSessionChanged,
  onWorkspaceChanged,
  onWorkspaceArchived,
  onSessionArchived,
}: Props) {
  const [workspaceSessions, setWorkspaceSessions] = useState<WorkspaceSessionList[]>([]);
  const [agentSnapshot, setAgentSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [selectedAgent, setSelectedAgent] = useState<AgentCliId | null>(null);
  const [pendingWorkspaceRoot, setPendingWorkspaceRoot] = useState<string | null>(null);
  const [pendingWorkspacePath, setPendingWorkspacePath] = useState<string | null>(null);
  const [pendingRemoteAgent, setPendingRemoteAgent] = useState<AgentCliId | null>(null);
  const [pendingRemoteWorkspace, setPendingRemoteWorkspace] = useState(false);
  const [modalMode, setModalMode] = useState<AgentModalMode | null>(null);
  const [modalError, setModalError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [pendingSwitch, setPendingSwitch] = useState<PendingSwitch | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [isSwitching, setIsSwitching] = useState(false);
  const [sessionsRefreshing, setSessionsRefreshing] = useState(false);
  const [agentDropdownOpen, setAgentDropdownOpen] = useState(false);
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [remoteOpenVisible, setRemoteOpenVisible] = useState(false);
  const [remoteReconnect, setRemoteReconnect] = useState<RemoteLinuxWorkspace | null>(null);
  const agentDropdownRef = useRef<HTMLDivElement>(null);
  const workspaceMenuRef = useRef<HTMLDivElement>(null);
  const refreshInFlightRef = useRef(false);

  const refresh = useCallback(() => {
    if (refreshInFlightRef.current) return;
    refreshInFlightRef.current = true;
    setSessionsRefreshing(true);
    sessionList()
      .then(setWorkspaceSessions)
      .catch(() => {})
      .finally(() => {
        refreshInFlightRef.current = false;
        setSessionsRefreshing(false);
      });
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 3000);
    return () => clearInterval(interval);
  }, [refresh]);

  useEffect(() => {
    if (refreshToken === undefined) return;
    refresh();
  }, [refresh, refreshToken]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    onSessionStatus(() => {
      refresh();
    })
      .then((cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
      })
      .catch(() => {});

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  useEffect(() => {
    if (!activeSessionId || !activeSessionTitle) return;
    setWorkspaceSessions((current) =>
      current.map((workspaceItem) => {
        if (workspaceItem.workspace.root !== activeWorkspaceRoot) {
          return workspaceItem;
        }
        return {
          ...workspaceItem,
          sessions: workspaceItem.sessions.map((session) =>
            session.id === activeSessionId
              ? { ...session, title: activeSessionTitle, status: currentSessionStatus }
              : session,
          ),
        };
      }),
    );
  }, [activeSessionId, activeSessionTitle, activeWorkspaceRoot, currentSessionStatus]);

  useEffect(() => {
    if (!agentDropdownOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (agentDropdownRef.current?.contains(target)) return;
      setAgentDropdownOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setAgentDropdownOpen(false);
      }
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, [agentDropdownOpen]);

  useEffect(() => {
    if (!workspaceMenuOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (workspaceMenuRef.current?.contains(target)) return;
      setWorkspaceMenuOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setWorkspaceMenuOpen(false);
      }
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, [workspaceMenuOpen]);

  const openAgentModal = useCallback(async (mode: AgentModalMode, preferredAgent: AgentCliId | null = null) => {
    setModalError(null);
    setAgentDropdownOpen(false);
    setWorkspaceMenuOpen(false);
    const snapshot = await settingsGetAgentSnapshot();
    setAgentSnapshot(snapshot);
    setSelectedAgent(preferredAgent ?? defaultAgentForNewWork(snapshot));
    setModalMode(mode);
    setAgentDropdownOpen(false);
  }, []);

  const handleCreateLocalWorkspace = useCallback(async () => {
    try {
      setPendingWorkspacePath(null);
      setPendingWorkspaceRoot(null);
      setPendingRemoteAgent(null);
      setPendingRemoteWorkspace(false);
      await openAgentModal("workspace");
    } catch (error) {
      setModalError(String(error));
    }
  }, [openAgentModal]);

  const handleOpenRemoteWorkspace = useCallback(() => {
    setWorkspaceMenuOpen(false);
    setModalError(null);
    setRemoteReconnect(null);
    setRemoteOpenVisible(true);
  }, []);

  const closeRemoteOpen = useCallback(() => {
    setRemoteOpenVisible(false);
    setRemoteReconnect(null);
  }, []);

  const handleRemoteWorkspaceOpened = useCallback(
    (snapshot: UiSnapshot) => {
      setRemoteOpenVisible(false);
      setRemoteReconnect(null);
      onWorkspaceChanged(snapshot);
      refresh();
    },
    [onWorkspaceChanged, refresh],
  );

  const handleReconnectRemoteWorkspace = useCallback((remote: RemoteLinuxWorkspace) => {
    setWorkspaceMenuOpen(false);
    setModalError(null);
    setRemoteReconnect(remote);
    setRemoteOpenVisible(true);
  }, []);

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
      try {
        await sessionSwitch(id, workspaceRoot);
        onSessionChanged();
        refresh();
      } catch {
        // ignore
      }
    },
    [activeSessionId, activeWorkspaceRoot, onSessionChanged, refresh],
  );

  const handleOpenAgentPicker = useCallback(
    async (workspaceRoot: string, remoteAgent: AgentCliId | null = null, isRemoteWorkspace = false) => {
      try {
        setPendingWorkspaceRoot(workspaceRoot);
        setPendingWorkspacePath(null);
        setPendingRemoteAgent(remoteAgent);
        setPendingRemoteWorkspace(isRemoteWorkspace);
        await openAgentModal("session", remoteAgent);
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
    setPendingRemoteAgent(null);
    setPendingRemoteWorkspace(false);
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
      setPendingRemoteAgent(null);
      setPendingRemoteWorkspace(false);
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

  const handleArchive = useCallback(
    async (id: string, workspaceRoot: string, sessionTitle?: string) => {
      try {
        const label = sessionTitle || "此会话";
        await sessionArchive(id, workspaceRoot);
        onSessionArchived?.({ id, title: label, workspaceRoot });
        onSessionChanged();
        refresh();
      } catch (error) {
        console.error("Failed to archive session", error);
      }
    },
    [onSessionArchived, onSessionChanged, refresh],
  );

  const handleArchiveWorkspace = useCallback(
    async (workspaceRoot: string, isActive: boolean, workspaceName?: string) => {
      try {
        const label = workspaceName || "此项目";
        const accepted = await confirm(`确定归档项目 ${label}？归档后该项目及其所有会话将从列表中移除，数据仍保留在本地。`);
        if (!accepted) return;
        const nextSnapshot = await workspaceArchive(workspaceRoot);
        if (isActive) {
          onWorkspaceArchived?.(nextSnapshot);
        }
        refresh();
      } catch (error) {
        console.error("Failed to archive workspace", error);
      }
    },
    [onWorkspaceArchived, refresh],
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

  const handleSelectModalAgent = useCallback((agent: AgentCliId) => {
    setSelectedAgent(agent);
    setAgentDropdownOpen(false);
  }, []);

  const modalTitle = modalMode === "workspace" ? "创建工作区" : "创建会话";
  const modalButtonText = modalMode === "workspace" ? "创建工作区" : "创建会话";
  const loadingText = modalMode === "workspace" ? "正在创建工作区..." : "正在创建会话...";
  const selectedAgentStatus = agentSnapshot?.agents.find((agent) => agent.id === selectedAgent) ?? null;
  const selectedAgentDisabled =
    selectedAgentStatus ? agentDisabledForModal(selectedAgentStatus, pendingRemoteWorkspace) : false;
  const selectedClaudeProfile = agentSnapshot?.claude.profiles.find(
    (profile) => profile.id === agentSnapshot.claude.selected_profile_id,
  );
  const selectedClaudeSetupMessage =
    selectedAgent === "claude-agent-acp" && agentSnapshot && selectedClaudeProfile
      ? selectedClaudeProfile.requires_credential && !selectedClaudeProfile.configured
          ? `Claude ${selectedClaudeProfile.label} 需要先在设置中保存 ${selectedClaudeProfile.credential_label ?? "API key"}。`
          : null
      : null;

  return (
    <div className="session-list">
      <div className="sl-header">
        <span className="sl-kicker">项目</span>
        <div className="sl-new-workspace" ref={workspaceMenuRef}>
          <button
            className={`sl-new-btn ${workspaceMenuOpen ? "is-open" : ""}`}
            type="button"
            onClick={() => setWorkspaceMenuOpen((open) => !open)}
            title="新建工作区"
            aria-label="新建工作区"
            aria-haspopup="menu"
            aria-expanded={workspaceMenuOpen}
          >
            <PlusIcon />
          </button>
          {workspaceMenuOpen && (
            <div className="sl-new-menu" role="menu" aria-label="新建工作区选项">
              <button type="button" role="menuitem" className="sl-new-menu-item" onClick={handleCreateLocalWorkspace}>
                <span>打开本地文件夹</span>
                <small>从这台机器选择目录</small>
              </button>
              <button type="button" role="menuitem" className="sl-new-menu-item" onClick={handleOpenRemoteWorkspace}>
                <span>打开远程目录</span>
                <small>使用已保存的 Linux 机器</small>
              </button>
            </div>
          )}
        </div>
      </div>

      <div className="sl-workspaces">
        {workspaceSessions.length === 0 && (
          sessionsRefreshing ? (
            <div className="sl-loading" role="status" aria-live="polite">
              <span className="sl-loading-spinner" aria-hidden="true" />
              <span>正在载入项目</span>
            </div>
          ) : (
            <div className="sl-empty">
              <span className="sl-empty-title">暂无工作区</span>
              <span className="sl-empty-copy">点击新建打开本地或远程工作区。</span>
            </div>
          )
        )}

        {workspaceSessions.map((workspaceItem) => (
          <WorkspaceSection
            key={workspaceItem.workspace.root}
            item={workspaceItem}
            activeSessionId={activeSessionId}
            activeSessionTitle={activeSessionTitle}
            currentSessionStatus={currentSessionStatus}
            activeConversationVisible={activeConversationVisible}
            onActivateWorkspace={handleActivateWorkspace}
            onReconnectRemoteWorkspace={handleReconnectRemoteWorkspace}
            onCreateSession={handleOpenAgentPicker}
            onSwitch={handleSwitch}
            onArchive={handleArchive}
            onArchiveWorkspace={handleArchiveWorkspace}
          />
        ))}
      </div>

      <div className="sl-footer">
        <button className="sl-settings-btn" type="button" onClick={onOpenSettings} title="设置" aria-label="打开设置">
          <SettingsIcon />
          <span>设置</span>
        </button>
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

      {remoteOpenVisible && createPortal(
        <div className="sl-agent-modal-backdrop" role="presentation" onClick={closeRemoteOpen}>
          <div
            className="sl-agent-modal sl-remote-open-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="sl-remote-open-title"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="sl-agent-modal-header">
              <span id="sl-remote-open-title">打开远程目录</span>
              <button type="button" className="sl-agent-close" onClick={closeRemoteOpen}>x</button>
            </div>
            <RemoteOpenPanel
              initialRemote={remoteReconnect}
              headingTitle={remoteReconnect ? "重新连接远程目录" : "打开远程目录"}
              onWorkspaceOpened={handleRemoteWorkspaceOpened}
              onOpenSettings={() => {
                setRemoteOpenVisible(false);
                setRemoteReconnect(null);
                onOpenSettings();
              }}
              onCancel={closeRemoteOpen}
            />
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
                  <div className="sl-agent-dropdown" ref={agentDropdownRef}>
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
                            onPointerDown={(event) => {
                              event.preventDefault();
                              event.stopPropagation();
                              if (agent.installed) {
                                handleSelectModalAgent(agent.id);
                              }
                            }}
                            onClick={(event) => {
                              event.preventDefault();
                              event.stopPropagation();
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
                  <label className={`sl-agent-option ${selectedAgent === agent.id ? "is-selected" : ""} ${agentDisabledForModal(agent, pendingRemoteWorkspace) ? "is-disabled" : ""}`} key={agent.id}>
                    <input
                      type="radio"
                      name="agent"
                      value={agent.id}
                      checked={selectedAgent === agent.id}
                      disabled={agentDisabledForModal(agent, pendingRemoteWorkspace) || isSubmitting}
                      onChange={() => handleSelectModalAgent(agent.id)}
                    />
                    <span className="sl-agent-label">{agent.label}</span>
                    <span className="sl-agent-meta">
                      {pendingRemoteWorkspace
                        ? agent.id === pendingRemoteAgent
                          ? "远程项目当前 Agent"
                          : "将为此会话准备远程 Agent"
                        : agent.id === agentSnapshot.settings.selected_agent
                          ? "Settings 默认"
                          : agent.binary}
                      {!pendingRemoteWorkspace && !agent.installed ? " · 未安装" : ""}
                    </span>
                  </label>
                ))}
              </div>
            )}
            {agentSnapshot.env_override && (
              <div className="sl-agent-note">已设置 ACP_AGENT_COMMAND；本次选择会直接使用所选 Agent。</div>
            )}
            {selectedClaudeSetupMessage && (
              <div className="sl-agent-error">{selectedClaudeSetupMessage}</div>
            )}
            {modalError && <div className="sl-agent-error">{modalError}</div>}
            <div className="sl-agent-actions">
              <button type="button" className="sl-agent-cancel" onClick={closeAgentPicker} disabled={isSubmitting}>取消</button>
              <button type="button" className="sl-agent-create" onClick={handleConfirmAgentModal} disabled={!selectedAgent || selectedAgentDisabled || !!selectedClaudeSetupMessage || (modalMode === "workspace" && !pendingWorkspacePath) || isSubmitting}>
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

function defaultAgentForNewWork(snapshot: AgentSettingsSnapshot): AgentCliId {
  const defaultAgent = snapshot.settings.selected_agent;
  const defaultInstalled = snapshot.agents.some(
    (agent) => agent.id === defaultAgent && agent.installed,
  );
  const codexInstalled = snapshot.agents.some(
    (agent) => agent.id === "codex-acp" && agent.installed,
  );
  const codebuddyInstalled = snapshot.agents.some(
    (agent) => agent.id === "codebuddy" && agent.installed,
  );
  if (
    defaultAgent === "claude-agent-acp" &&
    !claudeAgentReady(snapshot)
  ) {
    if (codexInstalled && codexAgentReady(snapshot)) {
      return "codex-acp";
    }
    if (!codebuddyInstalled) {
      return defaultAgent;
    }
    return "codebuddy";
  }
  return defaultInstalled
    ? defaultAgent
    : snapshot.agents.find((agent) => agent.installed)?.id ?? defaultAgent;
}

function agentDisabledForModal(
  agent: AgentSettingsSnapshot["agents"][number],
  pendingRemoteWorkspace: boolean,
) {
  if (pendingRemoteWorkspace) {
    return false;
  }
  return !agent.installed;
}

function codexAgentReady(snapshot: AgentSettingsSnapshot) {
  const selectedProfile = snapshot.codex_acp.profiles.find(
    (profile) => profile.id === snapshot.codex_acp.selected_profile_id,
  );
  if (!selectedProfile || selectedProfile.id === "default") return false;
  return !selectedProfile.requires_credential || selectedProfile.configured;
}

function claudeAgentReady(snapshot: AgentSettingsSnapshot) {
  const selectedProfile = snapshot.claude.profiles.find(
    (profile) => profile.id === snapshot.claude.selected_profile_id,
  );
  if (!selectedProfile) return false;
  return !selectedProfile.requires_credential || selectedProfile.configured;
}

function WorkspaceSection({
  item,
  activeSessionId,
  activeSessionTitle,
  currentSessionStatus,
  activeConversationVisible,
  onActivateWorkspace,
  onReconnectRemoteWorkspace,
  onCreateSession,
  onSwitch,
  onArchive,
  onArchiveWorkspace,
}: {
  item: WorkspaceSessionList;
  activeSessionId: string;
  activeSessionTitle: string;
  currentSessionStatus: SessionStatus;
  activeConversationVisible: boolean;
  onActivateWorkspace: (workspaceRoot: string) => void;
  onReconnectRemoteWorkspace: (remote: RemoteLinuxWorkspace) => void;
  onCreateSession: (workspaceRoot: string, remoteAgent: AgentCliId | null, isRemoteWorkspace: boolean) => void;
  onSwitch: (id: string, workspaceRoot: string) => void;
  onArchive: (id: string, workspaceRoot: string, sessionTitle?: string) => void;
  onArchiveWorkspace: (workspaceRoot: string, isActive: boolean, workspaceName?: string) => void;
}) {
  const sessions = sortSessions(item.sessions);
  const workspaceRoot = item.workspace.root;
  const isRemoteWorkspace = item.workspace.location?.kind === "remote_linux";
  const isDormantRemoteWorkspace = isRemoteWorkspace && !item.connected;
  const remoteWorkspace = item.workspace.location?.kind === "remote_linux" ? item.workspace.location : null;
  const workspaceActionRoot = remoteWorkspace ? remoteWorkspaceKey(remoteWorkspace) : workspaceRoot;
  const remoteAgent = remoteAgentForWorkspace(remoteWorkspace);
  const workspaceStateLabel = isDormantRemoteWorkspace ? "远程" : item.connected ? "在线" : "休眠";
  const workspaceActionHint = isDormantRemoteWorkspace ? "双击连接远程工作区" : undefined;
  const workspaceTooltip = workspaceActionHint ? `${workspaceActionHint}\n${workspaceRoot}` : workspaceRoot;

  return (
    <section className={`sl-workspace-section ${item.is_active ? "is-active" : ""} ${item.connected ? "is-connected" : "is-dormant"} ${isRemoteWorkspace ? "is-remote" : ""}`}>
      <div className="sl-workspace-row">
        <button
          className="sl-workspace-node"
          type="button"
          aria-current={item.is_active ? "true" : undefined}
          onClick={() => {
            if (!isDormantRemoteWorkspace) {
              onActivateWorkspace(workspaceActionRoot);
            }
          }}
          onDoubleClick={() => {
            if (isDormantRemoteWorkspace && remoteWorkspace) {
              onReconnectRemoteWorkspace(remoteWorkspace);
            }
          }}
          title={workspaceTooltip}
        >
          <FolderIcon />
          <span className="sl-workspace-copy">
            <span className="sl-workspace-name" title={workspaceRoot}>{item.workspace.name}</span>
          </span>
          <span className="sl-workspace-state">{workspaceStateLabel}</span>
        </button>
        <button
          className="sl-workspace-edit"
          type="button"
          onClick={() => onCreateSession(workspaceActionRoot, remoteAgent, isRemoteWorkspace)}
          title={isDormantRemoteWorkspace ? "先双击连接远程工作区" : "新建会话"}
          aria-label={`在 ${item.workspace.name} 中新建会话`}
          disabled={isDormantRemoteWorkspace}
        >
          <EditIcon />
        </button>
        <button
          className="sl-workspace-archive"
          type="button"
          onClick={() => onArchiveWorkspace(workspaceActionRoot, item.is_active, item.workspace.name)}
          title="归档项目"
          aria-label={`归档项目 ${item.workspace.name}`}
        >
          <ArchiveIcon />
        </button>
      </div>

      <div className="sl-thread-branch">
        <div className="sl-items">
          {item.sessions.length === 0 && (
            <div className="sl-empty sl-session-empty">
              <span className="sl-empty-title">{item.connected ? "暂无会话" : "未加载会话"}</span>
              <span className="sl-empty-copy">{item.connected ? "在此工作区中开始一个会话。" : isDormantRemoteWorkspace ? "双击项目目录后重新连接。" : "点击工作区后按需加载。"}</span>
            </div>
          )}

          {sessions.map((session) => {
            const isActiveSession = session.id === activeSessionId && item.is_active && !isDormantRemoteWorkspace;
            const displaySession = isActiveSession
              ? {
                  ...session,
                  title: activeSessionTitle || session.title,
                  status: currentSessionStatus,
                }
              : session;

            return (
              <ThreadRow
                key={session.id}
                session={displaySession}
                active={isActiveSession}
                activeConversationVisible={isActiveSession ? activeConversationVisible : true}
                connected={session.id === activeSessionId && item.is_active && item.connected}
                disabled={isDormantRemoteWorkspace}
                onSwitch={(id) => onSwitch(id, workspaceActionRoot)}
                onArchive={(id) => onArchive(id, workspaceActionRoot, session.title)}
              />
            );
          })}
        </div>
      </div>
    </section>
  );
}

function remoteWorkspaceKey(remote: RemoteLinuxWorkspace): string {
  const port = remote.ssh_port ? `:${remote.ssh_port}` : "";
  const remotePath = remote.remote_path.trim();
  const normalizedPath = remotePath.startsWith("/") ? remotePath : `/${remotePath}`;
  return `ssh://${remote.ssh_target.trim()}${port}${normalizedPath}`;
}

function remoteAgentForWorkspace(remote: RemoteLinuxWorkspace | null): AgentCliId | null {
  if (!remote) return null;
  if (remote.agent_cli) return remote.agent_cli;
  const command = remote.agent_command?.toLowerCase() ?? "";
  if (command.includes("claude-agent-acp")) return "claude-agent-acp";
  if (command.includes("codex-acp") || command.includes("kodex-acp")) return "codex-acp";
  if (command.includes("codebuddy")) return "codebuddy";
  return null;
}

function ThreadRow({
  session,
  active,
  activeConversationVisible,
  connected,
  disabled = false,
  onSwitch,
  onArchive,
}: {
  session: SessionListItem;
  active: boolean;
  activeConversationVisible: boolean;
  connected: boolean;
  disabled?: boolean;
  onSwitch: (id: string) => void;
  onArchive: (id: string) => void;
}) {
  const timeLabel = formatRelativeTime(session.updated_at || session.created_at);
  const agentLabel = formatAgentLabel(session.agent_cli);
  const disabledHint = "先连接远程项目后再打开会话";
  const sessionTooltip = disabled
    ? `${session.title} · ${disabledHint}`
    : agentLabel
      ? `${session.title} · ${agentLabel}`
      : session.title;
  const runtimeStatus = session.runtime_status ?? "none";
  const attentionState = session.attention_state ?? "none";
  const turnStillRunning = session.status === "Streaming" || session.status === "WaitingForTool";
  const showActiveProgress = !disabled && active && !activeConversationVisible && turnStillRunning;
  const hasAttention = !disabled && !active && attentionState === "needs_attention";
  const hiddenWorkspaceActiveProgress =
    !disabled && !active && !hasAttention && runtimeStatus === "active" && turnStillRunning;
  const showBackgroundProgress =
    !disabled &&
    !active &&
    !hasAttention &&
    (runtimeStatus === "background_running" || hiddenWorkspaceActiveProgress);
  const showProgress = showActiveProgress || showBackgroundProgress;
  const showCompletedDot = !disabled && !active && attentionState === "completed_unviewed";
  const showAttentionDot = hasAttention;
  const indicatorLabel = disabled
    ? disabledHint
    : showActiveProgress
    ? "当前会话仍在运行"
    : showAttentionDot
      ? "后台会话需要处理"
    : showBackgroundProgress
    ? "后台会话仍在运行"
      : showCompletedDot
        ? "后台会话已完成，尚未查看"
        : connected
          ? "Agent 已连接"
          : undefined;

  return (
    <div
      className={[
        "sl-item",
        active ? "sl-active" : "",
        disabled ? "is-disabled" : "",
        showActiveProgress ? "is-active-running" : "",
        showBackgroundProgress ? "is-background-running" : "",
        showCompletedDot ? "is-completed-unviewed" : "",
        showAttentionDot ? "is-needs-attention" : "",
      ].filter(Boolean).join(" ")}
    >
      <button
        className="sl-item-button"
        type="button"
        onClick={() => onSwitch(session.id)}
        aria-current={active ? "page" : undefined}
        disabled={disabled}
      >
        <span
          className={[
            "sl-session-online",
            connected ? "is-visible" : "",
            showProgress ? "is-progress" : "",
            showCompletedDot ? "is-complete" : "",
            showAttentionDot ? "is-attention" : "",
          ].filter(Boolean).join(" ")}
          title={indicatorLabel}
          aria-label={indicatorLabel}
        />
        <span className="sl-item-main">
          <span className="sl-item-title" title={sessionTooltip}>{session.title}</span>
        </span>
        {(timeLabel || agentLabel) && (
          <span className="sl-item-side-label">
            {timeLabel && <span className="sl-item-time">{timeLabel}</span>}
            {agentLabel && <span className="sl-item-agent">{agentLabel}</span>}
          </span>
        )}
      </button>
      <button
        className="sl-archive-btn"
        type="button"
        disabled={disabled}
        onPointerDown={(event) => {
          event.preventDefault();
          event.stopPropagation();
        }}
        onClick={(event) => {
          event.preventDefault();
          event.stopPropagation();
          if (!disabled) {
            onArchive(session.id);
          }
        }}
        title={disabled ? disabledHint : "归档会话"}
        aria-label={`归档会话 ${session.title}`}
      >
        <ArchiveIcon />
      </button>
    </div>
  );
}

function formatAgentLabel(value?: string | null): string | null {
  const raw = value?.trim();
  if (!raw) return "未知";
  const normalized = raw.toLowerCase();
  if (normalized.includes("codebuddy")) return "CodeBuddy";
  if (normalized.includes("claude")) return "Claude";
  if (normalized.includes("codex")) return "Codex";
  if (normalized.includes("goose")) return "goose";
  return raw;
}

function sortSessions(sessions: SessionListItem[]): SessionListItem[] {
  return [...sessions].sort((a, b) => {
    return getTimestamp(b.updated_at || b.created_at) - getTimestamp(a.updated_at || a.created_at);
  });
}

function formatRelativeTime(value: string): string | null {
  const timestamp = getTimestamp(value);
  if (!timestamp) return null;

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

function getTimestamp(value: string) {
  if (!value) return 0;

  const numericValue = Number(value);
  if (Number.isFinite(numericValue) && numericValue > 0) {
    return numericValue < 1_000_000_000_000 ? numericValue * 1000 : numericValue;
  }

  const normalizedValue = value.includes("T") || /[zZ]|[+-]\d{2}:?\d{2}$/.test(value)
    ? value
    : value.replace(" ", "T");
  const timestamp = new Date(normalizedValue).getTime();
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function FolderIcon() {
  return (
    <svg className="sl-nav-icon" viewBox="0 0 20 20" aria-hidden="true">
      <path d="M2.5 6.2c0-1 .8-1.8 1.8-1.8h3.4l1.5 1.6h6.5c1 0 1.8.8 1.8 1.8v6.7c0 1-.8 1.8-1.8 1.8H4.3c-1 0-1.8-.8-1.8-1.8V6.2Z" />
      <path d="M2.5 8.2h15" />
    </svg>
  );
}

function EditIcon() {
  return (
    <svg className="sl-action-icon" viewBox="0 0 20 20" aria-hidden="true">
      <path d="M11.9 4.2 15.8 8 8.1 15.7l-4.2.7.7-4.2 7.3-8Z" />
      <path d="m11.1 5 3.9 3.9" />
    </svg>
  );
}

function ArchiveIcon() {
  return (
    <svg className="sl-action-icon" viewBox="0 0 20 20" aria-hidden="true">
      <path d="M4.2 5.4h11.6v3H4.2z" />
      <path d="M5.5 8.4v6.2c0 .7.5 1.2 1.2 1.2h6.6c.7 0 1.2-.5 1.2-1.2V8.4" />
      <path d="M8 11.1h4" />
    </svg>
  );
}

function PlusIcon() {
  return (
    <svg className="sl-action-icon" viewBox="0 0 20 20" aria-hidden="true">
      <path d="M10 4.5v11" />
      <path d="M4.5 10h11" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg className="sl-action-icon" viewBox="0 0 20 20" aria-hidden="true">
      <circle cx="10" cy="10" r="2.6" />
      <path d="M16.4 11.5a1.4 1.4 0 0 0 .3 1.5l.1.1a1.7 1.7 0 1 1-2.4 2.4l-.1-.1a1.4 1.4 0 0 0-1.5-.3 1.4 1.4 0 0 0-.8 1.3v.2a1.7 1.7 0 1 1-3.4 0v-.2a1.4 1.4 0 0 0-.8-1.3 1.4 1.4 0 0 0-1.5.3l-.1.1a1.7 1.7 0 1 1-2.4-2.4l.1-.1a1.4 1.4 0 0 0 .3-1.5 1.4 1.4 0 0 0-1.3-.8h-.2a1.7 1.7 0 1 1 0-3.4h.2a1.4 1.4 0 0 0 1.3-.8 1.4 1.4 0 0 0-.3-1.5l-.1-.1a1.7 1.7 0 1 1 2.4-2.4l.1.1a1.4 1.4 0 0 0 1.5.3 1.4 1.4 0 0 0 .8-1.3v-.2a1.7 1.7 0 1 1 3.4 0v.2a1.4 1.4 0 0 0 .8 1.3 1.4 1.4 0 0 0 1.5-.3l.1-.1a1.7 1.7 0 1 1 2.4 2.4l-.1.1a1.4 1.4 0 0 0-.3 1.5 1.4 1.4 0 0 0 1.3.8h.2a1.7 1.7 0 1 1 0 3.4h-.2a1.4 1.4 0 0 0-1.3.8Z" />
    </svg>
  );
}
