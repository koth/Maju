import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import type { UiSnapshot, TabDescriptor, SessionFileChange, AppTheme } from "../../types";
import { sessionGetState, gitRefresh, sessionResolvePermission, reviewGetGitDiffContent, settingsGetAgentSnapshot } from "../../lib/tauri";
import { onUiSnapshot } from "../../lib/events";
import { ConversationTimeline } from "../conversation/ConversationTimeline";
import { Composer } from "../composer/Composer";
import { AgentPlanPanel } from "../composer/AgentPlanPanel";
import { ReviewPanel } from "../review/ReviewPanel";
import { DiffTab } from "../editor/DiffTab";
import { EditorView } from "../editor/EditorView";
import { ChangesBar } from "../changes/ChangesBar";
import { WelcomeLauncher } from "./WelcomeLauncher";
import { SessionList } from "../session/SessionList";
import { TabBar } from "./TabBar";
import { AppRail } from "./AppRail";
import { GlobalChrome } from "./GlobalChrome";
import { ThreadHeader } from "./ThreadHeader";
import { ThreadSidebarShell } from "./ThreadSidebarShell";
import { SettingsPage } from "../settings/SettingsPage";
import { applyAppTheme, DEFAULT_APP_THEME } from "../../theme";
import "./Workbench.css";

const CONVERSATION_TAB: TabDescriptor = {
  id: "conversation",
  type: "conversation",
  label: "智能体",
};

