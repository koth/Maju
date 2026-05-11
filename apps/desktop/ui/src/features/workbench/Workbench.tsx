import { useState, useEffect, useCallback, useRef } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import type { UiSnapshot, UiSnapshotPatch, TabDescriptor, SessionFileChange, AppTheme } from "../../types";
import { startupPerfMark, sessionGetState, gitRefresh, sessionResolvePermission, reviewGetGitDiffContent, sessionGetFileDiff, settingsGetAgentSnapshot, editorSaveFile } from "../../lib/tauri";
import { onUiSnapshot, onUiSnapshotPatch } from "../../lib/events";
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
import { GlobalChrome } from "./GlobalChrome";
import { ThreadHeader } from "./ThreadHeader";
import { ThreadSidebarShell } from "./ThreadSidebarShell";
import { SettingsPage } from "../settings/SettingsPage";
import { applyAppTheme, DEFAULT_APP_THEME } from "../../theme";
import { disposeModel, getModelBaseVersion, getModelValue, isModelDirty, updateModelBase, updateModelBaseVersion } from "../editor/monaco-model-registry";
import {
  appendStreamingMessageDelta,
  getStreamingMessageBody,
} from "../conversation/streaming-message-store";
import "./Workbench.css";

const CONVERSATION_TAB: TabDescriptor = {
  id: "conversation",
  type: "conversation",
  label: "Chat",
};

const RIGHT_PANEL_WIDTH_STORAGE_KEY = "kodex.rightPanelWidth";
const RIGHT_PANEL_DEFAULT_WIDTH = 292;
const RIGHT_PANEL_MIN_WIDTH = 248;
const RIGHT_PANEL_MAX_WIDTH = 520;

function applySnapshotPatch(snapshot: UiSnapshot, patch: UiSnapshotPatch): UiSnapshot {
  const messages =
    patch.messages.length === 0
      ? snapshot.messages
      : mergeById(snapshot.messages, patch.messages);
  const tools =
    patch.tools.length === 0
      ? snapshot.tools
      : mergeById(snapshot.tools, patch.tools);
  const timeline =
    patch.timeline.length === 0 && patch.timeline_start === snapshot.timeline.length
      ? snapshot.timeline
      : [...snapshot.timeline.slice(0, patch.timeline_start), ...patch.timeline];

  return {
    ...snapshot,
    revision: patch.revision,
    session: patch.session,
    session_config: patch.session_config,
    prompt_capabilities: patch.prompt_capabilities,
    available_commands: patch.available_commands,
    agent_plan: patch.agent_plan,
    messages,
    timeline,
    tools,
    inspector_tab: patch.inspector_tab,
    inspector_sections: patch.inspector_sections,
    session_changes: patch.session_changes,
    thinking_status: patch.thinking_status,
  };
}

function mergeById<T extends { id: string }>(current: T[], updates: T[]): T[] {
  if (updates.length === 0) return current;
  const next = current.slice();
  const appended: T[] = [];

  for (const update of updates) {
    const index = next.findIndex((item) => item.id === update.id);
    if (index >= 0) {
      if (next[index] !== update) {
        next[index] = update;
      }
    } else {
      appended.push(update);
    }
  }

  return appended.length === 0 ? next : [...next, ...appended];
}

function applyStreamingDeltas(patch: UiSnapshotPatch) {
  for (const delta of patch.message_deltas ?? []) {
    appendStreamingMessageDelta(delta.id, delta.append);
  }
}

function isStreamingDeltaOnlyPatch(patch: UiSnapshotPatch) {
  return (
    patch.session.status === "Streaming" &&
    (patch.message_deltas?.length ?? 0) > 0 &&
    patch.messages.length === 0 &&
    patch.timeline.length === 0 &&
    patch.tools.length === 0
  );
}

function materializeStreamingMessageBodies(snapshot: UiSnapshot): UiSnapshot {
  let changed = false;
  const messages = snapshot.messages.map((message) => {
    const streamingBody = getStreamingMessageBody(message.id);
    if (streamingBody == null || streamingBody === message.body) {
      return message;
    }
    changed = true;
    return { ...message, body: streamingBody };
  });
  return changed ? { ...snapshot, messages } : snapshot;
}

