import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type { UiSnapshot, AppTheme, ToolInvocation } from "../../types";
import { startupPerfMark, sessionResolvePermission, settingsGetAgentSnapshot } from "../../lib/tauri";
import { ConversationTimeline } from "../conversation/ConversationTimeline";
import { Composer, type ComposerReferenceRequest } from "../composer/Composer";
import {
  AgentPlanPanel,
  PlanApprovalModal,
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
import { ThreadHeader } from "./ThreadHeader";
import { ThreadSidebarShell } from "./ThreadSidebarShell";
import { SettingsPage, type AgentSettingsTab, type SettingsStartupNotice } from "../settings/SettingsPage";
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
  const [settingsStartupNotice, setSettingsStartupNotice] = useState<SettingsStartupNotice | null>(null);
  const [settingsInitialAgentTab, setSettingsInitialAgentTab] = useState<AgentSettingsTab | null>(null);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);
  const [startupUpdateInfo, setStartupUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [startupUpdateDismissed, setStartupUpdateDismissed] = useState(false);

  const handleOpenSettings = useCallback((options?: {
    startupNotice?: SettingsStartupNotice;
    initialAgentTab?: AgentSettingsTab;
  }) => {
    setSettingsStartupNotice(options?.startupNotice ?? null);
    setSettingsInitialAgentTab(options?.initialAgentTab ?? null);
    setSettingsOpen(true);
  }, []);

  const handleCloseSettings = useCallback(() => {
    setSettingsOpen(false);
    setSettingsStartupNotice(null);
    setSettingsInitialAgentTab(null);
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
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
  }, [acceptSnapshot, clearChangeSets, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handlePermissionSelect = useCallback(async (requestId: string, optionId: string | null) => {
    await sessionResolvePermission(requestId, optionId);
    await pollState();
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

  const pendingPlanApproval = useMemo(
    () => (snapshot ? findPendingPlanApproval(snapshot.tools) : null),
    [snapshot?.tools],
  );
  const hiddenPermissionRequestIds = useMemo(
    () =>
      pendingPlanApproval
        ? new Set<string>([pendingPlanApproval.requestId])
        : new Set<string>(),
    [pendingPlanApproval],
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
          initialAgentTab={settingsInitialAgentTab ?? undefined}
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

  const agentLabel = snapshot.session.agent_cli || "智能体";
  const showComposerPlan = shouldShowAgentPlanNearComposer(snapshot);
  const workbenchBodyClassName = [
    "workbench-body",
    terminalDockVisible ? "has-terminal-dock" : "",
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
        sidebarCollapsed={sidebarCollapsed}
        refreshing={gitRefreshing}
        rightPanelCollapsed={rightPanelCollapsed}
        terminalDockVisible={terminalDockVisible}
        onToggleSidebar={handleToggleThreads}
        onToggleTerminal={handleToggleTerminalDock}
        onRefreshGit={handleRefreshGit}
        onToggleRightPanel={handleToggleRightPanel}
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
                {showComposerPlan && (
                  <div className="composer-plan-slot">
                    <AgentPlanPanel entries={snapshot.agent_plan} />
                  </div>
                )}
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
                        onAddComposerReference={enqueueComposerReference}
                      />
                    )}
                  </section>
                )}
                {showComposerPlan && (
                  <div className="composer-plan-slot">
                    <AgentPlanPanel entries={snapshot.agent_plan} />
                  </div>
                )}
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
        {terminalDockMounted && (
          <TerminalDock
            key={snapshot.workspace.root}
            workspaceRoot={snapshot.workspace.root}
            appTheme={appTheme}
            visible={terminalDockVisible}
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
        <PlanApprovalModal
          approval={pendingPlanApproval}
          entries={snapshot.agent_plan ?? []}
          onPermissionSelect={handlePermissionSelect}
        />
        {updateNotice}
      </div>
      </div>
    </div>
  );
}

function findPendingPlanApproval(tools: ToolInvocation[]) {
  const tool = tools.find(
    (tool) =>
      tool.kind === "permission" &&
      tool.status === "Running" &&
      !tool.permission_decision &&
      tool.permission_options.some((option) => option.id === "plan") &&
      tool.permission_options.some(
        (option) => option.id === "default" || option.kind.toLowerCase().includes("allow"),
      ),
  );

  if (!tool) {
    return null;
  }

  return {
    requestId: tool.call_id,
    planText: tool.raw_input || tool.detail_text || null,
    options: tool.permission_options,
  };
}
