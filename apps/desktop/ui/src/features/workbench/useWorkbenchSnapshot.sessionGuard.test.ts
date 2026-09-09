import { describe, expect, it, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { UiSnapshot, UiSnapshotPatch } from "../../types";
import {
  getStreamingMessageBody,
  replaceStreamingMessageBody,
} from "../conversation/streaming-message-store";
import { useWorkbenchSnapshot } from "./useWorkbenchSnapshot";

// ── Mocks ──────────────────────────────────────────────────────────
// We need to control the Tauri event callbacks and sessionGetState to
// simulate the race condition where a stale event from the previous
// session blocks the new session's snapshot.

let snapshotCallback: ((snapshot: UiSnapshot) => void) | null = null;
let patchCallback: ((patch: UiSnapshotPatch) => void) | null = null;
let mockSessionGetState: (() => Promise<UiSnapshot>) | null = null;

vi.mock("../../lib/events", () => ({
  onUiSnapshot: vi.fn((cb: (snapshot: UiSnapshot) => void) => {
    snapshotCallback = cb;
    return Promise.resolve(() => {
      snapshotCallback = null;
    });
  }),
  onUiSnapshotPatch: vi.fn((cb: (patch: UiSnapshotPatch) => void) => {
    patchCallback = cb;
    return Promise.resolve(() => {
      patchCallback = null;
    });
  }),
}));

vi.mock("../../lib/tauri", () => ({
  startupPerfMark: vi.fn(() => Promise.resolve()),
  sessionGetState: vi.fn(() => {
    if (mockSessionGetState) return mockSessionGetState();
    return Promise.reject(new Error("no mock"));
  }),
  // Derive the revision probe from the full-state mock so existing fixtures
  // keep working: the probe reports whatever snapshot the poll would return.
  sessionGetRevision: vi.fn(async () => {
    if (!mockSessionGetState) return Promise.reject(new Error("no mock"));
    const snapshot = await mockSessionGetState();
    return [snapshot.session.id, snapshot.revision] as [string, number];
  }),
}));

// ── Fixtures ───────────────────────────────────────────────────────

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "test", root: "/test" },
    session: {
      id: "s-1",
      workspace_id: "ws-1",
      title: "test",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
    session_config: { hydrated: false, controls: [] },
    prompt_capabilities: { image: false, embedded_context: false, session_steer: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools: [],
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
    ...overrides,
  };
}

function makeFullPatch(snapshot: UiSnapshot, overrides: Partial<UiSnapshotPatch> = {}): UiSnapshotPatch {
  return {
    revision: snapshot.revision,
    session: snapshot.session,
    session_config: snapshot.session_config,
    prompt_capabilities: snapshot.prompt_capabilities,
    available_commands: snapshot.available_commands,
    agent_plan: snapshot.agent_plan,
    messages: snapshot.messages,
    message_deltas: [],
    timeline_start: 0,
    timeline: snapshot.timeline,
    tools: snapshot.tools,
    repository: snapshot.repository,
    inspector_tab: snapshot.inspector_tab,
    inspector_sections: snapshot.inspector_sections,
    session_changes: snapshot.session_changes,
    review_changes: snapshot.review_changes,
    turn_changes: snapshot.turn_changes,
    thinking_status: snapshot.thinking_status,
    ...overrides,
  };
}

function makeStreamingDeltaPatch(
  snapshot: UiSnapshot,
  messageId: string,
  append: string,
  baseLen = 0,
): UiSnapshotPatch {
  return {
    revision: snapshot.revision,
    session: snapshot.session,
    session_config: snapshot.session_config,
    prompt_capabilities: snapshot.prompt_capabilities,
    available_commands: snapshot.available_commands,
    agent_plan: snapshot.agent_plan,
    messages: [],
    message_deltas: [{ id: messageId, append, base_len: baseLen }],
    timeline_start: snapshot.timeline.length,
    timeline: [],
    tools: [],
    repository: null,
    inspector_tab: snapshot.inspector_tab,
    inspector_sections: snapshot.inspector_sections,
    session_changes: snapshot.session_changes,
    review_changes: snapshot.review_changes,
    turn_changes: snapshot.turn_changes,
    thinking_status: snapshot.thinking_status,
  };
}

// ── Tests ──────────────────────────────────────────────────────────

beforeEach(() => {
  snapshotCallback = null;
  patchCallback = null;
  mockSessionGetState = null;
  vi.clearAllMocks();
});

