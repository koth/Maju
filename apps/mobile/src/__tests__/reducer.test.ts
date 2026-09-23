import { describe, it, expect, beforeEach } from "vitest";
import {
  applySnapshotPatch,
  applyToolUpdated,
  applySessionStatus,
  materializeStreamingMessageBodies,
} from "../session/reducer";
import { SessionStore } from "../session/store";
import { clearAllStreamingMessages } from "../session/streaming-message-store";
import type { UiSnapshot, UiSnapshotPatch, ToolInvocation, ChatMessage } from "../types";

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
  revision: 1,
  workspace: { id: "ws-1", name: "demo", root: "/demo" },
  workspace_connected: true,
  session: {
  id: "s-1",
  workspace_id: "ws-1",
  title: "Session",
  model: "m",
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

function patchFor(snapshot: UiSnapshot, over: Partial<UiSnapshotPatch> = {}): UiSnapshotPatch {
  return {
  revision: snapshot.revision + 1,
  session: snapshot.session,
  session_config: snapshot.session_config,
  prompt_capabilities: snapshot.prompt_capabilities,
  available_commands: snapshot.available_commands,
  agent_plan: snapshot.agent_plan,
  messages: [],
  message_deltas: [],
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
  ...over,
  };
}

const userMsg = (id: string, body: string): ChatMessage => ({
  id,
  role: "User",
  body,
  created_at: "",
});
const tool = (id: string, name: string): ToolInvocation => ({
  id,
  call_id: id,
  parent_call_id: null,
  name,
  kind: "other",
  summary: name,
  status: "Running",
  is_subagent: false,
  detail_text: "",
  logs: [],
  diff_paths: [],
  diff_previews: [],
  raw_input: null,
  raw_output: null,
  terminal_output: null,
  error: null,
  permission_options: [],
  permission_input: null,
  permission_decision: null,
  can_stop: false,
  stop_kind: null,
  stop_status: null,
});

describe("applySnapshotPatch", () => {
  it("merges messages and tools by id, appending new ones", () => {
  const snap = makeSnapshot({ messages: [userMsg("m1", "a")], tools: [tool("t1", "run")] });
  const p = patchFor(snap, {
  messages: [userMsg("m1", "a-updated"), userMsg("m2", "new")],
  tools: [tool("t1", "run-updated"), tool("t2", "new")],
  });
  const out = applySnapshotPatch(snap, p);
  expect(out.messages.map((m) => m.id)).toEqual(["m1", "m2"]);
  expect(out.messages[0].body).toBe("a-updated");
  expect(out.tools.map((t) => t.id)).toEqual(["t1", "t2"]);
  expect(out.tools[0].summary).toBe("run-updated");
  });

  it("empty patch lists preserve the prior messages/tools", () => {
  const snap = makeSnapshot({ messages: [userMsg("m1", "a")], tools: [tool("t1", "run")] });
  const out = applySnapshotPatch(snap, patchFor(snap));
  expect(out.messages).toBe(snap.messages);
  expect(out.tools).toBe(snap.tools);
  });

  it("timeline splices at timeline_start", () => {
  const snap = makeSnapshot({
  timeline: [{ Message: "m1" }, { Message: "m2" }, { Message: "m3" }],
  });
  const p = patchFor(snap, {
  timeline_start: 1,
  timeline: [{ Tool: "t1" }],
  });
  const out = applySnapshotPatch(snap, p);
  expect(out.timeline).toEqual([{ Message: "m1" }, { Tool: "t1" }]);
  });

  it("applies patch usage so the session-info sheet sees context/token updates", () => {
  const snap = makeSnapshot();
  const out = applySnapshotPatch(
  snap,
  patchFor(snap, {
  usage: {
  context: { used_tokens: 1200, window_tokens: 950000, updated_at: null },
  current_turn: { total_tokens: 300 },
  session_total: { total_tokens: 4500 },
  by_model: [],
  },
  }),
  );
  expect(out.usage?.context?.used_tokens).toBe(1200);
  expect(out.usage?.context?.window_tokens).toBe(950000);
  expect(out.usage?.session_total?.total_tokens).toBe(4500);
  });

  it("patch without usage preserves the prior usage", () => {
  const seeded = applySnapshotPatch(
  makeSnapshot(),
  patchFor(makeSnapshot(), {
  usage: {
  context: { used_tokens: 1200, window_tokens: 950000, updated_at: null },
  current_turn: {},
  session_total: {},
  by_model: [],
  },
  }),
  );
  const out = applySnapshotPatch(seeded, patchFor(seeded));
  expect(out.usage?.context?.used_tokens).toBe(1200);
  });

  it("absent optional fields coalesce with prior", () => {
  const snap = makeSnapshot();
  const out = applySnapshotPatch(snap, patchFor(snap, { repository: null }));
  expect(out.repository).toBe(snap.repository);
  });
});

describe("applyToolUpdated / applySessionStatus", () => {
  it("ToolUpdated merges by id", () => {
  const snap = makeSnapshot({ tools: [tool("t1", "run")] });
  const out = applyToolUpdated(snap, tool("t1", "done"));
  expect(out.tools).toHaveLength(1);
  expect(out.tools[0].summary).toBe("done");
  });

  it("SessionStatusChanged updates status", () => {
  const snap = makeSnapshot();
  expect(applySessionStatus(snap, "Streaming").session.status).toBe("Streaming");
  });
});

describe("SessionStore guard", () => {
  let store: SessionStore;
  beforeEach(() => {
    clearAllStreamingMessages();
    store = new SessionStore();
    store.setSnapshot(makeSnapshot({ revision: 5 }));
  });

  it("SnapshotFull replaces", () => {
    // 换会话的全量替换要显式 beginSession（切换语义）；直接 setSnapshot 只
    // 替换当前会话的状态（跨会话污染守卫见 session-gating.test.ts）。
    const fresh = makeSnapshot({ revision: 9, session: { ...makeSnapshot().session, id: "s-9" } });
    store.beginSession("s-9");
    store.setSnapshot(fresh);
    expect(store.state?.revision).toBe(9);
    expect(store.state?.session.id).toBe("s-9");
  });

  it("ignores a patch for a different session", () => {
    const other = makeSnapshot({ session: { ...makeSnapshot().session, id: "s-other" } });
    const p = patchFor(other, { revision: 6 });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.revision).toBe(5);
  });

  it("ignores a stale/duplicate revision", () => {
    const p = patchFor(makeSnapshot({ revision: 5 }), { revision: 5, session: store.state!.session });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.revision).toBe(5);
  });

  it("applies a newer same-session patch", () => {
    const p = patchFor(store.state!, { revision: 6, messages: [userMsg("m1", "hi")] });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.revision).toBe(6);
    expect(store.state?.messages.map((m) => m.id)).toEqual(["m1"]);
  });

  it("drops a stale GetState response that raced behind newer patches", () => {
    // Patch landed first (revision 6), then the slower GetState response
    // (revision 5, built earlier on the PC) arrives: accepting it would
    // rewind the revision and wedge every subsequent patch guard.
    const patch = patchFor(store.state!, { revision: 6, messages: [userMsg("m1", "from patch")] });
    store.applyEventFrame({ kind: "snapshot_patch", patch });
    expect(store.state?.revision).toBe(6);
    const staleState = makeSnapshot({ revision: 5, messages: [userMsg("m0", "from getstate")] });
    store.setSnapshot(staleState);
    expect(store.state?.revision).toBe(6);
    expect(store.state?.messages.map((m) => m.id)).toEqual(["m1"]);
    // A same-session response at the SAME revision is stale too.
    store.setSnapshot(makeSnapshot({ revision: 6, messages: [userMsg("m2", "dupe")] }));
    expect(store.state?.messages.map((m) => m.id)).toEqual(["m1"]);
    // 别的会话的快照默认丢弃（跨会话污染守卫）；显式 beginSession（切换）
    // 之后才收编。
    const heldId = store.state!.session.id;
    const foreign = makeSnapshot({ revision: 1, session: { ...makeSnapshot().session, id: "s-next" } });
    store.setSnapshot(foreign);
    expect(store.state?.session.id).toBe(heldId);
    store.beginSession("s-next");
    store.setSnapshot(foreign);
    expect(store.state?.session.id).toBe("s-next");
  });
});

