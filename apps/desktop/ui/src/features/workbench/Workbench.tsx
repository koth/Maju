import { useState, useEffect, useCallback, useRef } from "react";
import type { UiSnapshot, TabDescriptor } from "../../types";
import { sessionGetState, gitRefresh, sessionResolvePermission } from "../../lib/tauri";
import { ConversationTimeline } from "../conversation/ConversationTimeline";
import { Composer } from "../composer/Composer";
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
import "./Workbench.css";

const CONVERSATION_TAB: TabDescriptor = {
  id: "conversation",
  type: "conversation",
  label: "Agent",
};

export function Workbench() {
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(null);
  const [tabs, setTabs] = useState<TabDescriptor[]>([CONVERSATION_TAB]);
  const [activeTabId, setActiveTabId] = useState("conversation");
  const [workspaceReady, setWorkspaceReady] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [rightPanelCollapsed, setRightPanelCollapsed] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [gitRefreshing, setGitRefreshing] = useState(false);
  const gitRefreshInFlight = useRef(false);
  const prevSnapshotJson = useRef<string>("");

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
    if (!workspaceReady) return;
    pollState();
    const interval = setInterval(pollState, 500);
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

  // No workspace loaded — show welcome screen
  if (!workspaceReady) {
    return <WelcomeLauncher onWorkspaceOpened={handleWorkspaceOpened} />;
  }

  if (!snapshot) {
    return (
      <div className="workbench">
        <div className="workbench-loading">Loading...</div>
      </div>
    );
  }

  if (settingsOpen) {
    return (
      <div className="workbench">
        <SettingsPage onBack={() => setSettingsOpen(false)} />
      </div>
    );
  }

  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0];

  // Use agent CLI label for conversation tab display
  const agentLabel = snapshot.session.agent_cli || "Agent";
  const displayTabs = tabs.map((t) =>
    t.type === "conversation" ? { ...t, label: agentLabel } : t,
  );

  // Find the SessionFileChange for a diff tab
  const activeDiffChange =
    activeTab.type === "diff" && activeTab.filePath
      ? snapshot.session_changes?.find((c) => c.path === activeTab.filePath) ?? null
      : null;

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
          workspace={snapshot.workspace}
          onSessionChanged={handleSessionChanged}
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
            {activeTab.type === "diff" && activeDiffChange && (
              <DiffTab change={activeDiffChange} />
            )}
            {activeTab.type === "diff" && !activeDiffChange && (
              <div className="workbench-loading">
                No diff data available for this file
              </div>
            )}
            {activeTab.type === "editor" && activeTab.filePath && (
              <EditorView
                path={activeTab.filePath}
                lineNumber={activeTab.lineNumber}
                searchQuery={activeTab.searchQuery}
                navToken={activeTab.navToken}
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
