import { useCallback, useEffect, useRef, useState } from "react";
import type { UiSnapshot, UiSnapshotPatch } from "../../types";
import { startupPerfMark, sessionGetState } from "../../lib/tauri";
import { onUiSnapshot, onUiSnapshotPatch } from "../../lib/events";
import {
  appendStreamingMessageDelta,
  getStreamingMessageBody,
} from "../conversation/streaming-message-store";

export function applySnapshotPatch(snapshot: UiSnapshot, patch: UiSnapshotPatch): UiSnapshot {
  const messages =
    patch.messages.length === 0
      ? snapshot.messages
      : mergeMessagesById(snapshot.messages, patch.messages);
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
  };
}

function mergeMessagesById(
  current: UiSnapshot["messages"],
  updates: UiSnapshot["messages"],
): UiSnapshot["messages"] {
  if (updates.length === 0) return current;
  const next = current.slice();
  const appended: UiSnapshot["messages"] = [];

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
    patch.tools.length === 0 &&
    patch.repository == null
  );
}

export function materializeStreamingMessageBodies(snapshot: UiSnapshot): UiSnapshot {
  let changed = false;
  const messages = snapshot.messages.map((message) => {
    const streamingBody = getStreamingMessageBody(message.id);
    if (
      streamingBody == null ||
      streamingBody === message.body ||
      streamingBody.length <= message.body.length ||
      !streamingBody.startsWith(message.body)
    ) {
      return message;
    }
    changed = true;
    return { ...message, body: streamingBody };
  });
  return changed ? { ...snapshot, messages } : snapshot;
}

export function useWorkbenchSnapshot() {
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(null);
  const [workspaceReady, setWorkspaceReady] = useState(false);
  // Track BOTH session id and revision. Revision is per-session (starts at 1,
  // bumps by 1), so two sessions can share the same revision value. Without
  // the session-id guard a stale event from the previous session (same
  // revision number) can block the new session's snapshot from being
  // accepted after a switch.
  const prevSnapshotRevision = useRef<number>(0);
  const prevSnapshotSessionId = useRef<string>("");
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
        void startupPerfMark(
          "workbench/first_snapshot_painted",
          `performance_now=${performance.now().toFixed(1)}`,
        );
      });
    }
  }, [snapshot]);

  const pollState = useCallback(async () => {
    try {
      const state = await sessionGetState();
      if (
        state.session.id !== prevSnapshotSessionId.current ||
        state.revision !== prevSnapshotRevision.current
      ) {
        prevSnapshotSessionId.current = state.session.id;
        prevSnapshotRevision.current = state.revision;
        setSnapshot(materializeStreamingMessageBodies(state));
      }
    } catch {
      // No workspace open; the welcome screen remains the source of truth.
    }
  }, []);

  const acceptSnapshot = useCallback((nextSnapshot: UiSnapshot) => {
    prevSnapshotSessionId.current = nextSnapshot.session.id;
    prevSnapshotRevision.current = nextSnapshot.revision;
    setWorkspaceReady(true);
    setSnapshot(materializeStreamingMessageBodies(nextSnapshot));
  }, []);

  const clearSnapshot = useCallback(() => {
    prevSnapshotSessionId.current = "";
    prevSnapshotRevision.current = 0;
    setSnapshot(null);
  }, []);

  const clearWorkspace = useCallback(() => {
    prevSnapshotSessionId.current = "";
    prevSnapshotRevision.current = 0;
    setWorkspaceReady(false);
    setSnapshot(null);
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let unlistenPatch: (() => void) | undefined;

    onUiSnapshot((nextSnapshot) => {
      if (disposed) return;
      if (
        nextSnapshot.session.id === prevSnapshotSessionId.current &&
        nextSnapshot.revision === prevSnapshotRevision.current
      )
        return;
      prevSnapshotSessionId.current = nextSnapshot.session.id;
      prevSnapshotRevision.current = nextSnapshot.revision;
      setWorkspaceReady(true);
      if (!firstWorkspaceReadyLogged.current) {
        firstWorkspaceReadyLogged.current = true;
        void startupPerfMark(
          "workbench/ui_snapshot_event_first",
          `revision=${nextSnapshot.revision} messages=${nextSnapshot.messages.length} tools=${nextSnapshot.tools.length} timeline=${nextSnapshot.timeline.length}`,
        );
      }
      setSnapshot(materializeStreamingMessageBodies(nextSnapshot));
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
      if (
        patch.session.id === prevSnapshotSessionId.current &&
        patch.revision === prevSnapshotRevision.current
      )
        return;
      applyStreamingDeltas(patch);
      setWorkspaceReady(true);
      if (isStreamingDeltaOnlyPatch(patch)) {
        prevSnapshotSessionId.current = patch.session.id;
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
        // Reject stale patches that belong to a different session than the
        // one currently rendered (e.g. a patch emitted by the bridge before a
        // session switch that arrives after the switch).
        if (patch.session.id !== prev.session.id || patch.revision <= prev.revision) {
          void pollState();
          return prev;
        }
        prevSnapshotSessionId.current = patch.session.id;
        prevSnapshotRevision.current = patch.revision;
        const next = applySnapshotPatch(prev, patch);
        return materializeStreamingMessageBodies(next);
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
    if (!workspaceReady || snapshot) return;
    pollState();
  }, [pollState, snapshot, workspaceReady]);

  return {
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady,
    pollState,
    acceptSnapshot,
    clearSnapshot,
    clearWorkspace,
  };
}