describe("useWorkbenchSnapshot – session-id revision collision guard", () => {
  it("accepts a new session's full snapshot even when its revision matches a stale event from the previous session", async () => {
    // Both sessions happen to have the same revision count → same revision
    // number after the session_switch bump.
    const sessionB = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-b", title: "B" },
    });
    const sessionA = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-a", title: "A" },
    });

    const { result } = renderHook(() => useWorkbenchSnapshot());

    // 1. Accept session B as the initial visible snapshot.
    await act(async () => {
      result.current.acceptSnapshot(sessionB);
    });
    expect(result.current.snapshot?.session.id).toBe("session-b");

    // 2. User switches to session A — clearSnapshot resets the tracking refs.
    await act(async () => {
      result.current.clearSnapshot();
    });
    expect(result.current.snapshot).toBeNull();

    // 3. A stale streaming-delta patch from session B (revision 7) arrives
    //    after clearSnapshot. Before the fix this would set
    //    prevSnapshotRevision=7, and because the new session also has
    //    revision 7, the subsequent full snapshot would be wrongly ignored.
    await act(async () => {
      patchCallback?.(makeStreamingDeltaPatch(sessionB, "msg-b", "delta"));
    });

    // 4. The bridge emits session A's full snapshot (revision 7). With the
    //    session-id guard this must be accepted because session.id differs.
    await act(async () => {
      snapshotCallback?.(sessionA);
    });

    expect(result.current.snapshot?.session.id).toBe("session-a");
    expect(result.current.snapshot?.revision).toBe(7);
  });

  it("accepts pollState result for a new session even when a stale patch pre-set the revision", async () => {
    const sessionB = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-b", title: "B" },
    });
    const sessionA = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-a", title: "A" },
    });

    const { result } = renderHook(() => useWorkbenchSnapshot());

    await act(async () => {
      result.current.acceptSnapshot(sessionB);
    });

    await act(async () => {
      result.current.clearSnapshot();
    });

    // Stale streaming-delta patch from B arrives.
    await act(async () => {
      patchCallback?.(makeStreamingDeltaPatch(sessionB, "msg-b", "delta"));
    });

    // pollState returns session A's snapshot.
    mockSessionGetState = () => Promise.resolve(sessionA);

    await act(async () => {
      await result.current.pollState();
    });

    expect(result.current.snapshot?.session.id).toBe("session-a");
  });

  it("rejects a stale non-streaming patch from a different session and polls instead", async () => {
    const sessionA = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-a", title: "A" },
    });
    const sessionB = makeSnapshot({
      revision: 8,
      session: { ...makeSnapshot().session, id: "session-b", title: "B" },
    });

    const { result } = renderHook(() => useWorkbenchSnapshot());

    // Start with session A visible.
    await act(async () => {
      result.current.acceptSnapshot(sessionA);
    });

    // pollState will be called by the patch rejection path.
    mockSessionGetState = () => Promise.resolve(sessionA);

    // A stale full patch from session B arrives (e.g. queued before switch).
    await act(async () => {
      patchCallback?.(makeFullPatch(sessionB));
    });

    // The snapshot must remain session A, not be overwritten by B's patch.
    expect(result.current.snapshot?.session.id).toBe("session-a");
  });

  it("still rejects a duplicate snapshot from the same session and revision", async () => {
    const sessionA = makeSnapshot({
      revision: 7,
      session: { ...makeSnapshot().session, id: "session-a", title: "A" },
    });

    const { result } = renderHook(() => useWorkbenchSnapshot());

    await act(async () => {
      result.current.acceptSnapshot(sessionA);
    });

    // Emit the exact same snapshot again — must be ignored.
    let emitCount = 0;
    const original = result.current.snapshot;
    await act(async () => {
      snapshotCallback?.(sessionA);
      emitCount++;
    });

    expect(emitCount).toBe(1);
    // Reference equality: setSnapshot was never called again.
    expect(result.current.snapshot).toBe(original);
  });
});

