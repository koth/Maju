import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { UiSnapshot, UiSnapshotPatch, SessionSummary, ChatMessage, ToolInvocation, RepositorySnapshot, TerminalOutputEvent, TerminalStatusEvent, TerminalExitEvent, RemoteOpenProgressEvent, ProxyRetryStatus, AutomationFiredEvent, BrowserInstallState, CapabilityStateEvent, BrowserViewTargetsEvent, BrowserViewMetaEvent, BrowserViewFrameEvent, BrowserViewStatusEvent } from "../types";

export function onUiSnapshot(callback: (snapshot: UiSnapshot) => void): Promise<UnlistenFn> {
  return listen<UiSnapshot>("ui:snapshot", (event) => callback(event.payload));
}

export function onUiSnapshotPatch(callback: (patch: UiSnapshotPatch) => void): Promise<UnlistenFn> {
  return listen<UiSnapshotPatch>("ui:snapshot_patch", (event) => callback(event.payload));
}

export function onSessionStatus(callback: (status: SessionSummary) => void): Promise<UnlistenFn> {
  return listen<SessionSummary>("session:status", (event) => callback(event.payload));
}

export function onSessionMessage(callback: (messages: ChatMessage[]) => void): Promise<UnlistenFn> {
  return listen<ChatMessage[]>("session:message", (event) => callback(event.payload));
}

export function onToolUpdated(callback: (tools: ToolInvocation[]) => void): Promise<UnlistenFn> {
  return listen<ToolInvocation[]>("tool:updated", (event) => callback(event.payload));
}

export function onGitStatusChanged(callback: (repo: RepositorySnapshot) => void): Promise<UnlistenFn> {
  return listen<RepositorySnapshot>("git:status_changed", (event) => callback(event.payload));
}

export function onCommitProgress(callback: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>("commit:progress", (event) => callback(event.payload));
}

export function onTerminalOutput(callback: (output: TerminalOutputEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalOutputEvent>("terminal:output", (event) => callback(event.payload));
}

export function onTerminalStatus(callback: (status: TerminalStatusEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalStatusEvent>("terminal:status", (event) => callback(event.payload));
}

export function onTerminalExit(callback: (exit: TerminalExitEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalExitEvent>("terminal:exit", (event) => callback(event.payload));
}

/**
 * Browser and computer-use state changes.
 *
 * Pushed rather than polled: a screenshot or navigation should appear as soon
 * as it happens, and a desktop capture can arrive while no turn is running.
 */
export function onCapabilityState(callback: (event: CapabilityStateEvent) => void): Promise<UnlistenFn> {
  return listen<CapabilityStateEvent>("capability:state", (event) => callback(event.payload));
}

/** Browser provider install progress. Pushed rather than polled: an install
 *  takes tens of seconds and a pane that looks idle is worse than one that
 *  says which step is running. */
export function onBrowserInstallProgress(
  callback: (state: BrowserInstallState) => void,
): Promise<UnlistenFn> {
  return listen<BrowserInstallState>("browser:install_progress", (event) =>
    callback(event.payload),
  );
}

/** Right-panel browser view (see docs/browser-view-subsystem.md): the live
 *  pages of the session's built-in browser. */
export function onBrowserViewTargets(
  callback: (event: BrowserViewTargetsEvent) => void,
): Promise<UnlistenFn> {
  return listen<BrowserViewTargetsEvent>("browser_view:targets", (event) =>
    callback(event.payload),
  );
}

export function onBrowserViewMeta(
  callback: (event: BrowserViewMetaEvent) => void,
): Promise<UnlistenFn> {
  return listen<BrowserViewMetaEvent>("browser_view:meta", (event) =>
    callback(event.payload),
  );
}

export function onBrowserViewFrame(
  callback: (event: BrowserViewFrameEvent) => void,
): Promise<UnlistenFn> {
  return listen<BrowserViewFrameEvent>("browser_view:frame", (event) =>
    callback(event.payload),
  );
}

export function onBrowserViewStatus(
  callback: (event: BrowserViewStatusEvent) => void,
): Promise<UnlistenFn> {
  return listen<BrowserViewStatusEvent>("browser_view:status", (event) =>
    callback(event.payload),
  );
}

export function onRemoteOpenProgress(callback: (progress: RemoteOpenProgressEvent) => void): Promise<UnlistenFn> {
  return listen<RemoteOpenProgressEvent>("remote_open:progress", (event) => callback(event.payload));
}

/** Upstream-retry status pushed by the codex_api_proxy via the snapshot
 *  bridge. `null` clears the retry animation (no retry in flight). */
export function onProxyRetry(callback: (status: ProxyRetryStatus | null) => void): Promise<UnlistenFn> {
  return listen<ProxyRetryStatus | null>("proxy:retry", (event) => callback(event.payload));
}

/** "到点提醒": an automation (定时任务) reached its trigger point and its run
 *  was dispatched — or failed to start (`error` is then set). */
export function onAutomationFired(callback: (event: AutomationFiredEvent) => void): Promise<UnlistenFn> {
  return listen<AutomationFiredEvent>("automation:fired", (event) => callback(event.payload));
}
