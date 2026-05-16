import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { UiSnapshot, UiSnapshotPatch, SessionSummary, ChatMessage, ToolInvocation, RepositorySnapshot, TerminalOutputEvent, TerminalStatusEvent, TerminalExitEvent } from "../types";

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

export function onTerminalOutput(callback: (output: TerminalOutputEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalOutputEvent>("terminal:output", (event) => callback(event.payload));
}

export function onTerminalStatus(callback: (status: TerminalStatusEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalStatusEvent>("terminal:status", (event) => callback(event.payload));
}

export function onTerminalExit(callback: (exit: TerminalExitEvent) => void): Promise<UnlistenFn> {
  return listen<TerminalExitEvent>("terminal:exit", (event) => callback(event.payload));
}
