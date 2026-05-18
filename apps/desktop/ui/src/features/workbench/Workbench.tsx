import { useState, useEffect, useCallback } from "react";
import type { UiSnapshot, AppTheme } from "../../types";
import { startupPerfMark, sessionResolvePermission, settingsGetAgentSnapshot } from "../../lib/tauri";
import { ConversationTimeline } from "../conversation/ConversationTimeline";
import { Composer, type ComposerReferenceRequest } from "../composer/Composer";
import { AgentPlanPanel } from "../composer/AgentPlanPanel";
import { ReviewPanel } from "../review/ReviewPanel";
import { DiffTab } from "../editor/DiffTab";
import { EditorView } from "../editor/EditorView";
import { WelcomeLauncher } from "./WelcomeLauncher";
import { SessionList } from "../session/SessionList";
import { TabBar } from "./TabBar";
import { GlobalChrome } from "./GlobalChrome";
import { ThreadHeader } from "./ThreadHeader";
import { ThreadSidebarShell } from "./ThreadSidebarShell";
import { SettingsPage } from "../settings/SettingsPage";
import { TerminalDock } from "../terminal/TerminalDock";
import { applyAppTheme, DEFAULT_APP_THEME } from "../../theme";
import { useWorkbenchSnapshot } from "./useWorkbenchSnapshot";
import { useWorkbenchGit } from "./useWorkbenchGit";
import { useTimelineChangeSets } from "./useTimelineChangeSets";
import { useWorkbenchTabs } from "./useWorkbenchTabs";
import { useRightPanelState } from "./useRightPanelState";
import { useTerminalDockState } from "./useTerminalDockState";
import "./Workbench.css";

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
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const {
    rightPanelCollapsed,
    setRightPanelCollapsed,
    rightPanelWidth,
    rightPanelStyle,
    clampStoredRightPanelWidth,
    handleRightPanelResizeStart,
  } = useRightPanelState();
  const {
    terminalDockVisible,
    terminalDockMounted,
    terminalDockHeight,
    handleToggleTerminalDock,
    handleHideTerminalDock,
    handleTerminalDockHeightChange,
  } = useTerminalDockState(snapshot, snapshotRef);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);

  useEffect(() => {
    settingsGetAgentSnapshot()
      .then((snapshot) => setAppTheme(applyAppTheme(snapshot.settings.theme)))
      .catch(() => setAppTheme(applyAppTheme(DEFAULT_APP_THEME)));
  }, []);

  useEffect(() => {
    window.addEventListener("resize", clampStoredRightPanelWidth);
    return () => window.removeEventListener("resize", clampStoredRightPanelWidth);
  }, [clampStoredRightPanelWidth]);

  useEffect(() => {
    const handleOpenSettings = () => setSettingsOpen(true);
    window.addEventListener("kodex:open-settings", handleOpenSettings);
    return () => window.removeEventListener("kodex:open-settings", handleOpenSettings);
  }, []);

  const handleWorkspaceOpened = useCallback((nextSnapshot: UiSnapshot) => {
    void startupPerfMark("workbench/handle_workspace_opened", "");
    acceptSnapshot(nextSnapshot);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    setRightPanelCollapsed(false);
  }, [acceptSnapshot, clearChangeSets, resetGitHydration, resetTabs]);

  const handleWorkspaceChanged = useCallback((nextSnapshot: UiSnapshot) => {
    acceptSnapshot(nextSnapshot);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
  }, [acceptSnapshot, clearChangeSets, resetGitHydration, resetTabs]);

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
    pollState();
  }, [clearChangeSets, clearSnapshot, pollState, resetGitHydration, resetTabs]);

  const handleToggleThreads = useCallback(() => {
    setSidebarCollapsed((collapsed) => !collapsed);
  }, []);

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

  // No workspace loaded — show welcome screen
  if (!workspaceReady) {
    return <WelcomeLauncher onWorkspaceOpened={handleWorkspaceOpened} />;
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

  if (settingsOpen) {
    return (
      <div className="workbench">
        <SettingsPage onBack={() => setSettingsOpen(false)} onThemeChange={setAppTheme} />
      </div>
    );
  }

  const agentLabel = snapshot.session.agent_cli || "智能体";

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
        onToggleRightPanel={() => setRightPanelCollapsed((collapsed) => !collapsed)}
        onFileOpen={handleSearchResultOpen}
      />

      <div className="workbench-content">
        <ThreadSidebarShell collapsed={sidebarCollapsed}>
          <SessionList
            activeSessionId={snapshot.session.id}
            activeSessionTitle={snapshot.session.title}
            activeWorkspaceRoot={snapshot.workspace.root}
            currentSessionStatus={snapshot.session.status}
            onOpenSettings={() => setSettingsOpen(true)}
            onSessionChanged={handleSessionChanged}
            onWorkspaceChanged={handleWorkspaceChanged}
          />
        </ThreadSidebarShell>

        <div className="workbench-main-shell">

        <div
          className={`workbench-body ${terminalDockVisible ? "has-terminal-dock" : ""}`}
          style={rightPanelStyle}
        >
          <main className="center-panel">
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

            <div className={`conversation-container ${activeTab.type === "conversation" ? "" : "is-workspace-tab"}`}>
              {activeTab.type === "conversation" ? (
                <>
                  <ConversationTimeline
                    snapshot={snapshot}
                    onPermissionSelect={handlePermissionSelect}
                    turnChangeSetsByMessageId={timelineTurnChangeSets}
                    onReviewFileSelect={(path, changeSetId) =>
                      handleOpenDiffTab(path, "change-set", undefined, changeSetId)
                    }
                    planPanel={<AgentPlanPanel entries={snapshot.agent_plan ?? []} />}
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
              <Composer
                snapshot={snapshot}
                onStateChange={pollState}
                referenceRequests={composerReferenceRequests}
                onReferenceRequestConsumed={handleComposerReferenceConsumed}
              />
            </div>
          </main>

          {!rightPanelCollapsed && (
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

          <aside className={`right-panel ${rightPanelCollapsed ? "is-collapsed" : ""}`}>
            <ReviewPanel
              snapshot={snapshot}
              refreshing={gitRefreshing}
              hydrated={gitHydrated}
              appTheme={appTheme}
              onRefresh={handleRefreshGit}
              onFileSelect={(path, changeSetId) =>
                handleOpenDiffTab(path, "git", undefined, changeSetId)
              }
              onFileOpen={handleOpenEditorTab}
              onAddComposerReference={(path) => enqueueComposerReference({ path })}
            />
          </aside>
        </div>
        {terminalDockMounted && (
          <TerminalDock
            key={snapshot.workspace.root}
            workspaceRoot={snapshot.workspace.root}
            appTheme={appTheme}
            visible={terminalDockVisible}
            height={terminalDockHeight}
            layoutSignal={`${rightPanelWidth}:${rightPanelCollapsed}`}
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
      </div>
      </div>
    </div>
  );
}