describe("streaming delta sync", () => {
  beforeEach(() => {
    clearAllStreamingMessages();
  });

  const assistant = (id: string, body: string): ChatMessage => ({
    id,
    role: "Assistant",
    body,
    created_at: "",
  });

  it("delta-only patches grow the assistant text in the snapshot", () => {
    const store = new SessionStore();
    const base = makeSnapshot({
      revision: 5,
      messages: [assistant("m1", "Hel")],
      timeline: [{ Message: "m1" }],
    });
    store.setSnapshot(base);
    // The PC streams via delta-only patches: empty messages/timeline/tools.
    const p = patchFor(base, {
      revision: 6,
      session: { ...base.session, status: "Streaming" },
      message_deltas: [{ id: "m1", append: "lo world" }],
    });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.revision).toBe(6);
    expect(store.state?.messages[0].body).toBe("Hello world");
  });

  it("delta + tool updates in one patch both land", () => {
    const store = new SessionStore();
    const base = makeSnapshot({
      revision: 5,
      messages: [assistant("m1", "")],
      tools: [tool("t1", "run")],
      timeline: [{ Message: "m1" }, { Tool: "t1" }],
    });
    store.setSnapshot(base);
    const p = patchFor(base, {
      revision: 6,
      message_deltas: [{ id: "m1", append: "text" }],
      tools: [tool("t1", "running")],
    });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.messages[0].body).toBe("text");
    expect(store.state?.tools[0].summary).toBe("running");
  });

  it("a same-revision patch with deltas still lands", () => {
    const store = new SessionStore();
    const base = makeSnapshot({
      revision: 5,
      messages: [assistant("m1", "par")],
      timeline: [{ Message: "m1" }],
    });
    store.setSnapshot(base);
    const p = patchFor(base, {
      revision: 5,
      message_deltas: [{ id: "m1", append: "tial" }],
    });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.messages[0].body).toBe("partial");
  });

  it("materialize prefers the longer continuation but never shrinks a full body", () => {
    const snap = makeSnapshot({ messages: [assistant("m1", "full body")] });
    // Streaming store holds a stale prefix: snapshot body wins.
    expect(materializeStreamingMessageBodies(snap).messages[0].body).toBe("full body");
  });

  it("a revision gap triggers a resync request instead of a misaligned merge", () => {
    const store = new SessionStore();
    store.setSnapshot(makeSnapshot({ revision: 5 }));
    let resyncs = 0;
    store.setResyncHandler(() => {
      resyncs += 1;
    });
    // Patch 7 skips revision 6: a frame was lost on the wire.
    const p = patchFor(store.state!, { revision: 7, messages: [userMsg("m1", "x")] });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(resyncs).toBe(1);
    expect(store.state?.revision).toBe(5);
    // Consecutive in-order patches do not request resyncs.
    const ok = patchFor(store.state!, { revision: 6, messages: [userMsg("m2", "y")] });
    store.applyEventFrame({ kind: "snapshot_patch", patch: ok });
    expect(resyncs).toBe(1);
    expect(store.state?.revision).toBe(6);
    store.setResyncHandler(null);
  });

  it("beginSession drops accumulated streaming bodies from the previous session", () => {
    const store = new SessionStore();
    const base = makeSnapshot({
      revision: 5,
      messages: [assistant("m1", "old")],
      timeline: [{ Message: "m1" }],
    });
    store.setSnapshot(base);
    const p = patchFor(base, { revision: 6, message_deltas: [{ id: "m1", append: "bodyXYZ" }] });
    store.applyEventFrame({ kind: "snapshot_patch", patch: p });
    expect(store.state?.messages[0].body).toBe("oldbodyXYZ");
    // Switch to a new session that happens to reuse the message id: the
    // stale streaming body must not bleed into the fresh full snapshot.
    store.beginSession("s-2");
    store.setSnapshot(
      makeSnapshot({
        revision: 1,
        session: { ...makeSnapshot().session, id: "s-2" },
        messages: [assistant("m1", "old")],
        timeline: [{ Message: "m1" }],
      }),
    );
    expect(store.state?.messages[0].body).toBe("old");
  });
});
// end of file
