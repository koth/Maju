import { useState, useEffect, useCallback, useMemo, useRef } from "react";

import type { UiSnapshot, AppTheme, ToolInvocation, PermissionInputResponse, WorkspaceDescriptor, AgentPlanEntry } from "../../types";
import {
  startupPerfMark,
  sessionCancel,
  sessionResolvePermission,
  sessionRetryUserMessage,
  sessionStopTool,
  sessionUnarchive,
  settingsGetAgentSnapshot,
} from "../../lib/tauri";
import { ConversationTimeline, type TimelineTurnChangeSet } from "../conversation/ConversationTimeline";
import { Composer, type ComposerReferenceRequest } from "../composer/Composer";
import {
  AgentPlanPanel,
  AgentPlanEnvironment,
  PermissionRequestPanel,
  type PendingPermissionRequest,
  type AgentPlanEnvironmentInfo,
  findPlanAcceptOption,
  findPlanReplanOption,
  findPlanTerminateOption,
} from "../composer/AgentPlanPanel";
import { ReviewPanel } from "../review/ReviewPanel";
import type { ReviewPanelActiveTab, ReviewPanelOpenTab, ReviewPreferredChangeSet } from "../review/ReviewPanel";
import { DiffTab } from "../editor/DiffTab";
import { EditorView } from "../editor/EditorView";
import { WelcomeLauncher } from "./WelcomeLauncher";
import { SessionList, type ArchivedSessionNotice } from "../session/SessionList";
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
import { useAgentPlanOverlap, type AgentPlanOverlapTier } from "./useAgentPlanOverlap";
import { useSessionAgentPlan } from "./useSessionAgentPlan";
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
type CenterTabType = "conversation" | "changes" | "diff" | "editor";

function reviewOpenTabKey(tab: ReviewPanelOpenTab) {
  return tab.kind === "diff" ? `diff:${tab.changeSetId}:${tab.path}` : `file:${tab.path}`;
}

let startupUpdateCheckPromise: Promise<AppUpdateInfo | null> | null = null;

type SessionArchiveToast = ArchivedSessionNotice & {
  token: number;
  restoring: boolean;
  error: string | null;
};

function ContextDockToggleIcon() {
  return (
    <svg aria-hidden="true" focusable="false" viewBox="0 0 24 24">
      <circle cx="7" cy="8" r="1.7" />
      <circle cx="7" cy="16" r="1.7" />
      <line x1="12" y1="8" x2="18" y2="8" />
      <line x1="12" y1="16" x2="18" y2="16" />
    </svg>
  );
}

function buildAgentPlanEnvironmentInfo(
  snapshot: UiSnapshot,
  gitHydrated: boolean,
): AgentPlanEnvironmentInfo {
  const changedFiles = snapshot.repository.changed_files;
  const addedLines = changedFiles.reduce((sum, file) => sum + file.stats.added, 0);
  const removedLines = changedFiles.reduce((sum, file) => sum + file.stats.removed, 0);
  const branchLabel =
    snapshot.repository.branch.trim() ||
    (snapshot.repository.head ? snapshot.repository.head.slice(0, 7) : "") ||
    "无分支";
  const changeCount = changedFiles.length;

  return {
    changeCount,
    addedLines,
    removedLines,
    locationLabel: snapshot.workspace.location?.kind === "remote_linux" ? "远程" : "本地",
    branchLabel,
    actionLabel: !gitHydrated
      ? "读取 Git 状态"
      : changeCount > 0
        ? "提交或推送"
        : "工作区干净",
    githubLabel: "GitHub CLI 不可用",
    usage: snapshot.usage,
    streaming:
      snapshot.session.status === "Streaming" ||
      snapshot.session.status === "WaitingForTool",
  };
}

export function isActiveTurnStatus(status: UiSnapshot["session"]["status"]) {
  return status === "Streaming" || status === "WaitingForTool";
}

export function agentPlanProgressSignature(entries: AgentPlanEntry[]) {
  return entries
    .map((entry) =>
      [
        entry.id ?? "",
        entry.content,
        entry.priority,
        entry.status,
      ].join("\u001f"),
    )
    .join("\u001e");
}

function usageTokenTotal(tokens: NonNullable<UiSnapshot["usage"]>["current_turn"] | undefined) {
  if (!tokens) return 0;
  // cache_read is a subset of input; adding it would double-count. Keep
  // cache_read/cache_write as display-only breakdown, not in the total.
  return tokens.total_tokens ?? (
    (tokens.input_tokens ?? 0) +
    (tokens.output_tokens ?? 0) +
    (tokens.reasoning_tokens ?? 0)
  );
}

