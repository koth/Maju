import { describe, expect, it, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { UiSnapshot, UiSnapshotPatch } from "../../types";
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
): UiSnapshotPatch {
  return {
    revision: snapshot.revision,
    session: snapshot.session,
    session_config: snapshot.session_config,
    prompt_capabilities: snapshot.prompt_capabilities,
    available_commands: snapshot.available_commands,
    agent_plan: snapshot.agent_plan,
    messages: [],
    message_deltas: [{ id: messageId, append }],
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
