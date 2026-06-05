import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type { UiSnapshot, AppTheme, ToolInvocation } from "../../types";
import { startupPerfMark, sessionResolvePermission, settingsGetAgentSnapshot } from "../../lib/tauri";
import { ConversationTimeline } from "../conversation/ConversationTimeline";
import { Composer, type ComposerReferenceRequest } from "../composer/Composer";
import {
  AgentPlanPanel,
  PermissionRequestPanel,
  type PendingPermissionRequest,
  findPlanAcceptOption,
  findPlanRejectOption,
  shouldShowAgentPlanNearComposer,
} from "../composer/AgentPlanPanel";
import { ReviewPanel } from "../review/ReviewPanel";
import type { ReviewPanelActiveTab, ReviewPanelOpenTab } from "../review/ReviewPanel";
import { DiffTab } from "../editor/DiffTab";
import { EditorView } from "../editor/EditorView";
import { WelcomeLauncher } from "./WelcomeLauncher";
import { SessionList } from "../session/SessionList";
import { TabBar } from "./TabBar";
import { GlobalChrome } from "./GlobalChrome";
import { RemoteOpenPanel } from "./RemoteOpenPanel";
import { ThreadHeader } from "./ThreadHeader";
import { ThreadSidebarShell } from "./ThreadSidebarShell";
import {
  SettingsPage,
  type AgentSettingsTab,
  type RemoteSettingsContext,
  type SettingsPane,
  type SettingsStartupNotice,
} from "../settings/SettingsPage";
import { TerminalDock } from "../terminal/TerminalDock";
import { applyAppTheme, DEFAULT_APP_THEME } from "../../theme";
import { checkForAppUpdate, type AppUpdateInfo } from "../../lib/updater";
import { useWorkbenchSnapshot } from "./useWorkbenchSnapshot";
import { useWorkbenchGit } from "./useWorkbenchGit";
import { useTimelineChangeSets } from "./useTimelineChangeSets";
import { useWorkbenchTabs } from "./useWorkbenchTabs";
import { useLeftSidebarState } from "./useLeftSidebarState";
import { useRightPanelState } from "./useRightPanelState";
import { useTerminalDockState } from "./useTerminalDockState";
import {
  latestReviewableTurnChangeSet,
  reviewableTurnChangeSetSignature,
} from "./autoReview";
import "./Workbench.css";

const INITIAL_REVIEW_PANEL_ACTIVE_TAB: ReviewPanelActiveTab = {
  kind: "base",
  tab: "Review",
};
const EMPTY_HIDDEN_PERMISSION_REQUEST_IDS = new Set<string>();

let startupUpdateCheckPromise: Promise<AppUpdateInfo | null> | null = null;

