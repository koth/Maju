import type { UiSnapshot, UiSnapshotPatch, ChatMessage, ToolInvocation, TimelineItem } from "../types";

// Verbatim port of the desktop `applySnapshotPatch` reducer
// (apps/desktop/ui/src/features/workbench/useWorkbenchSnapshot.ts), so the
// phone renders byte-equivalent state to the local frontend for the same
// reducer output. Messages/tools merge by id; an empty patch list preserves
// the prior list; timeline splices at timeline_start.

export function applySnapshotPatch(snapshot: UiSnapshot, patch: UiSnapshotPatch): UiSnapshot {
  const messages =
    patch.messages.length === 0
  ? snapshot.messages
  : mergeMessagesById(snapshot.messages, patch.messages);
  const tools =
    patch.tools.length === 0 ? snapshot.tools : mergeById(snapshot.tools, patch.tools);
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
  repository: patch.repository ?? snapshot.repository,
  inspector_tab: patch.inspector_tab,
  inspector_sections: patch.inspector_sections,
  session_changes: patch.session_changes,
  review_changes: patch.review_changes,
  turn_changes: patch.turn_changes ?? snapshot.turn_changes ?? [],
  thinking_status: patch.thinking_status,
  // The backend always sends the full replacement list of pending steers
  // (empty once they have been moved into the timeline).
  pending_steers: patch.pending_steers ?? snapshot.pending_steers ?? [],
  // `usage` is preserved from the prior snapshot, matching the desktop
  // reducer (it is not overridden by the patch in applySnapshotPatch).
  };
}

function mergeMessagesById(
  current: ChatMessage[],
  updates: ChatMessage[],
): ChatMessage[] {
  if (updates.length === 0) return current;
  const next = current.slice();
  const appended: ChatMessage[] = [];
  for (const update of updates) {
  const index = next.findIndex((item) => item.id === update.id);
  if (index >= 0) {
  const currentMessage = next[index];
  const shouldKeepLongerCurrentBody =
  currentMessage.role === update.role &&
  currentMessage.role === "Assistant" &&
  currentMessage.body.length > update.body.length &&
  currentMessage.body.startsWith(update.body);
  const nextMessage = shouldKeepLongerCurrentBody
  ? { ...update, body: currentMessage.body }
  : update;
  if (next[index] !== nextMessage) {
  next[index] = nextMessage;
  }
  } else {
  appended.push(update);
  }
  }
  return appended.length === 0 ? next : [...next, ...appended];
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

/** A `ToolUpdated` event is a single-tool merge (replace-or-append by id). */
export function applyToolUpdated(snapshot: UiSnapshot, tool: ToolInvocation): UiSnapshot {
  return { ...snapshot, tools: mergeById(snapshot.tools, [tool]) };
}

/** `SessionStatusChanged` updates the active session's status. */
export function applySessionStatus(
  snapshot: UiSnapshot,
  status: UiSnapshot["session"]["status"],
): UiSnapshot {
  return { ...snapshot, session: { ...snapshot.session, status } };
}
// end of file
