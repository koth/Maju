import { describe, it, expect, beforeEach } from "vitest";
import { applySnapshotPatch, applyToolUpdated, applySessionStatus } from "../session/reducer";
import { SessionStore } from "../session/store";
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
  store = new SessionStore();
  store.setSnapshot(makeSnapshot({ revision: 5 }));
  });

  it("SnapshotFull replaces", () => {
  const fresh = makeSnapshot({ revision: 9, session: { ...makeSnapshot().session, id: "s-9" } });
  store.setSnapshot(fresh);
  expect(store.state?.revision).toBe(9);
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
});
// end of file