export function Workbench() {
  const {
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady,
    pollState,
    acceptSnapshot,
    clearSnapshot,
  } = useWorkbenchSnapshot();
  const {
    gitRefreshing,
    gitHydrated,
    handleRefreshGit,
    resetGitHydration,
  } = useWorkbenchGit({
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady,
  });
  const {
    timelineTurnChangeSets,
    liveTurnChangeSet,
    agentConversationChangeCount,
    clearChangeSets,
  } = useTimelineChangeSets({
    snapshot,
    snapshotRef,
    workspaceReady,
    onGitRefresh: handleRefreshGit,
  });
  const handleAfterEditorSave = useCallback(async () => {
    await handleRefreshGit();
    await pollState();
  }, [handleRefreshGit, pollState]);
  const {
    activeTab,
    activeTabId,
    displayTabs,
    resolvedDiffChange,
    pendingCloseTab,
    resetTabs,
    handleOpenDiffTab,
    handleOpenEditorTab,
    handleSearchResultOpen,
    handleCloseTab,
    handleConfirmSaveClose,
    handleConfirmDiscardClose,
    handleCancelClose,
    handleEditorDirtyChange,
    handleEditorUserInteraction,
    handleEditorSaved,
    handleTabSelect,
  } = useWorkbenchTabs({ onAfterEditorSave: handleAfterEditorSave });
  const [composerReferenceRequests, setComposerReferenceRequests] = useState<
    ComposerReferenceRequest[]
  >([]);
  const [reviewFocusRequest, setReviewFocusRequest] = useState<{
    changeSetId: string;
    token: number;
  } | null>(null);
  const autoReviewSignatureRef = useRef<string | null>(null);
  const reviewFocusSeqRef = useRef(0);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const {
    leftSidebarWidth,
    leftSidebarStyle,
    clampStoredLeftSidebarWidth,
    handleLeftSidebarResizeStart,
  } = useLeftSidebarState();
  const {
    rightPanelCollapsed,
    setRightPanelCollapsed,
    rightPanelWidth,
    rightPanelStyle,
    clampStoredRightPanelWidth,
    handleRightPanelResizeStart,
  } = useRightPanelState();
  const [reviewPanelExpanded, setReviewPanelExpanded] = useState(false);
  const [expandedReviewSideTreeVisible, setExpandedReviewSideTreeVisible] = useState(false);
  const [reviewPanelActiveTab, setReviewPanelActiveTab] = useState<ReviewPanelActiveTab>(
    INITIAL_REVIEW_PANEL_ACTIVE_TAB,
  );
  const [reviewPanelOpenTabs, setReviewPanelOpenTabs] = useState<ReviewPanelOpenTab[]>([]);
  const {
    terminalDockVisible,
    terminalDockMounted,
    terminalDockHeight,
    handleToggleTerminalDock,
    handleHideTerminalDock,
    handleTerminalDockHeightChange,
  } = useTerminalDockState(snapshot, snapshotRef);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [remoteOpenVisible, setRemoteOpenVisible] = useState(false);
  const [settingsStartupNotice, setSettingsStartupNotice] = useState<SettingsStartupNotice | null>(null);
  const [settingsInitialPane, setSettingsInitialPane] = useState<SettingsPane | null>(null);
  const [settingsInitialAgentTab, setSettingsInitialAgentTab] = useState<AgentSettingsTab | null>(null);
  const [settingsRemoteContext, setSettingsRemoteContext] = useState<RemoteSettingsContext | null>(null);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);
  const [startupUpdateInfo, setStartupUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [startupUpdateDismissed, setStartupUpdateDismissed] = useState(false);
  const [resolvingPermissionIds, setResolvingPermissionIds] = useState<Set<string>>(
    () => new Set(),
  );

  useEffect(() => {
    if (!snapshot) {
      setResolvingPermissionIds((current) => (current.size === 0 ? current : new Set()));
      return;
    }
    const pendingIds = new Set(
      findPendingPermissionRequests(snapshot.tools).map((request) => request.requestId),
    );
    setResolvingPermissionIds((current) => {
      const next = new Set([...current].filter((requestId) => pendingIds.has(requestId)));
      return next.size === current.size ? current : next;
    });
  }, [snapshot?.tools]);

  const handleOpenSettings = useCallback((options?: {
    startupNotice?: SettingsStartupNotice;
    initialPane?: SettingsPane;
    initialAgentTab?: AgentSettingsTab;
  }) => {
    const currentSnapshot = snapshotRef.current;
    const remote = currentSnapshot?.workspace.location?.kind === "remote_linux"
      ? currentSnapshot.workspace.location
      : null;
    const remoteContext: RemoteSettingsContext | null = remote
      ? {
          profileId: remote.profile_id ?? null,
          workspaceName: currentSnapshot?.workspace.name ?? "远程工作区",
          sshTarget: remote.ssh_target,
          sshPort: remote.ssh_port,
          remotePath: remote.remote_path,
          agentLabel: currentSnapshot?.session.agent_cli ?? remote.agent_cli ?? null,
        }
      : null;
    setSettingsStartupNotice(options?.startupNotice ?? null);
    setSettingsInitialPane(options?.initialPane ?? (remoteContext ? "remote" : null));
    setSettingsInitialAgentTab(options?.initialAgentTab ?? null);
    setSettingsRemoteContext(remoteContext);
    setSettingsOpen(true);
  }, [snapshotRef]);

  const handleCloseSettings = useCallback(() => {
    setSettingsOpen(false);
    setSettingsStartupNotice(null);
    setSettingsInitialPane(null);
    setSettingsInitialAgentTab(null);
    setSettingsRemoteContext(null);
  }, []);

  const resetReviewPanelTabs = useCallback(() => {
    setReviewPanelActiveTab(INITIAL_REVIEW_PANEL_ACTIVE_TAB);
    setReviewPanelOpenTabs([]);
    setExpandedReviewSideTreeVisible(false);
  }, []);

  useEffect(() => {
    settingsGetAgentSnapshot()
      .then((snapshot) => setAppTheme(applyAppTheme(snapshot.settings.theme)))
      .catch(() => setAppTheme(applyAppTheme(DEFAULT_APP_THEME)));
  }, []);

  useEffect(() => {
    let disposed = false;
    startupUpdateCheckPromise ??= checkForAppUpdate()
      .catch((error) => {
        console.info("Startup update check skipped", error);
        return null;
      });
    void startupUpdateCheckPromise.then((update) => {
      if (!disposed && update) {
        setStartupUpdateInfo(update);
      }
    });

    return () => {
      disposed = true;
    };
  }, []);

  useEffect(() => {
    const handleResize = () => {
      clampStoredLeftSidebarWidth();
      clampStoredRightPanelWidth();
    };
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [clampStoredLeftSidebarWidth, clampStoredRightPanelWidth]);

  useEffect(() => {
    const handleOpenSettingsEvent = () => handleOpenSettings();
    window.addEventListener("kodex:open-settings", handleOpenSettingsEvent);
    return () => window.removeEventListener("kodex:open-settings", handleOpenSettingsEvent);
  }, [handleOpenSettings]);

  const handleWorkspaceOpened = useCallback((nextSnapshot: UiSnapshot) => {
    void startupPerfMark("workbench/handle_workspace_opened", "");
    acceptSnapshot(nextSnapshot);
    setRemoteOpenVisible(false);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
  }, [acceptSnapshot, clearChangeSets, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handleWorkspaceChanged = useCallback((nextSnapshot: UiSnapshot) => {
    acceptSnapshot(nextSnapshot);
    setRemoteOpenVisible(false);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
  }, [acceptSnapshot, clearChangeSets, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handlePermissionSelect = useCallback(async (requestId: string, optionId: string | null, guidance?: string | null) => {
    setResolvingPermissionIds((current) => {
      if (current.has(requestId)) return current;
      const next = new Set(current);
      next.add(requestId);
      return next;
    });
    try {
      await sessionResolvePermission(requestId, optionId, guidance);
      await pollState();
    } catch (error) {
      setResolvingPermissionIds((current) => {
        const next = new Set(current);
        next.delete(requestId);
        return next;
      });
      throw error;
    }
  }, [pollState]);

  const handleSessionChanged = useCallback(() => {
    clearSnapshot();
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setReviewPanelExpanded(false);
    pollState();
  }, [clearChangeSets, clearSnapshot, pollState, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handleToggleThreads = useCallback(() => {
    setSidebarCollapsed((collapsed) => !collapsed);
  }, []);

  const handleToggleRightPanel = useCallback(() => {
    setRightPanelCollapsed((collapsed) => {
      if (!collapsed) {
        setReviewPanelExpanded(false);
      }
      return !collapsed;
    });
  }, [setRightPanelCollapsed]);

  const handleReviewPanelExpandedChange = useCallback((expanded: boolean) => {
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(expanded);
    if (!expanded) {
      setExpandedReviewSideTreeVisible(false);
    }
  }, [setRightPanelCollapsed]);

  const handleReviewChangeSetSelect = useCallback((changeSetId: string) => {
    setRightPanelCollapsed(false);
    reviewFocusSeqRef.current += 1;
    setReviewFocusRequest({ changeSetId, token: reviewFocusSeqRef.current });
  }, [setRightPanelCollapsed]);

  const autoReviewTarget = useMemo(
    () => latestReviewableTurnChangeSet(timelineTurnChangeSets, liveTurnChangeSet),
    [liveTurnChangeSet, timelineTurnChangeSets],
  );
  const autoReviewSignature = useMemo(
    () =>
      autoReviewTarget
        ? reviewableTurnChangeSetSignature(snapshot?.session.id ?? "", autoReviewTarget)
        : null,
    [autoReviewTarget, snapshot?.session.id],
  );

  useEffect(() => {
    if (!autoReviewTarget || !autoReviewSignature) return;
    if (autoReviewSignatureRef.current === autoReviewSignature) return;
    autoReviewSignatureRef.current = autoReviewSignature;
    setRightPanelCollapsed(false);
    reviewFocusSeqRef.current += 1;
    setReviewFocusRequest({
      changeSetId: autoReviewTarget.changeSetId,
      token: reviewFocusSeqRef.current,
    });
  }, [autoReviewSignature, autoReviewTarget, setRightPanelCollapsed]);

  const enqueueComposerReference = useCallback(
    (request: Omit<ComposerReferenceRequest, "id">) => {
      setComposerReferenceRequests((current) => [
        ...current,
        {
          ...request,
          id: `ref-${Date.now()}-${Math.random().toString(36).slice(2)}`,
        },
      ]);
    },
    [],
  );

  const handleComposerReferenceConsumed = useCallback((id: string) => {
    setComposerReferenceRequests((current) => current.filter((request) => request.id !== id));
  }, []);

  const renderReviewFileTab = useCallback((
    path: string,
    context?: { fileTreeVisible: boolean; onToggleFileTree?: () => void },
  ) => (
    <EditorView
      path={path}
      appTheme={appTheme}
      toolbarMode="breadcrumbs"
      workspaceName={snapshot?.workspace.name}
      fileTreeVisible={context?.fileTreeVisible ?? false}
      onToggleFileTree={context?.onToggleFileTree}
      onDirtyChange={handleEditorDirtyChange}
      onSaved={handleEditorSaved}
    />
  ), [appTheme, handleEditorDirtyChange, handleEditorSaved, snapshot?.workspace.name]);

  const pendingPermissionRequests = useMemo(
    () =>
      snapshot
        ? findPendingPermissionRequests(snapshot.tools).filter(
            (request) => !resolvingPermissionIds.has(request.requestId),
          )
        : [],
    [snapshot?.tools, resolvingPermissionIds],
  );
  const hiddenPermissionRequestIds = useMemo(
    () =>
      pendingPermissionRequests.length > 0
        ? new Set(pendingPermissionRequests.map((request) => request.requestId))
        : EMPTY_HIDDEN_PERMISSION_REQUEST_IDS,
    [pendingPermissionRequests],
  );
  const updateNotice = startupUpdateInfo && !startupUpdateDismissed ? (
    <div className="startup-update-notice" role="status" aria-live="polite">
      <div className="startup-update-copy">
        <span className="startup-update-title">发现新版本 {startupUpdateInfo.version}</span>
        <span className="startup-update-meta">当前版本 {startupUpdateInfo.currentVersion}</span>
      </div>
      <div className="startup-update-actions">
        <button type="button" className="startup-update-btn" onClick={() => handleOpenSettings()}>
          打开设置
        </button>
        <button
          type="button"
          className="startup-update-close"
          onClick={() => setStartupUpdateDismissed(true)}
          aria-label="关闭更新提示"
        >
          ×
        </button>
      </div>
    </div>
  ) : null;

  if (settingsOpen) {
    return (
      <div className="workbench">
        <SettingsPage
          initialPane={settingsInitialPane ?? undefined}
          initialAgentTab={settingsInitialAgentTab ?? undefined}
          remoteContext={settingsRemoteContext}
          startupNotice={settingsStartupNotice}
          onBack={handleCloseSettings}
          onStartupNoticeDismissed={() => setSettingsStartupNotice(null)}
          onThemeChange={setAppTheme}
        />
        {updateNotice}
      </div>
    );
  }

  // No workspace loaded — show welcome screen
  if (!workspaceReady) {
    return (
      <>
        <WelcomeLauncher
          onWorkspaceOpened={handleWorkspaceOpened}
          onOpenSettings={handleOpenSettings}
        />
        {updateNotice}
      </>
    );
  }

  if (!snapshot) {
    return (
      <div className="workbench" style={{ alignItems: "center", justifyContent: "center" }}>
        <div style={{ color: "var(--text-strong, #d7e1df)", fontSize: 16, fontFamily: "monospace" }}>
          正在等待后端快照...
        </div>
      </div>
    );
  }

  const isRemoteWorkspace = snapshot.workspace.location?.kind === "remote_linux";
  const terminalDockAvailable = !isRemoteWorkspace;
  const terminalDockActive = terminalDockAvailable && terminalDockVisible;
  const agentLabel = snapshot.session.agent_cli || "智能体";
  const showComposerPlan = shouldShowAgentPlanNearComposer(snapshot);
  const composerStatusSlot = pendingPermissionRequests.length > 0 || showComposerPlan ? (
    <div className="composer-plan-slot">
      {pendingPermissionRequests.map((request) => (
        <PermissionRequestPanel
          key={request.requestId}
          request={request}
          entries={snapshot.agent_plan ?? []}
          onPermissionSelect={handlePermissionSelect}
        />
      ))}
      {showComposerPlan && (
        <AgentPlanPanel entries={snapshot.agent_plan} />
      )}
    </div>
  ) : null;
  const workbenchBodyClassName = [
    "workbench-body",
    terminalDockActive ? "has-terminal-dock" : "",
    reviewPanelExpanded ? "is-review-expanded" : "",
    reviewPanelExpanded && expandedReviewSideTreeVisible ? "has-expanded-review-side-tree" : "",
  ].filter(Boolean).join(" ");
  const reviewPanel = (
    <ReviewPanel
      snapshot={snapshot}
      refreshing={gitRefreshing}
      hydrated={gitHydrated}
      appTheme={appTheme}
      panelExpanded={reviewPanelExpanded}
      onRefresh={handleRefreshGit}
      onFileSelect={(path, changeSetId) =>
        handleOpenDiffTab(path, "git", undefined, changeSetId)
      }
      onFileOpen={handleOpenEditorTab}
      onAddComposerReference={(path) => enqueueComposerReference({ path })}
      onPanelExpandedChange={handleReviewPanelExpandedChange}
      onEditorFileTreeVisibleChange={setExpandedReviewSideTreeVisible}
      renderFileTab={renderReviewFileTab}
      activeTab={reviewPanelActiveTab}
      openTabs={reviewPanelOpenTabs}
      onActiveTabChange={setReviewPanelActiveTab}
      onOpenTabsChange={setReviewPanelOpenTabs}
      focusRequest={reviewFocusRequest}
    />
  );

  return (
    <div className="workbench">
      <GlobalChrome
        workspace={snapshot.workspace}
        remoteWorkspace={isRemoteWorkspace}
        sidebarCollapsed={sidebarCollapsed}
        refreshing={gitRefreshing}
        rightPanelCollapsed={rightPanelCollapsed}
        terminalDockVisible={terminalDockActive}
        onToggleSidebar={handleToggleThreads}
        onToggleTerminal={terminalDockAvailable ? handleToggleTerminalDock : () => undefined}
        onRefreshGit={handleRefreshGit}
        onToggleRightPanel={handleToggleRightPanel}
        onOpenRemoteWorkspace={() => setRemoteOpenVisible(true)}
        onFileOpen={handleSearchResultOpen}
      />

      <div className="workbench-content" style={leftSidebarStyle}>
        <ThreadSidebarShell collapsed={sidebarCollapsed}>
          <SessionList
            activeSessionId={snapshot.session.id}
            activeSessionTitle={snapshot.session.title}
            activeWorkspaceRoot={snapshot.workspace.root}
            currentSessionStatus={snapshot.session.status}
            onOpenSettings={handleOpenSettings}
            onSessionChanged={handleSessionChanged}
            onWorkspaceChanged={handleWorkspaceChanged}
          />
        </ThreadSidebarShell>
        {!sidebarCollapsed && (
          <div className="sidebar-resizer">
            <button
              type="button"
              className="sidebar-resizer-hit"
              aria-label="调整项目栏宽度"
              title="拖拽调整项目栏宽度"
              onPointerDown={handleLeftSidebarResizeStart}
            />
          </div>
        )}

        <div className="workbench-main-shell">

        <div
          className={workbenchBodyClassName}
          style={rightPanelStyle}
        >
          <main className="center-panel">
            {reviewPanelExpanded && (
              <section className="expanded-review-panel-shell" aria-label="展开审查面板">
                {reviewPanel}
              </section>
            )}
            <ThreadHeader
              session={snapshot.session}
              workspace={snapshot.workspace}
              activeTabLabel={agentLabel}
              changeCount={agentConversationChangeCount}
            />

            {displayTabs.length > 1 && (
              <div className="center-tab-bar-shell">
                <TabBar
                  tabs={displayTabs}
                  activeTabId={activeTabId}
                  onTabSelect={handleTabSelect}
                  onTabClose={handleCloseTab}
                  className="center-tab-bar"
                />
              </div>
            )}

            {reviewPanelExpanded ? (
              <div
                className={`expanded-review-composer-layer ${
                  expandedReviewSideTreeVisible ? "has-review-side-tree" : ""
                }`}
              >
                {composerStatusSlot}
                <Composer
                  snapshot={snapshot}
                  onStateChange={pollState}
                  referenceRequests={composerReferenceRequests}
                  onReferenceRequestConsumed={handleComposerReferenceConsumed}
                  compact
                />
              </div>
            ) : (
              <div
                className={`conversation-container ${
                  activeTab.type === "conversation" ? "" : "is-workspace-tab"
                }`}
              >
                {activeTab.type === "conversation" ? (
                  <>
                    <ConversationTimeline
                      snapshot={snapshot}
                      onPermissionSelect={handlePermissionSelect}
                      turnChangeSetsByMessageId={timelineTurnChangeSets}
                      onReviewFileSelect={(path, changeSetId) =>
                        handleOpenDiffTab(path, "change-set", undefined, changeSetId)
                      }
                      onReviewChangeSetSelect={handleReviewChangeSetSelect}
                      hiddenPermissionRequestIds={hiddenPermissionRequestIds}
                    />
                  </>
                ) : (
                  <section className="workspace-tab-content" aria-label="打开的文件">
                    {activeTab.type === "diff" && resolvedDiffChange && (
                      <DiffTab change={resolvedDiffChange} appTheme={appTheme} />
                    )}
                    {activeTab.type === "diff" && !resolvedDiffChange && (
                      <div className="workbench-loading">正在加载差异...</div>
                    )}
                    {activeTab.type === "editor" && activeTab.filePath && (
                      <EditorView
                        path={activeTab.filePath}
                        lineNumber={activeTab.lineNumber}
                        searchQuery={activeTab.searchQuery}
                        navToken={activeTab.navToken}
                        appTheme={appTheme}
                        onDirtyChange={handleEditorDirtyChange}
                        onSaved={handleEditorSaved}
                        onUserInteraction={handleEditorUserInteraction}
                        onAddComposerReference={enqueueComposerReference}
                      />
                    )}
                  </section>
                )}
                {composerStatusSlot}
                <Composer
                  snapshot={snapshot}
                  onStateChange={pollState}
                  referenceRequests={composerReferenceRequests}
                  onReferenceRequestConsumed={handleComposerReferenceConsumed}
                />
              </div>
            )}
          </main>

          {!rightPanelCollapsed && !reviewPanelExpanded && (
            <div className="panel-resizer">
              <button
                type="button"
                className="panel-resizer-hit"
                aria-label="调整右侧面板宽度"
                title="拖拽调整右侧面板宽度"
                onPointerDown={handleRightPanelResizeStart}
              />
            </div>
          )}

          {reviewPanelExpanded ? (
            <aside className="right-panel is-expanded-spacer" aria-hidden="true" />
          ) : (
            <aside className={`right-panel ${rightPanelCollapsed ? "is-collapsed" : ""}`}>
              {reviewPanel}
            </aside>
          )}
        </div>
        {terminalDockAvailable && terminalDockMounted && (
          <TerminalDock
            key={snapshot.workspace.root}
            workspaceRoot={snapshot.workspace.root}
            appTheme={appTheme}
            visible={terminalDockActive}
            height={terminalDockHeight}
            layoutSignal={`${leftSidebarWidth}:${sidebarCollapsed}:${rightPanelWidth}:${rightPanelCollapsed}:${reviewPanelExpanded}`}
            onHeightChange={handleTerminalDockHeightChange}
            onHide={handleHideTerminalDock}
          />
        )}
        {pendingCloseTab && (
          <div className="unsaved-close-backdrop" role="presentation">
            <div className="unsaved-close-dialog" role="dialog" aria-modal="true" aria-labelledby="unsaved-close-title">
              <div className="unsaved-close-title" id="unsaved-close-title">
                内容有改变
              </div>
              <div className="unsaved-close-message">
                {pendingCloseTab.label} 还没有保存，关闭前要保存修改吗？
              </div>
              <div className="unsaved-close-actions">
                <button type="button" className="unsaved-close-btn" onClick={handleCancelClose}>
                  取消
                </button>
                <button type="button" className="unsaved-close-btn" onClick={handleConfirmDiscardClose}>
                  直接关闭
                </button>
                <button type="button" className="unsaved-close-btn is-primary" onClick={handleConfirmSaveClose}>
                  保存并关闭
                </button>
              </div>
            </div>
          </div>
        )}
        {remoteOpenVisible && (
          <div className="remote-open-backdrop" role="presentation">
            <div className="remote-open-dialog" role="dialog" aria-modal="true" aria-labelledby="remote-open-title">
              <div className="remote-open-dialog-head">
                <div>
                  <div className="remote-open-dialog-kicker">远程</div>
                  <h2 id="remote-open-title">打开远程目录</h2>
                </div>
                <button
                  type="button"
                  className="remote-open-close"
                  onClick={() => setRemoteOpenVisible(false)}
                  aria-label="关闭远程打开"
                >
                  ×
                </button>
              </div>
              <RemoteOpenPanel
                onWorkspaceOpened={handleWorkspaceChanged}
                onOpenSettings={() => {
                  setRemoteOpenVisible(false);
                  handleOpenSettings();
                }}
                onCancel={() => setRemoteOpenVisible(false)}
              />
            </div>
          </div>
        )}
        {updateNotice}
      </div>
      </div>
    </div>
  );
}

export function findPendingPlanApproval(tools: ToolInvocation[]) {
  const toolIndex = tools.findIndex(
    (tool) =>
      tool.kind === "permission" &&
      tool.status === "Running" &&
      !tool.permission_decision &&
      isPlanApprovalPermission(tool) &&
      !!findPlanAcceptOption(tool.permission_options) &&
      !!findPlanRejectOption(tool.permission_options),
  );

  const tool = toolIndex >= 0 ? tools[toolIndex] : null;
  if (!tool) {
    return null;
  }

  return {
    requestId: tool.call_id,
    planText: planApprovalText(tool, tools, toolIndex),
    options: tool.permission_options,
  };
}

export function findPendingPermissionRequest(tools: ToolInvocation[]): PendingPermissionRequest | null {
  return findPendingPermissionRequests(tools)[0] ?? null;
}

export function findPendingPermissionRequests(tools: ToolInvocation[]): PendingPermissionRequest[] {
  return tools.flatMap((tool, index) => {
    if (!isPendingPermissionTool(tool)) {
      return [];
    }
    const planApproval = isPlanApprovalPermission(tool);
    const planText = planApproval ? planApprovalText(tool, tools, index) : null;
    const details = permissionRequestDetails(tool, planText);

    return [{
      requestId: tool.call_id,
      title: permissionRequestTitle(tool, details),
      details,
      planText,
      options: tool.permission_options,
      isPlanApproval: planApproval,
    }];
  });
}

export function pendingPermissionRequestIds(tools: ToolInvocation[]) {
  return tools.filter(isPendingPermissionTool).map((tool) => tool.call_id);
}

function isPendingPermissionTool(tool: ToolInvocation) {
  return (
    tool.kind === "permission" &&
    tool.status === "Running" &&
    !tool.permission_decision &&
    tool.permission_options.length > 0
  );
}

function permissionRequestTitle(tool: ToolInvocation, details: string | null) {
  const name = tool.name.trim();
  const baseTitle = !name || name.toLowerCase() === "permission request" ? "选择权限" : name;
  const path = permissionRequestTitlePath(details);
  if (path && !baseTitle.includes(path)) {
    return `${baseTitle}: ${path}`;
  }
  return baseTitle;
}

function permissionRequestDetails(tool: ToolInvocation, planText: string | null) {
  const detailText = tool.detail_text.trim();
  if (detailText && detailText !== planText) {
    return detailText;
  }

  const rawInput = tool.raw_input?.trim();
  if (rawInput && rawInput !== planText && !looksLikeJsonPayload(rawInput)) {
    return rawInput;
  }

  return null;
}

function permissionRequestTitlePath(details: string | null) {
  if (!details) return null;
  const lines = details.split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index].trim();
    const inlinePath = line.match(/^Path:\s*(.+)$/i)?.[1]?.trim();
    if (inlinePath) {
      return inlinePath;
    }
    if (/^Paths:\s*$/i.test(line)) {
      for (let next = index + 1; next < lines.length; next += 1) {
        const path = lines[next].trim().replace(/^-\s*/, "").trim();
        if (path) {
          return path;
        }
      }
    }
    const firstListPath = line.match(/^Paths:\s*-\s*(.+)$/i)?.[1]?.trim();
    if (firstListPath) {
      return firstListPath;
    }
  }
  return null;
}

function planApprovalText(tool: ToolInvocation, tools: ToolInvocation[] = [], toolIndex = -1) {
  const rawInputPlan = extractPlanText(tool.raw_input);
  if (rawInputPlan) {
    return rawInputPlan;
  }

  const detailPlan = extractStructuredPlanText(tool.detail_text);
  if (detailPlan) {
    return detailPlan;
  }

  const nearbyPlan = latestCodeBuddyPlanText(tools, toolIndex);
  if (nearbyPlan) {
    return nearbyPlan;
  }

  const detailText = tool.detail_text.trim();
  if (detailText && looksLikePlanBody(detailText)) {
    return detailText;
  }

  const rawInput = tool.raw_input?.trim();
  if (rawInput && !looksLikeJsonPayload(rawInput) && looksLikePlanBody(rawInput)) {
    return rawInput;
  }

  return null;
}

function latestCodeBuddyPlanText(tools: ToolInvocation[], toolIndex: number) {
  const end = toolIndex >= 0 ? toolIndex : tools.length;
  for (let index = end - 1; index >= 0; index -= 1) {
    const tool = tools[index];
    if (!looksLikeCodeBuddyPlanWriteTool(tool)) {
      continue;
    }
    const planText = structuredPlanTextFromTool(tool);
    if (planText) {
      return planText;
    }
  }
  return null;
}

function looksLikeCodeBuddyPlanWriteTool(tool: ToolInvocation) {
  const haystack = [
    tool.name,
    tool.summary,
    tool.detail_text,
    tool.raw_input,
    tool.raw_output,
    ...tool.diff_paths,
    ...tool.logs.flatMap((log) => [log.title, log.body]),
  ]
    .filter(Boolean)
    .join("\n")
    .toLowerCase();

  return (
    haystack.includes(".codebuddy/plans/") ||
    haystack.includes(".codebuddy\\plans\\") ||
    (haystack.includes("write") && haystack.includes("plan")) ||
    haystack.includes("implementation plan")
  );
}

function structuredPlanTextFromTool(tool: ToolInvocation) {
  const payloads = [
    tool.raw_input,
    tool.raw_output,
    tool.detail_text,
    ...tool.logs.flatMap((log) => [log.body, log.title]),
  ];

  for (const payload of payloads) {
    const planText = extractStructuredPlanText(payload);
    if (planText) {
      return planText;
    }
  }

  return null;
}

function extractStructuredPlanText(payload: string | null | undefined) {
  const trimmed = payload?.trim();
  if (!trimmed) {
    return null;
  }

  try {
    return structuredPlanTextFromParsedPayload(JSON.parse(trimmed));
  } catch {
    return looksLikePlanBody(trimmed) ? trimmed : null;
  }
}

function extractPlanText(payload: string | null | undefined) {
  const trimmed = payload?.trim();
  if (!trimmed) {
    return null;
  }

  try {
    return planTextFromParsedPayload(JSON.parse(trimmed));
  } catch {
    return trimmed;
  }
}

function planTextFromParsedPayload(payload: unknown): string | null {
  if (typeof payload === "string") {
    const trimmed = payload.trim();
    return trimmed || null;
  }

  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  const record = payload as Record<string, unknown>;
  const plan = stringValue(record.plan);
  if (plan) {
    return plan;
  }

  const rawResponse = record["codebuddy.ai/rawResponse"];
  if (rawResponse && typeof rawResponse === "object" && !Array.isArray(rawResponse)) {
    return stringValue((rawResponse as Record<string, unknown>).plan);
  }

  return null;
}

function structuredPlanTextFromParsedPayload(payload: unknown): string | null {
  if (typeof payload === "string") {
    const trimmed = payload.trim();
    return looksLikePlanBody(trimmed) ? trimmed : null;
  }

  if (!payload || typeof payload !== "object") {
    return null;
  }

  if (Array.isArray(payload)) {
    return payload.map(structuredPlanTextFromParsedPayload).find(Boolean) ?? null;
  }

  const record = payload as Record<string, unknown>;
  const explicitPlan = stringValue(record.plan);
  if (explicitPlan) {
    return explicitPlan;
  }

  for (const key of ["content", "newText", "new_text", "text", "markdown", "body"]) {
    const value = stringValue(record[key]);
    if (value && looksLikePlanBody(value)) {
      return value;
    }
  }

  for (const key of ["rawInput", "raw_input", "input", "rawOutput", "raw_output", "content"]) {
    const nested = record[key];
    if (nested && typeof nested === "object") {
      const planText = structuredPlanTextFromParsedPayload(nested);
      if (planText) {
        return planText;
      }
    }
  }

  const rawResponse = record["codebuddy.ai/rawResponse"];
  if (rawResponse && typeof rawResponse === "object") {
    const planText = structuredPlanTextFromParsedPayload(rawResponse);
    if (planText) {
      return planText;
    }
  }

  return null;
}

function looksLikePlanBody(value: string) {
  const trimmed = value.trim();
  if (!trimmed || looksLikeCodeBuddyPlanPath(trimmed)) {
    return false;
  }
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    return false;
  }
  if (/^(python|node|bash|pwsh|powershell|cmd)\b/i.test(trimmed)) {
    return false;
  }
  return (
    trimmed.startsWith("#") ||
    trimmed.includes("\n") ||
    /计划|实施|步骤|目标|验证|plan|problem|implementation|approach/i.test(trimmed)
  );
}

function looksLikeCodeBuddyPlanPath(value: string) {
  const normalized = value.trim().replace(/\\/g, "/").toLowerCase();
  return normalized.includes(".codebuddy/plans/") && /\.mdx?$/.test(normalized);
}

function stringValue(value: unknown) {
  if (typeof value !== "string") {
    return null;
  }
  const trimmed = value.trim();
  return trimmed || null;
}

function looksLikeJsonPayload(value: string) {
  return value.startsWith("{") || value.startsWith("[");
}

function isPlanApprovalPermission(tool: ToolInvocation) {
  if (tool.name.toLowerCase() === "exitplanmode") {
    return true;
  }
  return tool.permission_options.some((option) =>
    ["plan", "reject_and_exit_plan", "rejectAndExitPlan"].includes(option.id),
  );
}
