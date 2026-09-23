import { useCallback, useEffect, useRef, useState } from "react";
import type { UiSnapshot, UiSnapshotPatch } from "../../types";
import {
  sessionGetPatchesSince,
  startupPerfMark,
  sessionGetState,
  sessionGetRevision,
  sessionLoadHistoryBefore,
} from "../../lib/tauri";
import { onUiSnapshot, onUiSnapshotPatch } from "../../lib/events";
import {
  appendStreamingMessageDelta,
  clearStreamingMessageBodies,
  ensureStreamingMessageBody,
  flushStreamingMessageBodies,
  getStreamingMessageBody,
  replaceStreamingMessageBody,
} from "../conversation/streaming-message-store";

/** How often the workbench re-syncs its snapshot from the backend. Streaming
 *  renders depend on incremental `ui:snapshot_patch` events whose deltas are
 *  only correct when applied in-order with no gaps. A single dropped event
 *  (webview busy under heavy markdown rendering) silently desyncs the local
 *  stream store from the backend message body, truncating the final reply
 *  with no way to recover. This periodic full poll is the self-heal: when the
 *  backend revision is ahead of ours we replace the local snapshot with the
 *  complete state, so a missed delta self-corrects within a few seconds. */
const SNAPSHOT_SELF_HEAL_POLL_MS = 3000;

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
    thinking_text: patch.thinking_text ?? snapshot.thinking_text,
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

/** Apply a patch's streaming deltas to the append-only stream store. Returns
 *  true when at least one delta was skipped as misaligned — the caller must
 *  then schedule a full re-snapshot, because a desynced store can never
 *  recover by appending (that is the "final part of the reply renders
 *  truncated" failure mode).
 *
 *  Alignment rule: the backend stamps every delta with `base_len`, the UTF-16
 *  length of the base body it extends. The local store body must have exactly
 *  that length. A message whose streaming render has not mounted yet (no
 *  store entry) may be seeded from the snapshot body when it matches
 *  `base_len`; otherwise the delta is skipped. */