export function Workbench() {
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(null);
  const [tabs, setTabs] = useState<TabDescriptor[]>([CONVERSATION_TAB]);
  const [activeTabId, setActiveTabId] = useState("conversation");
  const [workspaceReady, setWorkspaceReady] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [rightPanelCollapsed, setRightPanelCollapsed] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);
  const [gitRefreshing, setGitRefreshing] = useState(false);
  const gitRefreshInFlight = useRef(false);
  const prevSnapshotJson = useRef<string>("");

  useEffect(() => {
    settingsGetAgentSnapshot()
      .then((snapshot) => setAppTheme(applyAppTheme(snapshot.settings.theme)))
      .catch(() => setAppTheme(applyAppTheme(DEFAULT_APP_THEME)));
  }, []);

  const pollState = useCallback(async () => {
    try {
      const state = await sessionGetState();
      const json = JSON.stringify(state);
      if (json !== prevSnapshotJson.current) {
        prevSnapshotJson.current = json;
        setSnapshot(state);
      }
    } catch {
      // No workspace open — stay on welcome screen
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    onUiSnapshot((nextSnapshot) => {
      if (disposed) return;
      prevSnapshotJson.current = JSON.stringify(nextSnapshot);
      setWorkspaceReady(true);
      setSnapshot(nextSnapshot);
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
  }, []);

  useEffect(() => {
    if (!workspaceReady) return;
    pollState();
    const interval = setInterval(pollState, 2000);
    return () => clearInterval(interval);
  }, [pollState, workspaceReady]);

  const handleWorkspaceOpened = useCallback(() => {
    setWorkspaceReady(true);
    setSnapshot(null);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    setRightPanelCollapsed(false);
  }, []);

  const handleWorkspaceChanged = useCallback((nextSnapshot: UiSnapshot) => {
    prevSnapshotJson.current = JSON.stringify(nextSnapshot);
    setWorkspaceReady(true);
    setSnapshot(nextSnapshot);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
  }, []);

  const handleRefreshGit = useCallback(async () => {
    if (gitRefreshInFlight.current) return;
    gitRefreshInFlight.current = true;
    setGitRefreshing(true);
    try {
      const repo = await gitRefresh();
      setSnapshot((prev) => (prev ? { ...prev, repository: repo } : prev));
    } catch {
      // ignored
    } finally {
      gitRefreshInFlight.current = false;
      setGitRefreshing(false);
    }
  }, []);

  const handlePermissionSelect = useCallback(async (requestId: string, optionId: string | null) => {
    await sessionResolvePermission(requestId, optionId);
    await pollState();
  }, [pollState]);

  const handleSessionChanged = useCallback(() => {
    prevSnapshotJson.current = "";
    setSnapshot(null);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    pollState();
  }, [pollState]);

  const handleToggleThreads = useCallback(() => {
    setSidebarCollapsed((collapsed) => !collapsed);
  }, []);

  // ── Tab management ──

  const handleOpenDiffTab = useCallback(
    (path: string) => {
      const tabId = `diff:${path}`;
      setTabs((prev) => {
        if (prev.some((t) => t.id === tabId)) return prev;
        const fileName = path.replace(/\\/g, "/").split("/").pop() || path;
        return [
          ...prev,
          { id: tabId, type: "diff" as const, label: fileName, filePath: path },
        ];
      });
      setActiveTabId(tabId);
    },
    [],
  );

  const handleOpenEditorTab = useCallback(
    (filePath: string) => {
      const tabId = `editor:${filePath}`;
      setTabs((prev) => {
        if (prev.some((t) => t.id === tabId)) return prev;
        const fileName = filePath.replace(/\\/g, "/").split("/").pop() || filePath;
        return [
          ...prev,
          { id: tabId, type: "editor" as const, label: fileName, filePath },
        ];
      });
      setActiveTabId(tabId);
    },
    [],
  );

  const navTokenRef = useRef(0);

  const handleSearchResultOpen = useCallback(
    (filePath: string, lineNumber?: number, searchQuery?: string) => {
      const tabId = `editor:${filePath}`;
      const token = ++navTokenRef.current;
      setTabs((prev) => {
        const existing = prev.find((t) => t.id === tabId);
        if (existing) {
          return prev.map((t) =>
            t.id === tabId ? { ...t, lineNumber, searchQuery, navToken: token } : t,
          );
        }
        const fileName = filePath.replace(/\\/g, "/").split("/").pop() || filePath;
        return [
          ...prev,
          { id: tabId, type: "editor" as const, label: fileName, filePath, lineNumber, searchQuery, navToken: token },
        ];
      });
      setActiveTabId(tabId);
    },
    [],
  );

  const handleCloseTab = useCallback(
    (id: string) => {
      if (id === "conversation") return;
      setTabs((prev) => {
        const filtered = prev.filter((t) => t.id !== id);
        if (activeTabId === id) {
          const idx = prev.findIndex((t) => t.id === id);
          const newActive = filtered[Math.min(idx, filtered.length - 1)]?.id ?? "conversation";
          setActiveTabId(newActive);
        }
        return filtered;
      });
    },
    [activeTabId],
  );

  const handleTabSelect = useCallback((id: string) => {
    setActiveTabId(id);
  }, []);

  // Computed before conditional returns — hooks must be unconditional
  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  const isDiffTab = activeTab.type === "diff" && activeTab.filePath != null;

  const [gitDiffChange, setGitDiffChange] = useState<SessionFileChange | null>(null);

  const activeDiffChange = useMemo(() => {
    if (isDiffTab && activeTab.filePath) {
      const fromSession = snapshot?.session_changes?.find((c) => c.path === activeTab.filePath) ?? null;
      if (fromSession) return fromSession;
    }
    return null;
  }, [isDiffTab, activeTab.filePath, snapshot?.session_changes]);

  // Fetch git diff content when no session change
  useEffect(() => {
    if (isDiffTab && activeTab.filePath && !activeDiffChange) {
      setGitDiffChange(null);
      reviewGetGitDiffContent(activeTab.filePath).then(setGitDiffChange).catch(() => setGitDiffChange(null));
    } else {
      setGitDiffChange(null);
    }
  }, [isDiffTab, activeTab.filePath, activeDiffChange]);

  const resolvedDiffChange = activeDiffChange ?? gitDiffChange;

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

  // Use agent CLI label for conversation tab display
  const agentLabel = snapshot.session.agent_cli || "智能体";
  const displayTabs = tabs.map((t) =>
    t.type === "conversation" ? { ...t, label: agentLabel } : t,
  );

  return (
    <div className="workbench">
      <AppRail
        sidebarCollapsed={sidebarCollapsed}
        onToggleThreads={handleToggleThreads}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <ThreadSidebarShell collapsed={sidebarCollapsed}>
        <SessionList
          activeSessionId={snapshot.session.id}
          activeWorkspaceRoot={snapshot.workspace.root}
          currentSessionStatus={snapshot.session.status}
          onSessionChanged={handleSessionChanged}
          onWorkspaceChanged={handleWorkspaceChanged}
        />
      </ThreadSidebarShell>

      <div className="workbench-main-shell">
        <GlobalChrome
          workspace={snapshot.workspace}
          refreshing={gitRefreshing}
          rightPanelCollapsed={rightPanelCollapsed}
          onRefreshGit={handleRefreshGit}
          onToggleRightPanel={() => setRightPanelCollapsed((collapsed) => !collapsed)}
          onFileOpen={handleSearchResultOpen}
        />

        <TabBar
          tabs={displayTabs}
          activeTabId={activeTabId}
          onTabSelect={handleTabSelect}
          onTabClose={handleCloseTab}
        />

        <div className="workbench-body">
          <main className="center-panel">
            {activeTab.type === "conversation" && (
              <ThreadHeader
                session={snapshot.session}
                workspace={snapshot.workspace}
                activeTabLabel={agentLabel}
                changeCount={snapshot.session_changes?.length ?? 0}
              />
            )}

            {activeTab.type === "conversation" && (
              <div className="conversation-container">
                <ConversationTimeline
                  snapshot={snapshot}
                  onPermissionSelect={handlePermissionSelect}
                />
                <AgentPlanPanel entries={snapshot.agent_plan ?? []} />
                <ChangesBar
                  changes={snapshot.session_changes ?? []}
                  onFileSelect={handleOpenDiffTab}
                />
                <Composer
                  snapshot={snapshot}
                  onStateChange={pollState}
                  onWorkspaceChanged={handleWorkspaceChanged}
                />
              </div>
            )}
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
              />
            )}
          </main>

          <aside className={`right-panel ${rightPanelCollapsed ? "is-collapsed" : ""}`}>
            <ReviewPanel
              snapshot={snapshot}
              refreshing={gitRefreshing}
              onRefresh={handleRefreshGit}
              onFileSelect={handleOpenDiffTab}
              onFileOpen={handleOpenEditorTab}
            />
          </aside>
        </div>
      </div>
    </div>
  );
}