function sessionChangesSignature(changes: SessionFileChange[] | undefined) {
  if (!changes || changes.length === 0) return "";
  return changes
    .map((change) =>
      [
        change.path,
        change.change_type,
        change.added_lines,
        change.removed_lines,
      ].join(":"),
    )
    .sort()
    .join("|");
}

interface PendingCloseTab {
  id: string;
  label: string;
  filePath: string;
}

function clampRightPanelWidth(width: number) {
  return Math.min(RIGHT_PANEL_MAX_WIDTH, Math.max(RIGHT_PANEL_MIN_WIDTH, width));
}

export function Workbench() {
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(null);
  const [tabs, setTabs] = useState<TabDescriptor[]>([CONVERSATION_TAB]);
  const [activeTabId, setActiveTabId] = useState("conversation");
  const [workspaceReady, setWorkspaceReady] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [rightPanelCollapsed, setRightPanelCollapsed] = useState(false);
  const [rightPanelWidth, setRightPanelWidth] = useState(() => {
    const stored = Number(window.localStorage.getItem(RIGHT_PANEL_WIDTH_STORAGE_KEY));
    return Number.isFinite(stored) ? clampRightPanelWidth(stored) : RIGHT_PANEL_DEFAULT_WIDTH;
  });
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);
  const [gitRefreshing, setGitRefreshing] = useState(false);
  const [gitHydrated, setGitHydrated] = useState(false);
  const [pendingCloseTab, setPendingCloseTab] = useState<PendingCloseTab | null>(null);
  const gitRefreshInFlight = useRef(false);
  const gitRefreshPending = useRef(false);
  const gitHydrationKey = useRef(0);
  const sessionChangesRefreshRef = useRef<{
    workspaceRoot: string;
    signature: string;
  } | null>(null);
  const prevSnapshotRevision = useRef<number>(0);
  const snapshotRef = useRef<UiSnapshot | null>(null);
  const firstSnapshotLogged = useRef(false);
  const firstWorkspaceReadyLogged = useRef(false);

  useEffect(() => {
    snapshotRef.current = snapshot;
    if (snapshot && !firstSnapshotLogged.current) {
      firstSnapshotLogged.current = true;
      void startupPerfMark(
        "workbench/first_snapshot_committed",
        `revision=${snapshot.revision} messages=${snapshot.messages.length} tools=${snapshot.tools.length} timeline=${snapshot.timeline.length}`,
      );
      requestAnimationFrame(() => {
        void startupPerfMark("workbench/first_snapshot_painted", `performance_now=${performance.now().toFixed(1)}`);
      });
    }
  }, [snapshot]);

  useEffect(() => {
    settingsGetAgentSnapshot()
      .then((snapshot) => setAppTheme(applyAppTheme(snapshot.settings.theme)))
      .catch(() => setAppTheme(applyAppTheme(DEFAULT_APP_THEME)));
  }, []);

  useEffect(() => {
    const handleOpenSettings = () => setSettingsOpen(true);
    window.addEventListener("kodex:open-settings", handleOpenSettings);
    return () => window.removeEventListener("kodex:open-settings", handleOpenSettings);
  }, []);

  const pollState = useCallback(async () => {
    try {
      const state = await sessionGetState();
      if (state.revision !== prevSnapshotRevision.current) {
        prevSnapshotRevision.current = state.revision;
        setSnapshot(state);
      }
    } catch {
      // No workspace open — stay on welcome screen
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let unlistenPatch: (() => void) | undefined;

    onUiSnapshot((nextSnapshot) => {
      if (disposed) return;
      if (nextSnapshot.revision === prevSnapshotRevision.current) return;
      prevSnapshotRevision.current = nextSnapshot.revision;
      setWorkspaceReady(true);
      if (!firstWorkspaceReadyLogged.current) {
        firstWorkspaceReadyLogged.current = true;
        void startupPerfMark(
          "workbench/ui_snapshot_event_first",
          `revision=${nextSnapshot.revision} messages=${nextSnapshot.messages.length} tools=${nextSnapshot.tools.length} timeline=${nextSnapshot.timeline.length}`,
        );
      }
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

    onUiSnapshotPatch((patch) => {
      if (disposed) return;
      if (patch.revision === prevSnapshotRevision.current) return;
      applyStreamingDeltas(patch);
      setWorkspaceReady(true);
      if (isStreamingDeltaOnlyPatch(patch)) {
        prevSnapshotRevision.current = patch.revision;
        if (!snapshotRef.current) {
          void pollState();
        }
        return;
      }
      setSnapshot((prev) => {
        if (!prev) {
          void pollState();
          return prev;
        }
        if (patch.revision <= prev.revision) return prev;
        prevSnapshotRevision.current = patch.revision;
        const next = applySnapshotPatch(prev, patch);
        return patch.session.status === "Streaming"
          ? next
          : materializeStreamingMessageBodies(next);
      });
    })
      .then((cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlistenPatch = cleanup;
      })
      .catch(() => {});

    return () => {
      disposed = true;
      unlisten?.();
      unlistenPatch?.();
    };
  }, [pollState]);

  useEffect(() => {
    if (!workspaceReady) return;
    pollState();
    const interval = setInterval(() => {
      const status = snapshotRef.current?.session.status;
      if (status === "Streaming" || status === "WaitingForTool") return;
      pollState();
    }, 2000);
    return () => clearInterval(interval);
  }, [pollState, workspaceReady]);

  const handleWorkspaceOpened = useCallback(() => {
    void startupPerfMark("workbench/handle_workspace_opened", "");
    prevSnapshotRevision.current = 0;
    setWorkspaceReady(true);
    setSnapshot(null);
    setGitHydrated(false);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    setRightPanelCollapsed(false);
  }, []);

  const handleWorkspaceChanged = useCallback((nextSnapshot: UiSnapshot) => {
    prevSnapshotRevision.current = nextSnapshot.revision;
    setWorkspaceReady(true);
    setSnapshot(nextSnapshot);
    setGitHydrated(false);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
  }, []);

  const handleRefreshGit = useCallback(async () => {
    if (gitRefreshInFlight.current) {
      gitRefreshPending.current = true;
      return;
    }
    const workspaceRoot = snapshotRef.current?.workspace.root;
    if (!workspaceRoot) return;
    const requestKey = ++gitHydrationKey.current;
    gitRefreshInFlight.current = true;
    gitRefreshPending.current = false;
    setGitRefreshing(true);
    try {
      const repo = await gitRefresh();
      setSnapshot((prev) => {
        if (!prev || prev.workspace.root !== workspaceRoot || requestKey !== gitHydrationKey.current) {
          return prev;
        }
        return { ...prev, repository: repo };
      });
      if (requestKey === gitHydrationKey.current) {
        setGitHydrated(true);
      }
    } catch {
      // ignored
    } finally {
      if (requestKey === gitHydrationKey.current) {
        gitRefreshInFlight.current = false;
        setGitRefreshing(false);
      }
      if (
        gitRefreshPending.current &&
        requestKey === gitHydrationKey.current &&
        snapshotRef.current?.workspace.root === workspaceRoot
      ) {
        gitRefreshPending.current = false;
        void handleRefreshGit();
      }
    }
  }, []);

  useEffect(() => {
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceReady || !workspaceRoot) return;

    const requestKey = ++gitHydrationKey.current;
    setGitHydrated(false);
    setGitRefreshing(true);

    let disposed = false;
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (disposed || requestKey !== gitHydrationKey.current) return;
        gitRefresh()
          .then((repo) => {
            if (disposed || requestKey !== gitHydrationKey.current) return;
            setSnapshot((prev) =>
              prev && prev.workspace.root === workspaceRoot
                ? { ...prev, repository: repo }
                : prev,
            );
            setGitHydrated(true);
          })
          .catch(() => {
            if (!disposed && requestKey === gitHydrationKey.current) {
              setGitHydrated(true);
            }
          })
          .finally(() => {
            if (!disposed && requestKey === gitHydrationKey.current) {
              setGitRefreshing(false);
            }
          });
      });
    });

    return () => {
      disposed = true;
    };
  }, [snapshot?.workspace.root, workspaceReady]);

  const currentSessionChangesSignature = sessionChangesSignature(snapshot?.session_changes);

  useEffect(() => {
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceReady || !workspaceRoot) return;

    const previous = sessionChangesRefreshRef.current;
    if (!previous || previous.workspaceRoot !== workspaceRoot) {
      sessionChangesRefreshRef.current = {
        workspaceRoot,
        signature: currentSessionChangesSignature,
      };
      return;
    }

    if (previous.signature === currentSessionChangesSignature) return;
    sessionChangesRefreshRef.current = {
      workspaceRoot,
      signature: currentSessionChangesSignature,
    };

    const timer = window.setTimeout(() => {
      if (snapshotRef.current?.workspace.root === workspaceRoot) {
        void handleRefreshGit();
      }
    }, 120);

    return () => window.clearTimeout(timer);
  }, [currentSessionChangesSignature, handleRefreshGit, snapshot?.workspace.root, workspaceReady]);

  const handlePermissionSelect = useCallback(async (requestId: string, optionId: string | null) => {
    await sessionResolvePermission(requestId, optionId);
    await pollState();
  }, [pollState]);

  const handleSessionChanged = useCallback(() => {
    prevSnapshotRevision.current = 0;
    setSnapshot(null);
    setGitHydrated(false);
    setTabs([CONVERSATION_TAB]);
    setActiveTabId("conversation");
    pollState();
  }, [pollState]);

  const handleToggleThreads = useCallback(() => {
    setSidebarCollapsed((collapsed) => !collapsed);
  }, []);

  const handleRightPanelResizeStart = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    event.preventDefault();
    const pointerId = event.pointerId;
    event.currentTarget.setPointerCapture(pointerId);
    document.body.classList.add("is-resizing-right-panel");

    const updateWidth = (clientX: number) => {
      const nextWidth = clampRightPanelWidth(window.innerWidth - clientX - 10);
      setRightPanelWidth(nextWidth);
      window.localStorage.setItem(RIGHT_PANEL_WIDTH_STORAGE_KEY, String(nextWidth));
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      updateWidth(moveEvent.clientX);
    };

    const handlePointerUp = () => {
      document.body.classList.remove("is-resizing-right-panel");
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);
  }, []);

  // ── Tab management ──

  const handleOpenDiffTab = useCallback(
    (path: string, source: "session" | "git" = "session") => {
      const tabId = `diff:${source}:${path}`;
      setTabs((prev) => {
        if (prev.some((t) => t.id === tabId)) return prev;
        const fileName = path.replace(/\\/g, "/").split("/").pop() || path;
        return [
          ...prev,
          { id: tabId, type: "diff" as const, label: fileName, filePath: path, diffSource: source },
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

  const closeTabById = useCallback(
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

  const handleCloseTab = useCallback(
    async (id: string) => {
      if (id === "conversation") return;

      const closing = tabs.find((tab) => tab.id === id);
      if (closing?.type !== "editor" || !closing.filePath) {
        closeTabById(id);
        return;
      }

      const hasUnsavedChanges = Boolean(closing.dirty) || isModelDirty(closing.filePath);
      if (!hasUnsavedChanges) {
        closeTabById(id);
        return;
      }

      setPendingCloseTab({
        id,
        label: closing.label,
        filePath: closing.filePath,
      });
    },
    [closeTabById, tabs],
  );

  const handleConfirmSaveClose = useCallback(async () => {
    if (!pendingCloseTab) return;

    const content = getModelValue(pendingCloseTab.filePath);
    const baseVersion = getModelBaseVersion(pendingCloseTab.filePath);
    if (content == null || !baseVersion) {
      window.alert("这个文件的编辑状态还没有准备好，请切回文件后再保存或关闭。");
      return;
    }

    try {
      const saved = await editorSaveFile(pendingCloseTab.filePath, content, baseVersion);
      updateModelBase(pendingCloseTab.filePath, saved.content);
      updateModelBaseVersion(pendingCloseTab.filePath, saved.version);
      disposeModel(pendingCloseTab.filePath);
      closeTabById(pendingCloseTab.id);
      setPendingCloseTab(null);
      await handleRefreshGit();
      await pollState();
    } catch (error) {
      window.alert(`保存失败，文件未关闭：${String(error)}`);
    }
  }, [closeTabById, handleRefreshGit, pendingCloseTab, pollState]);

  const handleConfirmDiscardClose = useCallback(() => {
    if (!pendingCloseTab) return;
    disposeModel(pendingCloseTab.filePath);
    closeTabById(pendingCloseTab.id);
    setPendingCloseTab(null);
  }, [closeTabById, pendingCloseTab]);

  const handleCancelClose = useCallback(() => {
    setPendingCloseTab(null);
  }, []);

  const handleEditorDirtyChange = useCallback((filePath: string, dirty: boolean) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.type === "editor" && tab.filePath === filePath ? { ...tab, dirty } : tab,
      ),
    );
  }, []);

  const handleEditorSaved = useCallback(async () => {
    await handleRefreshGit();
    await pollState();
  }, [handleRefreshGit, pollState]);

  const handleTabSelect = useCallback((id: string) => {
    setActiveTabId(id);
  }, []);

  // Computed before conditional returns — hooks must be unconditional
  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  const isDiffTab = activeTab.type === "diff" && activeTab.filePath != null;

  const [resolvedDiffChange, setResolvedDiffChange] = useState<SessionFileChange | null>(null);

  // Snapshot changes intentionally carry only stats; fetch full old/new text on demand.
  useEffect(() => {
    const source = activeTab.diffSource ?? "session";
    const filePath = activeTab.filePath;
    if (!isDiffTab || !filePath) {
      setResolvedDiffChange(null);
      return;
    }

    let cancelled = false;
    setResolvedDiffChange(null);
    const loadDiff =
      source === "git"
        ? reviewGetGitDiffContent(filePath)
        : sessionGetFileDiff(filePath).catch(() => reviewGetGitDiffContent(filePath));
    loadDiff
      .then((change) => {
        if (!cancelled) setResolvedDiffChange(change);
      })
      .catch(() => {
        if (!cancelled) setResolvedDiffChange(null);
      });

    return () => {
      cancelled = true;
    };
  }, [isDiffTab, activeTab.filePath, activeTab.diffSource, snapshot?.revision]);

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
  const displayTabs = tabs.map((t) => (t.type === "conversation" ? { ...t, label: "Chat" } : t));

  return (
    <div className="workbench">
      <GlobalChrome
        workspace={snapshot.workspace}
        sidebarCollapsed={sidebarCollapsed}
        refreshing={gitRefreshing}
        rightPanelCollapsed={rightPanelCollapsed}
        onToggleSidebar={handleToggleThreads}
        onRefreshGit={handleRefreshGit}
        onToggleRightPanel={() => setRightPanelCollapsed((collapsed) => !collapsed)}
        onFileOpen={handleSearchResultOpen}
      />

      <div className="workbench-content">
        <ThreadSidebarShell collapsed={sidebarCollapsed}>
          <SessionList
            activeSessionId={snapshot.session.id}
            activeWorkspaceRoot={snapshot.workspace.root}
            currentSessionStatus={snapshot.session.status}
            onOpenSettings={() => setSettingsOpen(true)}
            onSessionChanged={handleSessionChanged}
            onWorkspaceChanged={handleWorkspaceChanged}
          />
        </ThreadSidebarShell>

        <div className="workbench-main-shell">

        <div
          className="workbench-body"
          style={{ "--right-panel-width": `${rightPanelWidth}px` } as CSSProperties}
        >
          <main className="center-panel">
            <ThreadHeader
              session={snapshot.session}
              workspace={snapshot.workspace}
              activeTabLabel={agentLabel}
              changeCount={snapshot.session_changes?.length ?? 0}
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

            <div className="conversation-container">
              {activeTab.type === "conversation" ? (
                <>
                  <ConversationTimeline
                    snapshot={snapshot}
                    onPermissionSelect={handlePermissionSelect}
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
                    />
                  )}
                </section>
              )}
              <ChangesBar
                changes={snapshot.session_changes ?? []}
                onFileSelect={(path) => handleOpenDiffTab(path, "session")}
              />
              <Composer
                snapshot={snapshot}
                onStateChange={pollState}
              />
            </div>
          </main>

          {!rightPanelCollapsed && (
            <div className="panel-resizer">
              <button
                type="button"
                className="panel-resizer-hit"
                aria-label="调整右侧面板宽度"
                onPointerDown={handleRightPanelResizeStart}
              />
            </div>
          )}

          <aside className={`right-panel ${rightPanelCollapsed ? "is-collapsed" : ""}`}>
            <ReviewPanel
              snapshot={snapshot}
              refreshing={gitRefreshing}
              hydrated={gitHydrated}
              onRefresh={handleRefreshGit}
              onFileSelect={(path) => handleOpenDiffTab(path, "git")}
              onFileOpen={handleOpenEditorTab}
            />
          </aside>
        </div>
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