function applyStreamingDeltas(
  patch: UiSnapshotPatch,
  messages: UiSnapshot["messages"],
): boolean {
  let misaligned = false;
  for (const delta of patch.message_deltas ?? []) {
    if (!delta.append) continue;
    const storeBody = getStreamingMessageBody(delta.id) ?? "";
    let baseBody = storeBody;
    if (!baseBody) {
      const snapshotBody =
        messages.find((message) => message.id === delta.id)?.body ?? "";
      baseBody = snapshotBody;
    }
    if (typeof delta.base_len === "number" && baseBody.length !== delta.base_len) {
      misaligned = true;
      continue;
    }
    if (!storeBody && baseBody) {
      ensureStreamingMessageBody(delta.id, baseBody);
    }
    appendStreamingMessageDelta(delta.id, delta.append);
  }
  return misaligned;
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

export function materializeStreamingMessageBodies(
  snapshot: UiSnapshot,
  options?: { reconcileStore?: boolean },
): UiSnapshot {
  // Pending stream flushes are debounced; force them out before we decide whether
  // the snapshot body is stale relative to the live stream store.
  flushStreamingMessageBodies();
  const reconcileStore = options?.reconcileStore ?? false;
  let changed = false;
  const messages = snapshot.messages.map((message) => {
    const streamingBody = getStreamingMessageBody(message.id);
    if (
      streamingBody == null ||
      streamingBody === message.body ||
      streamingBody.length <= message.body.length
    ) {
      if (
        reconcileStore &&
        streamingBody != null &&
        streamingBody !== message.body
      ) {
        // A full snapshot is authoritative. The append-only stream store can
        // never recover from a divergence by appending: without this repair
        // every later fold fails, the snapshot body freezes at the divergence
        // point while revisions keep advancing, and the final Idle render
        // shows a truncated prefix of the reply. Re-align the store so the
        // next delta appends to the correct base.
        replaceStreamingMessageBody(message.id, message.body);
      }
      return message;
    }
    // Prefer the longer stream body whenever it is a continuation OR the
    // snapshot body is only a stale prefix-incompatible fragment. The common
    // failure mode is delta-only patches updating the stream store while
    // `snapshot.messages` stays truncated; when streaming ends the UI would
    // otherwise render the truncated snapshot body.
    const streamIsContinuation = streamingBody.startsWith(message.body);
    const snapshotLooksStalePrefix =
      message.role === "Assistant" &&
      message.body.length > 0 &&
      streamingBody.includes(message.body);
    if (!streamIsContinuation && !snapshotLooksStalePrefix) {
      if (reconcileStore) {
        replaceStreamingMessageBody(message.id, message.body);
      }
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
  // Timestamp of the most recently ACCEPTED patch / full snapshot event. The
  // 3s self-heal poll only re-fetches the full snapshot when updates have
  // genuinely stopped flowing: while streaming, the backend revision advances
  // through accepted patches, and treating that advance as "suspect desync"
  // made pollState() clone + serialize + JSON.parse the ENTIRE snapshot
  // (multi-MB once a session has real history) on the webview main thread
  // every 3 seconds — a rhythmic ~100-400ms freeze for the whole turn, the
  // "constantly laggy as soon as a conversation starts" complaint.
  const lastAcceptedUpdateAtRef = useRef(0);

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

  // Free cached streaming bodies when the session changes — the stream store
  // only ever grows otherwise, so long sessions accumulate every historical
  // message body in memory.
  const currentSessionId = snapshot?.session.id ?? null;
  useEffect(() => {
    return () => {
      clearStreamingMessageBodies();
    };
  }, [currentSessionId]);

  const pollState = useCallback(async (force = false) => {
    try {
      const state = await sessionGetState();
      if (
        force ||
        state.session.id !== prevSnapshotSessionId.current ||
        state.revision !== prevSnapshotRevision.current
      ) {
        prevSnapshotSessionId.current = state.session.id;
        prevSnapshotRevision.current = state.revision;
        lastAcceptedUpdateAtRef.current = Date.now();
        setSnapshot(materializeStreamingMessageBodies(state, { reconcileStore: true }));
      }
    } catch {
      // No workspace open; the welcome screen remains the source of truth.
    }
  }, []);

  const acceptSnapshot = useCallback((nextSnapshot: UiSnapshot) => {
    prevSnapshotSessionId.current = nextSnapshot.session.id;
    prevSnapshotRevision.current = nextSnapshot.revision;
    lastAcceptedUpdateAtRef.current = Date.now();
    setWorkspaceReady(true);
    setSnapshot(materializeStreamingMessageBodies(nextSnapshot, { reconcileStore: true }));
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
    // Debounce full-snapshot re-syncs triggered by patch reconcile paths so a
    // burst of gap/stale patches during streaming doesn't fire several full
    // `session_get_state` clones back-to-back.
    let reconcileTimer = 0;
    const scheduleFullResync = () => {
      if (reconcileTimer !== 0) return;
      reconcileTimer = window.setTimeout(() => {
        reconcileTimer = 0;
        // Forced: a reconcile is only scheduled when the local state is known
        // to be suspect (gap/stale patch, misaligned delta, missing base).
        // The revision may already match the backend even though the message
        // bodies diverged — the revision-equality guard must not skip the
        // authoritative re-fetch in that case.
        void pollState(true);
      }, 120);
    };
    // Guard against double-applying a patch's deltas when React StrictMode
    // re-invokes the setSnapshot updater for the same patch object.
    const appliedDeltaPatches = new WeakSet<UiSnapshotPatch>();

    // Incremental self-heal: replay the missed patch chain from the bridge's
    // ring buffer instead of refetching the whole snapshot. `fromRevision` is
    // the last locally accepted revision; the backend returns every emitted
    // patch continuing from there (or null when the buffer cannot cover the
    // span — then, and only then, fall back to a full snapshot). The loop
    // re-fetches until caught up so patches emitted while replaying are
    // covered too.
    const replayingRef = { active: false };
    const replayPatchesSince = async (fromRevision: number) => {
      if (replayingRef.active || disposed) return;
      replayingRef.active = true;
      try {
        let from = fromRevision;
        for (let round = 0; round < 20; round += 1) {
          let chain: UiSnapshotPatch[] | null = null;
          try {
            chain = await sessionGetPatchesSince(from);
          } catch {
            chain = null;
          }
          if (disposed) return;
          if (!chain) {
            scheduleFullResync();
            return;
          }
          if (chain.length === 0) return; // caught up
          let lastRevision = from;
          for (const missedPatch of chain) {
            let misaligned = false;
            setSnapshot((prev) => {
              if (!prev) {
                scheduleFullResync();
                return prev;
              }
              if (
                missedPatch.session.id !== prev.session.id ||
                missedPatch.revision <= prev.revision
              ) {
                // Already covered (a live patch raced ahead during replay).
                return prev;
              }
              prevSnapshotSessionId.current = missedPatch.session.id;
              prevSnapshotRevision.current = Math.max(
                prev.revision,
                missedPatch.revision,
              );
              lastAcceptedUpdateAtRef.current = Date.now();
              const missedHasDeltas =
                (missedPatch.message_deltas?.length ?? 0) > 0;
              if (
                missedHasDeltas &&
                !appliedDeltaPatches.has(missedPatch)
              ) {
                appliedDeltaPatches.add(missedPatch);
                if (applyStreamingDeltas(missedPatch, prev.messages)) {
                  misaligned = true;
                }
              }
              if (
                isStreamingDeltaOnlyPatch(missedPatch) ||
                (missedHasDeltas && missedPatch.messages.length === 0)
              ) {
                return materializeStreamingMessageBodies({
                  ...prev,
                  revision: Math.max(prev.revision, missedPatch.revision),
                  session: missedPatch.session,
                  session_config: missedPatch.session_config ?? prev.session_config,
                  thinking_status: missedPatch.thinking_status,
                  thinking_text: missedPatch.thinking_text ?? prev.thinking_text,
                  pending_steers: missedPatch.pending_steers ?? prev.pending_steers,
                });
              }
              return materializeStreamingMessageBodies(
                applySnapshotPatch(prev, missedPatch),
              );
            });
            if (misaligned) {
              scheduleFullResync();
              return;
            }
            lastRevision = missedPatch.revision;
          }
          from = lastRevision;
        }
      } finally {
        replayingRef.active = false;
      }
    };

    onUiSnapshot((nextSnapshot) => {
      if (disposed) return;
      if (
        nextSnapshot.session.id === prevSnapshotSessionId.current &&
        nextSnapshot.revision === prevSnapshotRevision.current
      )
        return;
      prevSnapshotSessionId.current = nextSnapshot.session.id;
      prevSnapshotRevision.current = nextSnapshot.revision;
      lastAcceptedUpdateAtRef.current = Date.now();
      setWorkspaceReady(true);
      if (!firstWorkspaceReadyLogged.current) {
        firstWorkspaceReadyLogged.current = true;
        void startupPerfMark(
          "workbench/ui_snapshot_event_first",
          `revision=${nextSnapshot.revision} messages=${nextSnapshot.messages.length} tools=${nextSnapshot.tools.length} timeline=${nextSnapshot.timeline.length}`,
        );
      }
      setSnapshot(materializeStreamingMessageBodies(nextSnapshot, { reconcileStore: true }));
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
      const hasDeltas = (patch.message_deltas?.length ?? 0) > 0;
      const isDuplicateRevision =
        patch.session.id === prevSnapshotSessionId.current &&
        patch.revision === prevSnapshotRevision.current;
      // Same-revision patches are normally ignored, but streaming deltas must
      // still land in the stream store + snapshot bodies.
      if (isDuplicateRevision && !hasDeltas) return;

      setWorkspaceReady(true);
      setSnapshot((prev) => {
        if (!prev) {
          scheduleFullResync();
          return prev;
        }
        // Reject stale patches that belong to a different session than the
        // one currently rendered (e.g. a patch emitted by the bridge before a
        // session switch that arrives after the switch).
        if (patch.session.id !== prev.session.id || patch.revision < prev.revision) {
          scheduleFullResync();
          return prev;
        }
        // Continuity check: each patch diffs from `base_revision` (the bridge
        // cursor's last-emitted state). Coalesced revision bumps make jumps
        // NORMAL — a matching base means the patch is self-contained, apply
        // directly. A mismatch means emitted patch events were lost in IPC;
        // repair incrementally from the bridge's replay buffer instead of
        // refetching the whole snapshot (a multi-MB clone + main-thread
        // JSON.parse on long sessions, previously triggered on nearly every
        // streaming patch because bursts skip revision numbers).
        const baseMatches =
          patch.base_revision === 0 || patch.base_revision == null
            ? patch.revision === prev.revision + 1
            : patch.base_revision === prev.revision;
        // Same-revision re-deliveries fall through to the delta dedupe below;
        // only an AHEAD revision with a broken base means lost patch events.
        if (!baseMatches && patch.revision > prev.revision) {
          void replayPatchesSince(prev.revision);
          return prev;
        }

        lastAcceptedUpdateAtRef.current = Date.now();
        prevSnapshotSessionId.current = patch.session.id;
        prevSnapshotRevision.current = Math.max(prev.revision, patch.revision);

        // Streaming deltas mutate the append-only stream store, so they may
        // only be applied once the patch is actually accepted — never for a
        // stale/gap patch (those deltas are computed against a base the local
        // store does not hold). Misaligned deltas (store length ≠ the
        // backend's base_len) are skipped and repaired by a full re-snapshot:
        // appending them would desync the store permanently.
        if (hasDeltas && !appliedDeltaPatches.has(patch)) {
          appliedDeltaPatches.add(patch);
          if (applyStreamingDeltas(patch, prev.messages)) {
            scheduleFullResync();
          }
        }

        // Delta-only patches intentionally omit `messages`. Fold the live
        // stream store back into snapshot bodies so Idle/final renders keep
        // the full assistant text instead of a truncated prefix.
        if (isStreamingDeltaOnlyPatch(patch) || (hasDeltas && patch.messages.length === 0)) {
          return materializeStreamingMessageBodies({
            ...prev,
            revision: Math.max(prev.revision, patch.revision),
            session: patch.session,
            session_config: patch.session_config ?? prev.session_config,
            // `thinking_status` is `ThinkingStatus | null`; the backend always
            // sends it (even as null for an idle turn). Using `??` would treat
            // `null` as nullish and keep the previous "Active" state, leaking
            // a stale thinking indicator. Accept the patch value verbatim.
            thinking_status: patch.thinking_status,
            thinking_text: patch.thinking_text ?? prev.thinking_text,
            pending_steers: patch.pending_steers ?? prev.pending_steers,
          });
        }

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
      if (reconcileTimer !== 0) {
        window.clearTimeout(reconcileTimer);
        reconcileTimer = 0;
      }
      unlisten?.();
      unlistenPatch?.();
    };
  }, [pollState]);

  // Periodic full-snapshot reconciliation. Catches patch loss that never
  // surfaces as a revision gap (e.g. the last deltas of a turn are dropped and
  // no further patch arrives to trigger the gap check): the backend revision
  // stays ahead of ours, so the next poll replaces the truncated local state
  // with the complete reply.
  useEffect(() => {
    if (!workspaceReady) return;
    let cancelled = false;
    const interval = window.setInterval(() => {
      // Probe the cheap revision endpoint first; only pay for a full snapshot
      // clone + serialization when the backend actually advanced. Long sessions
      // make `session_get_state` expensive, so this keeps the 3s poll light.
      sessionGetRevision()
        .then(([sessionId, revision]) => {
          if (cancelled) return;
          const changed =
            sessionId !== prevSnapshotSessionId.current ||
            revision !== prevSnapshotRevision.current;
          if (!changed) return;
          // While updates are flowing normally the revision advances through
          // accepted patches/events and this probe stays a no-op. Only treat
          // an ahead revision as a suspected loss when nothing has been
          // accepted for a while (dropped events / throttled webview) —
          // refetching the full snapshot for every streaming revision bump
          // was a multi-MB main-thread freeze every 3s on long sessions.
          if (Date.now() - lastAcceptedUpdateAtRef.current < 8000) return;
          void pollState();
        })
        .catch(() => {});
    }, SNAPSHOT_SELF_HEAL_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [workspaceReady, pollState]);

  useEffect(() => {
    if (!workspaceReady || snapshot) return;
    pollState();
  }, [pollState, snapshot, workspaceReady]);

  // Page older history (before the loaded window's earliest seq) and prepend
  // it into the local snapshot. Returns false when there's nothing older.
  const loadOlderHistory = useCallback(async (limit = 200): Promise<boolean> => {
    const current = snapshotRef.current;
    const earliest = current?.history_earliest_seq;
    if (!current || earliest == null) return false;
    // 绑定发起时的会话：后端在**执行时刻**按当时的当前会话查询，等待期间的
    // 会话切换会让本页属于另一个对话。错会话的页面必须丢弃——否则历史某个
    // 对话的内容会被合并进当前会话（内容覆盖污染）。
    const sessionId = current.session.id;
    try {
      const page = await sessionLoadHistoryBefore(earliest, limit);
      if (page.session_id !== sessionId) return false;
      if (page.timeline.length === 0) return false;
      if (snapshotRef.current?.session.id !== sessionId) return false;
      setSnapshot((prev) => {
        if (!prev || prev.session.id !== sessionId) return prev;
        // Dedupe by id in case of overlap with the current window.
        const knownMessageIds = new Set(prev.messages.map((m) => m.id));
        const knownToolIds = new Set(prev.tools.map((t) => t.id));
        const knownTimeline = new Set(
          prev.timeline.map((item) =>
            typeof item === "object" && "Message" in item
              ? `m:${item.Message}`
              : typeof item === "object" && "Tool" in item
              ? `t:${item.Tool}`
              : String(item),
          ),
        );
        const newMessages = page.messages.filter((m) => !knownMessageIds.has(m.id));
        const newTools = page.tools.filter((t) => !knownToolIds.has(t.id));
        const newTimeline = page.timeline.filter((item) => {
          const key =
            typeof item === "object" && "Message" in item
              ? `m:${item.Message}`
              : typeof item === "object" && "Tool" in item
              ? `t:${item.Tool}`
              : String(item);
          return !knownTimeline.has(key);
        });
        return {
          ...prev,
          messages: [...newMessages, ...prev.messages],
          tools: [...newTools, ...prev.tools],
          timeline: [...newTimeline, ...prev.timeline],
          history_earliest_seq: page.has_more ? page.earliest_seq : null,
        };
      });
      return true;
    } catch {
      return false;
    }
  }, []);

  return {
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady,
    pollState,
    acceptSnapshot,
    clearSnapshot,
    clearWorkspace,
    loadOlderHistory,
  };
}