function liveTurnChangeSetSignature(changeSet: TimelineTurnChangeSet | null) {
  if (!changeSet || changeSet.files.length === 0) return "";
  return [
    changeSet.changeSetId,
    changeSet.updatedAt,
    ...changeSet.files.map((file) =>
      [
        file.path,
        file.change_type,
        file.added_lines,
        file.removed_lines,
        file.quality,
        file.updated_at,
      ].join("\u001f"),
    ),
  ].join("\u001e");
}

function persistedTurnChangesSignature(snapshot: UiSnapshot) {
  return snapshot.turn_changes
    .flatMap((turn) => turn.changes.map((change) => [
      turn.message_id,
      change.path,
      change.change_type,
      change.added_lines,
      change.removed_lines,
      change.timestamp,
    ].join("\u001f")))
    .join("\u001e");
}

export function agentPlanDockProgressSignature(
  snapshot: UiSnapshot,
  liveTurnChanges?: TimelineTurnChangeSet | null,
) {
  const parts: string[] = [];
  const planSignature = agentPlanProgressSignature(snapshot.agent_plan);
  if (planSignature) {
    parts.push(`plan:${planSignature}`);
  }

  const turnChangeSignature =
    liveTurnChanges === undefined
      ? persistedTurnChangesSignature(snapshot)
      : liveTurnChangeSetSignature(liveTurnChanges);
  if (turnChangeSignature) {
    parts.push(`changes:${turnChangeSignature}`);
  }

  const usage = snapshot.usage;
  const usageContext = usage?.context;
  const currentTurnTokens = usageTokenTotal(usage?.current_turn);
  if (
    currentTurnTokens > 0 ||
    usageContext?.used_tokens != null ||
    usageContext?.window_tokens != null
  ) {
    parts.push([
      "usage",
      usageContext?.used_tokens ?? "",
      usageContext?.window_tokens ?? "",
      usageContext?.updated_at ?? "",
      currentTurnTokens,
    ].join("\u001f"));
  }

  return parts.join("\u001e");
}

export function shouldAutoOpenAgentPlanDock({
  entryCount,
  hasProgress,
  sessionStatus,
  activeTabType,
  reviewPanelExpanded,
  currentSignature,
  lastAutoOpenedSignature,
}: {
  entryCount: number;
  hasProgress?: boolean;
  sessionStatus: UiSnapshot["session"]["status"];
  activeTabType: CenterTabType;
  reviewPanelExpanded: boolean;
  currentSignature: string;
  lastAutoOpenedSignature: string | null;
}) {
  return (
    (hasProgress ?? entryCount > 0) &&
    currentSignature.length > 0 &&
    currentSignature !== lastAutoOpenedSignature &&
    isActiveTurnStatus(sessionStatus) &&
    activeTabType === "conversation" &&
    !reviewPanelExpanded
  );
}