describe("useWorkbenchSnapshot – dropped patch self-heal", () => {
  it("re-syncs from a full snapshot when a revision gap signals dropped patches", async () => {
    const session = makeSnapshot({
      revision: 1,
      session: { ...makeSnapshot().session, status: "Streaming" },
      messages: [{ id: "msg-1", role: "Assistant", body: "prefix" }],
      timeline: [{ Message: "msg-1" }],
    });
    // The backend's full state after the dropped patch(es): revision jumped
    // from 1 to 3 and the message body is complete.
    const healed = makeSnapshot({
      revision: 3,
      session: { ...makeSnapshot().session, status: "Streaming" },
      messages: [{ id: "msg-1", role: "Assistant", body: "prefix + complete suffix" }],
      timeline: [{ Message: "msg-1" }],
    });

    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });

    mockSessionGetState = () => Promise.resolve(healed);

    // A patch with a revision gap (2 skipped) arrives. The local stream store
    // would otherwise append a delta computed against an unknown intermediate
    // body; instead the patch must be rejected and a full poll triggered.
    await act(async () => {
      patchCallback?.({
        ...makeStreamingDeltaPatch(session, "msg-1", " + suffix"),
        revision: 3,
      });
    });

    // The full re-sync is debounced (~120ms) to avoid back-to-back full
    // snapshot clones during a streaming patch burst; wait for it to land.
    await vi.waitFor(() => {
      expect(result.current.snapshot?.revision).toBe(3);
      expect(result.current.snapshot?.messages[0].body).toBe("prefix + complete suffix");
    });
  });

  it("periodically reconciles a truncated snapshot even when no further patch arrives", async () => {
    vi.useFakeTimers();
    try {
      const session = makeSnapshot({
        revision: 1,
        session: { ...makeSnapshot().session, status: "Idle" },
        messages: [{ id: "msg-1", role: "Assistant", body: "prefix only" }],
        timeline: [{ Message: "msg-1" }],
      });
      // The last deltas of the turn were dropped: the backend already holds
      // the complete reply but no further patch will arrive to trigger the
      // revision-gap check. The periodic poll must repair the UI.
      const healed = makeSnapshot({
        revision: 2,
        session: { ...makeSnapshot().session, status: "Idle" },
        messages: [{ id: "msg-1", role: "Assistant", body: "prefix only + final tail" }],
        timeline: [{ Message: "msg-1" }],
      });

      const { result } = renderHook(() => useWorkbenchSnapshot());
      await act(async () => {
        result.current.acceptSnapshot(session);
      });
      expect(result.current.snapshot?.messages[0].body).toBe("prefix only");

      mockSessionGetState = () => Promise.resolve(healed);

      // While updates have been accepted recently the probe is deliberately a
      // no-op (streaming bumps revisions constantly — a full refetch for each
      // advance froze the UI on long sessions). The self-heal only fires once
      // the update stream has gone quiet (>8s).
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3000);
      });
      expect(result.current.snapshot?.revision).toBe(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(6000);
      });

      expect(result.current.snapshot?.revision).toBe(2);
      expect(result.current.snapshot?.messages[0].body).toBe("prefix only + final tail");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("useWorkbenchSnapshot – streaming delta alignment guard", () => {
  function streamingSession(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
    return makeSnapshot({
      revision: 1,
      session: { ...makeSnapshot().session, status: "Streaming" },
      messages: [{ id: "msg-align", role: "Assistant", body: "prefix" }],
      timeline: [{ Message: "msg-align" }],
      ...overrides,
    });
  }

  it("applies an aligned delta and folds it into the snapshot body", async () => {
    const session = streamingSession();
    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });
    // The streaming render seeds the store from the snapshot body; the
    // backend's delta base ("prefix") has UTF-16 length 6.
    replaceStreamingMessageBody("msg-align", "prefix");

    await act(async () => {
      patchCallback?.({
        ...makeStreamingDeltaPatch(session, "msg-align", " + tail", 6),
        revision: 2,
      });
    });

    expect(result.current.snapshot?.messages[0].body).toBe("prefix + tail");
    expect(getStreamingMessageBody("msg-align")).toBe("prefix + tail");
  });

  it("seeds a missing store entry from the snapshot body when it matches base_len", async () => {
    const session = streamingSession();
    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });
    // No store entry yet (the delta raced the streaming mount).

    await act(async () => {
      patchCallback?.({
        ...makeStreamingDeltaPatch(session, "msg-align", " + tail", 6),
        revision: 2,
      });
    });

    expect(result.current.snapshot?.messages[0].body).toBe("prefix + tail");
  });

  it("skips a misaligned delta and re-syncs from a full snapshot", async () => {
    const session = streamingSession();
    const healed = streamingSession({
      revision: 2,
      messages: [{ id: "msg-align", role: "Assistant", body: "prefix + complete tail" }],
    });
    mockSessionGetState = () => Promise.resolve(healed);
    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });
    replaceStreamingMessageBody("msg-align", "prefix");

    // base_len 999 ≠ local store length 6: the delta chain desynced upstream
    // (a dropped/duplicated patch). Appending would permanently misalign the
    // store — the delta must be skipped and a full re-sync triggered instead.
    await act(async () => {
      patchCallback?.({
        ...makeStreamingDeltaPatch(session, "msg-align", " + tail", 999),
        revision: 2,
      });
    });

    await vi.waitFor(() => {
      expect(result.current.snapshot?.messages[0].body).toBe("prefix + complete tail");
      expect(getStreamingMessageBody("msg-align")).toBe("prefix + complete tail");
    });
  });

  it("does not append deltas from a revision-gap patch into the stream store", async () => {
    const session = streamingSession();
    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });
    replaceStreamingMessageBody("msg-align", "prefix");

    await act(async () => {
      patchCallback?.({
        ...makeStreamingDeltaPatch(session, "msg-align", " + suffix", 6),
        revision: 3,
      });
    });

    // The gap patch was rejected; its delta must not touch the store.
    expect(getStreamingMessageBody("msg-align")).toBe("prefix");
  });

  it("applies a duplicate-delivered patch's deltas only once", async () => {
    const session = streamingSession();
    const { result } = renderHook(() => useWorkbenchSnapshot());
    await act(async () => {
      result.current.acceptSnapshot(session);
    });
    replaceStreamingMessageBody("msg-align", "prefix");
    const patch = makeStreamingDeltaPatch(session, "msg-align", " + tail", 6);

    await act(async () => {
      patchCallback?.(patch);
      // StrictMode re-invokes the updater with the same patch object.
      patchCallback?.(patch);
      // A genuine re-delivery arrives as a fresh object with identical
      // content — the base_len guard must reject the second append.
      patchCallback?.({ ...patch });
    });

    expect(getStreamingMessageBody("msg-align")).toBe("prefix + tail");
  });
});