export function Workbench() {
  const {
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady,
    pollState,
    acceptSnapshot,
    clearSnapshot,
    clearWorkspace,
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
    clearChangeSets,
  } = useTimelineChangeSets({
    snapshot,
    snapshotRef,
    workspaceReady,
    onGitRefresh: handleRefreshGit,
  });
  const agentPlanEntries = useSessionAgentPlan(snapshot);
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
  const searchOpenSeqRef = useRef(0);
  const centerPanelRef = useRef<HTMLElement>(null);
  const contextDockResizeCheckRef = useRef(false);
  const lastAutoOpenedAgentPlanSignatureRef = useRef<string | null>(null);
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
    rightPanelResizing,
    rightPanelWidth,
    rightPanelStyle,
    clampStoredRightPanelWidth,
    handleRightPanelResizeStart,
  } = useRightPanelState();
  const [reviewPanelExpanded, setReviewPanelExpanded] = useState(false);
  const [contextDockCollapsed, setContextDockCollapsed] = useState(true);
  const [contextDockResizeTier, setContextDockResizeTier] =
    useState<AgentPlanOverlapTier>("none");
  const [expandedReviewSideTreeVisible, setExpandedReviewSideTreeVisible] = useState(false);
  const [reviewPanelActiveTab, setReviewPanelActiveTab] = useState<ReviewPanelActiveTab>(
    INITIAL_REVIEW_PANEL_ACTIVE_TAB,
  );
  const [reviewPanelOpenTabs, setReviewPanelOpenTabs] = useState<ReviewPanelOpenTab[]>([]);
  const [reviewPreferredChangeSet, setReviewPreferredChangeSet] =
    useState<ReviewPreferredChangeSet | null>(null);
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
  const [remoteWorkspaceHydration, setRemoteWorkspaceHydration] = useState<{
    workspaceRoot: string;
    startedAt: number;
  } | null>(null);
  const [settingsStartupNotice, setSettingsStartupNotice] = useState<SettingsStartupNotice | null>(null);
  const [settingsInitialPane, setSettingsInitialPane] = useState<SettingsPane | null>(null);
  const [settingsInitialAgentTab, setSettingsInitialAgentTab] = useState<AgentSettingsTab | null>(null);
  const [settingsRemoteContext, setSettingsRemoteContext] = useState<RemoteSettingsContext | null>(null);
  const [appTheme, setAppTheme] = useState<AppTheme>(DEFAULT_APP_THEME);
  const [startupUpdateInfo, setStartupUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [startupUpdateDismissed, setStartupUpdateDismissed] = useState(false);
  const [sessionListRefreshToken, setSessionListRefreshToken] = useState(0);
  const [sessionArchiveToast, setSessionArchiveToast] = useState<SessionArchiveToast | null>(null);
  const [cancellingTurn, setCancellingTurn] = useState(false);
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
    setReviewPreferredChangeSet(null);
    setExpandedReviewSideTreeVisible(false);
  }, []);

  const beginWorkspaceHydration = useCallback((nextSnapshot: UiSnapshot | null) => {
    if (nextSnapshot?.workspace.location?.kind !== "remote_linux") {
      setRemoteWorkspaceHydration(null);
      return;
    }
    setRemoteWorkspaceHydration({
      workspaceRoot: nextSnapshot.workspace.root,
      startedAt: performance.now(),
    });
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

  useEffect(() => {
    if (!sessionArchiveToast || sessionArchiveToast.restoring || sessionArchiveToast.error) return;
    const timeout = window.setTimeout(() => {
      setSessionArchiveToast((current) =>
        current?.token === sessionArchiveToast.token ? null : current,
      );
    }, 7000);
    return () => window.clearTimeout(timeout);
  }, [
    sessionArchiveToast?.error,
    sessionArchiveToast?.restoring,
    sessionArchiveToast?.token,
  ]);

  const handleWorkspaceOpened = useCallback((nextSnapshot: UiSnapshot) => {
    void startupPerfMark("workbench/handle_workspace_opened", "");
    acceptSnapshot(nextSnapshot);
    beginWorkspaceHydration(nextSnapshot);
    setRemoteOpenVisible(false);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
  }, [acceptSnapshot, beginWorkspaceHydration, clearChangeSets, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handleWorkspaceChanged = useCallback((nextSnapshot: UiSnapshot) => {
    acceptSnapshot(nextSnapshot);
    beginWorkspaceHydration(nextSnapshot);
    setRemoteOpenVisible(false);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setSidebarCollapsed(false);
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
  }, [acceptSnapshot, beginWorkspaceHydration, clearChangeSets, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handleWorkspaceArchived = useCallback((nextSnapshot: UiSnapshot | null) => {
    setRemoteOpenVisible(false);
    clearChangeSets();
    setComposerReferenceRequests([]);
    resetGitHydration();
    resetTabs();
    resetReviewPanelTabs();
    setReviewPanelExpanded(false);
    if (nextSnapshot) {
      acceptSnapshot(nextSnapshot);
      beginWorkspaceHydration(nextSnapshot);
    } else {
      beginWorkspaceHydration(null);
      clearWorkspace();
    }
  }, [acceptSnapshot, beginWorkspaceHydration, clearChangeSets, clearWorkspace, resetGitHydration, resetReviewPanelTabs, resetTabs]);

  const handlePermissionSelect = useCallback(async (
    requestId: string,
    optionId: string | null,
    guidance?: string | null,
    inputResponse?: PermissionInputResponse | null,
  ) => {
    setResolvingPermissionIds((current) => {
      if (current.has(requestId)) return current;
      const next = new Set(current);
      next.add(requestId);
      return next;
    });
    try {
      await sessionResolvePermission(requestId, optionId, guidance, inputResponse);
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

  const handleRetryUserMessage = useCallback(async (messageId: string, text: string) => {
    await sessionRetryUserMessage(messageId, text);
    await pollState();
  }, [pollState]);

  const handleCancelTurn = useCallback(async () => {
    if (cancellingTurn) return;
    setCancellingTurn(true);
    try {
      await sessionCancel();
      await pollState();
    } finally {
      setCancellingTurn(false);
    }
  }, [cancellingTurn, pollState]);

  const handleStopTool = useCallback(async (toolCallId: string) => {
    await sessionStopTool(toolCallId);
    await pollState();
  }, [pollState]);

  useEffect(() => {
    if (!remoteWorkspaceHydration) return;
    if (!snapshot || snapshot.workspace.root !== remoteWorkspaceHydration.workspaceRoot) {
      setRemoteWorkspaceHydration(null);
      return;
    }

    const elapsed = performance.now() - remoteWorkspaceHydration.startedAt;
    const minimumVisibleMs = 650;
    const maximumVisibleMs = 2600;
    const ready = gitHydrated || snapshot.workspace_connected === false;
    if ((ready && elapsed >= minimumVisibleMs) || elapsed >= maximumVisibleMs) {
      setRemoteWorkspaceHydration(null);
      return;
    }

    const timeoutMs = ready
      ? Math.max(0, minimumVisibleMs - elapsed)
      : Math.max(0, maximumVisibleMs - elapsed);
    const timeout = window.setTimeout(() => {
      setRemoteWorkspaceHydration((current) =>
        current?.workspaceRoot === remoteWorkspaceHydration.workspaceRoot ? null : current,
      );
    }, timeoutMs);

    return () => window.clearTimeout(timeout);
  }, [
    gitHydrated,
    remoteWorkspaceHydration,
    snapshot?.workspace.root,
    snapshot?.workspace_connected,
  ]);

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

  const handleSessionArchived = useCallback((session: ArchivedSessionNotice) => {
    setSessionArchiveToast({
      ...session,
      token: Date.now(),
      restoring: false,
      error: null,
    });
  }, []);

  const handleUndoSessionArchive = useCallback(async () => {
    if (!sessionArchiveToast || sessionArchiveToast.restoring) return;
    const token = sessionArchiveToast.token;
    setSessionArchiveToast((current) =>
      current?.token === token ? { ...current, restoring: true, error: null } : current,
    );
    try {
      await sessionUnarchive(sessionArchiveToast.id, sessionArchiveToast.workspaceRoot);
      setSessionArchiveToast((current) => (current?.token === token ? null : current));
      setSessionListRefreshToken((value) => value + 1);
    } catch (error) {
      setSessionArchiveToast((current) =>
        current?.token === token
          ? { ...current, restoring: false, error: `恢复失败：${String(error)}` }
          : current,
      );
    }
  }, [sessionArchiveToast]);

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
    const token = reviewFocusSeqRef.current;
    setReviewFocusRequest({ changeSetId, token });
    setReviewPreferredChangeSet({
      id: changeSetId,
      token,
      consumedSignature: null,
    });
    setReviewPanelActiveTab(INITIAL_REVIEW_PANEL_ACTIVE_TAB);
  }, [setRightPanelCollapsed]);

  const handleSearchFileOpen = useCallback((filePath: string, lineNumber?: number, searchQuery?: string) => {
    searchOpenSeqRef.current += 1;
    const tab: ReviewPanelOpenTab = {
      kind: "file",
      path: filePath,
      lineNumber,
      searchQuery,
      navToken: searchOpenSeqRef.current,
    };
    setRightPanelCollapsed(false);
    setReviewPanelExpanded(false);
    setReviewPanelOpenTabs((current) => {
      const tabId = `file:${filePath}`;
      if (current.some((openTab) => reviewOpenTabKey(openTab) === tabId)) {
        return current.map((openTab) => (reviewOpenTabKey(openTab) === tabId ? tab : openTab));
      }
      return [...current, tab];
    });
    setReviewPanelActiveTab(tab);
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
    const token = reviewFocusSeqRef.current;
    setReviewFocusRequest({
      changeSetId: autoReviewTarget.changeSetId,
      token,
    });
    setReviewPreferredChangeSet({
      id: autoReviewTarget.changeSetId,
      token,
      consumedSignature: null,
    });
    setReviewPanelActiveTab(INITIAL_REVIEW_PANEL_ACTIVE_TAB);
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
    context?: {
      fileTreeVisible: boolean;
      onToggleFileTree?: () => void;
      onUserInteraction?: () => void;
      lineNumber?: number;
      searchQuery?: string;
      navToken?: number;
    },
  ) => (
    <EditorView
      path={path}
      lineNumber={context?.lineNumber}
      searchQuery={context?.searchQuery}
      navToken={context?.navToken}
      appTheme={appTheme}
      toolbarMode="breadcrumbs"
      workspaceName={snapshot?.workspace.name}
      fileTreeVisible={context?.fileTreeVisible ?? false}
      onToggleFileTree={context?.onToggleFileTree}
      onDirtyChange={handleEditorDirtyChange}
      onSaved={handleEditorSaved}
      onUserInteraction={(p) => {
        handleEditorUserInteraction(p);
        context?.onUserInteraction?.();
      }}
      onAddComposerReference={enqueueComposerReference}
    />
  ), [appTheme, handleEditorDirtyChange, handleEditorSaved, handleEditorUserInteraction, enqueueComposerReference, snapshot?.workspace.name]);

  const allPendingPermissionRequests = useMemo(
    () => (snapshot ? findPendingPermissionRequests(snapshot.tools) : []),
    [snapshot?.tools],
  );
  const pendingPermissionRequests = useMemo(
    () =>
      allPendingPermissionRequests.filter(
        (request) => !resolvingPermissionIds.has(request.requestId),
      ),
    [allPendingPermissionRequests, resolvingPermissionIds],
  );
  const hiddenPermissionRequestIds = useMemo(
    () => {
      if (allPendingPermissionRequests.length === 0 && resolvingPermissionIds.size === 0) {
        return EMPTY_HIDDEN_PERMISSION_REQUEST_IDS;
      }
      const ids = new Set(allPendingPermissionRequests.map((request) => request.requestId));
      for (const requestId of resolvingPermissionIds) {
        ids.add(requestId);
      }
      return ids;
    },
    [allPendingPermissionRequests, resolvingPermissionIds],
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
  const liveAgentPlanDockSignature = snapshot
    ? agentPlanDockProgressSignature(snapshot, liveTurnChangeSet)
    : "";

  useEffect(() => {
    if (!snapshot) {
      lastAutoOpenedAgentPlanSignatureRef.current = null;
      return;
    }

    if (!isActiveTurnStatus(snapshot.session.status)) {
      lastAutoOpenedAgentPlanSignatureRef.current = null;
      return;
    }

    const scopedSignature = liveAgentPlanDockSignature
      ? `${snapshot.session.id}\u001e${liveAgentPlanDockSignature}`
      : "";

    if (!shouldAutoOpenAgentPlanDock({
      entryCount: snapshot.agent_plan.length,
      hasProgress: liveAgentPlanDockSignature.length > 0,
      sessionStatus: snapshot.session.status,
      activeTabType: activeTab.type,
      reviewPanelExpanded,
      currentSignature: scopedSignature,
      lastAutoOpenedSignature: lastAutoOpenedAgentPlanSignatureRef.current,
    })) {
      return;
    }

    lastAutoOpenedAgentPlanSignatureRef.current = scopedSignature;
    setContextDockResizeTier("none");
    setContextDockCollapsed(false);
  }, [
    activeTab.type,
    liveAgentPlanDockSignature,
    liveTurnChangeSet,
    reviewPanelExpanded,
    snapshot?.agent_plan.length,
    snapshot?.session.id,
    snapshot?.session.status,
  ]);

  const contextDockBaseOpen = !!snapshot && !reviewPanelExpanded && !contextDockCollapsed;
  const contextDockOverlapCheck = contextDockBaseOpen && activeTab.type === "conversation";
  const contextDockResizeCheck = contextDockOverlapCheck && rightPanelResizing;
  const agentPlanOverlap = useAgentPlanOverlap(
    centerPanelRef,
    contextDockOverlapCheck,
  );
  const effectiveContextDockTier = contextDockOverlapCheck ? agentPlanOverlap : contextDockResizeTier;
  const contextDockAutoHidden = effectiveContextDockTier === "hidden";
  const contextDockVisible = contextDockBaseOpen && !contextDockAutoHidden;
  const contextDockShouldShift =
    contextDockVisible && effectiveContextDockTier === "shift";

  useEffect(() => {
    if (contextDockResizeCheck) {
      setContextDockResizeTier(agentPlanOverlap);
    }
  }, [agentPlanOverlap, contextDockResizeCheck]);

  useEffect(() => {
    const wasResizeChecking = contextDockResizeCheckRef.current;
    contextDockResizeCheckRef.current = contextDockResizeCheck;

    if (!wasResizeChecking || contextDockResizeCheck || contextDockResizeTier !== "hidden") {
      return;
    }

    setContextDockCollapsed(true);
  }, [contextDockResizeCheck, contextDockResizeTier]);

  useEffect(() => {
    if (contextDockCollapsed) {
      contextDockResizeCheckRef.current = false;
    }
  }, [contextDockCollapsed]);

  const handleContextDockToggle = useCallback(() => {
    setContextDockResizeTier("none");
    setContextDockCollapsed((collapsed) => !collapsed);
  }, []);


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
  const remoteHydrating =
    isRemoteWorkspace && remoteWorkspaceHydration?.workspaceRoot === snapshot.workspace.root;
  const terminalDockAvailable = isTerminalDockAvailableForWorkspace(snapshot.workspace);
  const terminalDockActive = terminalDockAvailable && terminalDockVisible;
  const agentPlanEnvironment = buildAgentPlanEnvironmentInfo(snapshot, gitHydrated);
  const agentPlanDockSlot =
    contextDockVisible ? (
      <aside
        className={`agent-plan-dock ${displayTabs.length > 1 ? "has-center-tabs" : ""} ${
          agentPlanEntries.length > 0 ? "has-progress" : ""
        }`}
        aria-label="环境信息"
      >
        <AgentPlanEnvironment environment={agentPlanEnvironment} />
        <AgentPlanPanel entries={agentPlanEntries} />
      </aside>
    ) : null;
  // Steers queued while a turn was running but not yet moved into the
  // timeline by the backend. Rendered as a small pending area above the
  // composer so the user sees their queued 追加指令 without it cutting the
  // currently-streaming assistant output.
  const pendingSteers = snapshot?.pending_steers ?? [];
  const pendingSteersSlot =
    pendingSteers.length > 0 ? (
      <div className="composer-pending-steers" role="status" aria-label="待处理的追加指令">
        {pendingSteers.map((steer) => (
          <div key={steer.message_id} className="composer-pending-steer">
            <span className="composer-pending-steer-badge">追加指令</span>
            <span className="composer-pending-steer-body">{steer.body}</span>
          </div>
        ))}
      </div>
    ) : null;
  const composerStatusSlot =
    pendingPermissionRequests.length > 0 ? (
      <div className="composer-plan-slot">
        {pendingPermissionRequests.map((request) => (
          <PermissionRequestPanel
            key={request.requestId}
            request={request}
            entries={agentPlanEntries}
            onPermissionSelect={handlePermissionSelect}
          />
        ))}
      </div>
    ) : null;
  const workbenchBodyClassName = [
    "workbench-body",
    terminalDockActive ? "has-terminal-dock" : "",
    remoteHydrating ? "is-remote-hydrating" : "",
    reviewPanelExpanded ? "is-review-expanded" : "",
    reviewPanelExpanded && expandedReviewSideTreeVisible ? "has-expanded-review-side-tree" : "",
  ].filter(Boolean).join(" ");
  const reviewPanel = remoteHydrating ? (
    <RemoteWorkspaceHydrationPanel />
  ) : (
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
      preferredChangeSet={reviewPreferredChangeSet}
      onPreferredChangeSetChange={setReviewPreferredChangeSet}
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
        onFileOpen={handleSearchFileOpen}
      />

      <div className="workbench-content" style={leftSidebarStyle}>
        <ThreadSidebarShell collapsed={sidebarCollapsed}>
          {remoteHydrating ? (
            <RemoteWorkspaceHydrationSidebar workspace={snapshot.workspace} />
          ) : (
            <SessionList
              activeSessionId={snapshot.session.id}
              activeSessionTitle={snapshot.session.title}
              activeWorkspaceRoot={snapshot.workspace.root}
              currentSessionStatus={snapshot.session.status}
              activeConversationVisible={activeTab.type === "conversation"}
              refreshToken={sessionListRefreshToken}
              onOpenSettings={handleOpenSettings}
              onSessionChanged={handleSessionChanged}
              onWorkspaceChanged={handleWorkspaceChanged}
              onWorkspaceArchived={handleWorkspaceArchived}
              onSessionArchived={handleSessionArchived}
            />
          )}
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
            <main
              ref={centerPanelRef}
              className={
                "center-panel" +
                (contextDockShouldShift ? " is-agent-plan-active is-agent-plan-overlap" : "")
              }
            >
            {reviewPanelExpanded && (
              <section className="expanded-review-panel-shell" aria-label="展开审查面板">
                {reviewPanel}
              </section>
            )}
            <ThreadHeader
              session={snapshot.session}
              planToggle={
                <button
                  type="button"
                  className={`thread-header-plan-toggle ${
                    contextDockVisible ? "is-active" : ""
                  }`}
                  aria-label={contextDockVisible ? "折叠环境信息" : "展开环境信息"}
                  aria-expanded={contextDockVisible}
                  title={contextDockVisible ? "折叠环境信息" : "展开环境信息"}
                  onClick={handleContextDockToggle}
                >
                  <ContextDockToggleIcon />
                </button>
              }
            />
            {sessionArchiveToast && (
              <div className="session-archive-toast" role="status" aria-live="polite">
                <div className="session-archive-toast-copy">
                  <span>已归档“{sessionArchiveToast.title}”</span>
                  {sessionArchiveToast.error && <small>{sessionArchiveToast.error}</small>}
                </div>
                <div className="session-archive-toast-actions">
                  <button
                    type="button"
                    className="session-archive-toast-btn"
                    disabled={sessionArchiveToast.restoring}
                    onClick={handleUndoSessionArchive}
                  >
                    {sessionArchiveToast.restoring ? "恢复中..." : "撤销"}
                  </button>
                  <button
                    type="button"
                    className="session-archive-toast-btn"
                    onClick={() => {
                      setSessionArchiveToast(null);
                      handleOpenSettings({ initialPane: "archive" });
                    }}
                  >
                    查看已归档
                  </button>
                  <button
                    type="button"
                    className="session-archive-toast-close"
                    aria-label="关闭归档提示"
                    onClick={() => setSessionArchiveToast(null)}
                  >
                    ×
                  </button>
                </div>
              </div>
            )}
            {agentPlanDockSlot}

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

            {remoteHydrating ? (
              <div className="conversation-container is-remote-hydrating">
                <RemoteWorkspaceHydrationMain workspace={snapshot.workspace} />
              </div>
            ) : reviewPanelExpanded ? (
              <div
                className={`expanded-review-composer-layer ${
                  expandedReviewSideTreeVisible ? "has-review-side-tree" : ""
                }`}
              >
                {pendingSteersSlot}
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
                      onRetryUserMessage={handleRetryUserMessage}
                      onCancelTurn={handleCancelTurn}
                      onStopTool={handleStopTool}
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
                {pendingSteersSlot}
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
          {shouldRenderTerminalDock(snapshot.workspace, terminalDockMounted) && (
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

function RemoteWorkspaceHydrationMain({ workspace }: { workspace: WorkspaceDescriptor }) {
  return (
    <div className="remote-hydration-main" role="status" aria-live="polite">
      <span className="remote-hydration-spinner" aria-hidden="true" />
      <div>
        <div className="remote-hydration-title">正在载入远程项目</div>
        <div className="remote-hydration-copy">{workspace.name}</div>
        <div className="remote-hydration-meta">{remoteWorkspaceLocationText(workspace)}</div>
      </div>
    </div>
  );
}

function RemoteWorkspaceHydrationSidebar({ workspace }: { workspace: WorkspaceDescriptor }) {
  return (
    <div className="remote-hydration-sidebar" role="status" aria-live="polite">
      <div className="remote-hydration-sidebar-kicker">项目</div>
      <div className="remote-hydration-sidebar-card">
        <span className="remote-hydration-spinner" aria-hidden="true" />
        <div>
          <div className="remote-hydration-sidebar-title">{workspace.name}</div>
          <div className="remote-hydration-sidebar-copy">正在同步远程状态</div>
        </div>
      </div>
    </div>
  );
}

function RemoteWorkspaceHydrationPanel() {
  return (
    <div className="remote-hydration-panel" role="status" aria-live="polite">
      <span className="remote-hydration-spinner" aria-hidden="true" />
      <span>正在准备审查面板</span>
    </div>
  );
}

function remoteWorkspaceLocationText(workspace: WorkspaceDescriptor) {
  const location = workspace.location;
  if (location?.kind !== "remote_linux") {
    return workspace.root;
  }
  const port = location.ssh_port ? `:${location.ssh_port}` : "";
  return `${location.ssh_target}${port}:${location.remote_path}`;
}

export function findPendingPlanApproval(tools: ToolInvocation[]) {
  const toolIndex = tools.findIndex(
    (tool) =>
      tool.kind === "permission" &&
      tool.status === "Running" &&
      !tool.permission_decision &&
      isPlanApprovalPermission(tool) &&
      !!findPlanAcceptOption(tool.permission_options) &&
      (!!findPlanReplanOption(tool.permission_options) ||
        !!findPlanTerminateOption(tool.permission_options)),
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
      input: tool.permission_input,
      isPlanApproval: planApproval,
    }];
  });
}

export function pendingPermissionRequestIds(tools: ToolInvocation[]) {
  return tools.filter(isPendingPermissionTool).map((tool) => tool.call_id);
}

export function isTerminalDockAvailableForWorkspace(_workspace: WorkspaceDescriptor) {
  return true;
}

export function shouldRenderTerminalDock(workspace: WorkspaceDescriptor, mounted: boolean) {
  return mounted && isTerminalDockAvailableForWorkspace(workspace);
}

function isPendingPermissionTool(tool: ToolInvocation) {
  return (
    tool.status === "Running" &&
    !tool.permission_decision &&
    (tool.permission_options.length > 0 || (tool.permission_input?.questions.length ?? 0) > 0)
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
  const detailText = normalizedPermissionDetailText(tool.detail_text, tool.permission_options);
  if (detailText && detailText !== planText) {
    return detailText;
  }

  const rawInput = normalizedPermissionDetailText(tool.raw_input, tool.permission_options);
  if (rawInput && rawInput !== planText && !looksLikeJsonPayload(rawInput)) {
    return rawInput;
  }

  return null;
}

function normalizedPermissionDetailText(
  value: string | null | undefined,
  options: ToolInvocation["permission_options"],
) {
  const trimmed = value?.trim();
  if (!trimmed) {
    return null;
  }

  const cleaned = stripInternalPermissionDecisionBlock(trimmed, options)
    .replace(/[ \t]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();

  return cleaned || null;
}

function stripInternalPermissionDecisionBlock(
  value: string,
  options: ToolInvocation["permission_options"],
) {
  const optionKeys = new Set(
    options
      .flatMap((option) => [option.id, option.label, option.kind])
      .map(normalizedPermissionDecisionToken)
      .filter(Boolean),
  );
  const knownDecisionKeys = [
    "approved",
    "approve",
    "approvedexecpolicyamendment",
    "approvedexecpolicy",
    "abort",
    "allow",
    "allowonce",
    "allowalways",
    "deny",
    "denied",
    "reject",
    "rejected",
    "rejectonce",
    "cancel",
    "cancelled",
  ];
  for (const key of knownDecisionKeys) {
    optionKeys.add(key);
  }

  const lines = value.split(/\r?\n/);
  const visibleLines: string[] = [];
  let skippingDecisionChoices = false;

  for (const line of lines) {
    const trimmed = line.trim();
    if (/^Available\s+(?:Decisions?|Options?)\s*:/i.test(trimmed)) {
      skippingDecisionChoices = true;
      continue;
    }

    if (skippingDecisionChoices) {
      if (!trimmed) {
        skippingDecisionChoices = false;
        continue;
      }

      if (looksLikePermissionDecisionChoice(trimmed, optionKeys)) {
        continue;
      }

      skippingDecisionChoices = false;
    }

    visibleLines.push(line);
  }

  return visibleLines.join("\n");
}

function looksLikePermissionDecisionChoice(line: string, optionKeys: Set<string>) {
  const key = normalizedPermissionDecisionToken(line);
  if (key && optionKeys.has(key)) {
    return true;
  }

  return (
    /^[A-Za-z][A-Za-z0-9_ -]*$/.test(line) &&
    /(approved|approve|allow|abort|cancel|deny|reject|decision|policy|amendment)/i.test(line)
  );
}

function normalizedPermissionDecisionToken(value: string | null | undefined) {
  return value?.toLowerCase().replace(/[^a-z0-9]/g, "") ?? "";
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
    const planText = structuredPlanTextFromTool(tool) ?? planTextFromDiffPreviews(tool);
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

function planTextFromDiffPreviews(tool: ToolInvocation) {
  for (const preview of tool.diff_previews) {
    if (!looksLikeCodeBuddyPlanPath(preview.path)) {
      continue;
    }
    const addedText = preview.hunks
      .flatMap((hunk) => hunk.lines)
      .filter((line) => line.kind === "Added")
      .map((line) => line.content)
      .join("\n")
      .trim();
    if (looksLikePlanBody(addedText)) {
      return addedText;
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
    return planTextCandidate(trimmed);
  }
}

function planTextCandidate(value: string | null | undefined) {
  const trimmed = value?.trim();
  if (!trimmed || isCodeBuddyMissingPlanWarning(trimmed)) {
    return null;
  }
  return trimmed;
}

function planTextFromParsedPayload(payload: unknown): string | null {
  if (typeof payload === "string") {
    return planTextCandidate(payload);
  }

  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  const record = payload as Record<string, unknown>;
  const plan = planTextCandidate(stringValue(record.plan));
  if (plan) {
    return plan;
  }

  const rawResponse = record["codebuddy.ai/rawResponse"];
  if (rawResponse && typeof rawResponse === "object" && !Array.isArray(rawResponse)) {
    return planTextCandidate(stringValue((rawResponse as Record<string, unknown>).plan));
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

  for (const key of ["content", "newString", "new_string", "newText", "new_text", "text", "markdown", "body"]) {
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
  if (!trimmed || looksLikeCodeBuddyPlanPath(trimmed) || isCodeBuddyMissingPlanWarning(trimmed)) {
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

function isCodeBuddyMissingPlanWarning(value: string) {
  return value.toLowerCase().includes("plan file was not found or empty");
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
