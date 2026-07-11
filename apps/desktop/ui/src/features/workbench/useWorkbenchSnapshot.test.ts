import { describe, expect, it } from "vitest";
import type { ToolInvocation, UiSnapshot, UiSnapshotPatch } from "../../types";
import { replaceStreamingMessageBody } from "../conversation/streaming-message-store";
import { applySnapshotPatch, materializeStreamingMessageBodies } from "./useWorkbenchSnapshot";

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

function makeTool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "tool-1",
    call_id: "call-1",
    parent_call_id: null,
    name: "Read",
    kind: "read",
    summary: "Read file",
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
    ...overrides,
  };
}

function makePatch(snapshot: UiSnapshot, overrides: Partial<UiSnapshotPatch> = {}): UiSnapshotPatch {
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
    ...overrides,
  };
}

describe("materializeStreamingMessageBodies", () => {
  it("uses a longer streaming body to complete stale snapshot text", () => {
    replaceStreamingMessageBody("msg-stream-complete", "\n\n##xxxx\n\n#### yy");
    const snapshot = makeSnapshot({
      messages: [{ id: "msg-stream-complete", role: "Assistant", body: "\n\n##" }],
      timeline: [{ Message: "msg-stream-complete" }],
    });

    const next = materializeStreamingMessageBodies(snapshot);

    expect(next.messages[0].body).toBe("\n\n##xxxx\n\n#### yy");
  });

  it("does not overwrite a newer final snapshot body with stale streaming text", () => {
    replaceStreamingMessageBody("msg-final-complete", "\n\n##");
    const snapshot = makeSnapshot({
      messages: [
        {
          id: "msg-final-complete",
          role: "Assistant",
          body: "\n\n##xxxx\n\n#### yy",
        },
      ],
      timeline: [{ Message: "msg-final-complete" }],
    });

    const next = materializeStreamingMessageBodies(snapshot);

    expect(next).toBe(snapshot);
    expect(next.messages[0].body).toBe("\n\n##xxxx\n\n#### yy");
  });
});

describe("applySnapshotPatch", () => {
  it("does not let a stale assistant message update shorten displayed text", () => {
    const snapshot = makeSnapshot({
      messages: [{ id: "msg-1", role: "Assistant", body: "I will inspect the file first." }],
      timeline: [{ Message: "msg-1" }],
    });
    const patch = makePatch(snapshot, {
      messages: [{ id: "msg-1", role: "Assistant", body: "I will inspect" }],
    });

    const next = applySnapshotPatch(snapshot, patch);

    expect(next.messages[0].body).toBe("I will inspect the file first.");
  });

  it("keeps streamed assistant text materialized when a tool card is appended", () => {
    replaceStreamingMessageBody("msg-before-tool", "I will inspect the file first.");
    const snapshot = makeSnapshot({
      session: { ...makeSnapshot().session, status: "Streaming" },
      messages: [{ id: "msg-before-tool", role: "Assistant", body: "I will inspect" }],
      timeline: [{ Message: "msg-before-tool" }],
    });
    const tool = makeTool();
    const patch = makePatch(snapshot, {
      session: { ...snapshot.session, status: "WaitingForTool" },
      timeline: [{ Tool: tool.id }],
      tools: [tool],
    });

    const next = materializeStreamingMessageBodies(applySnapshotPatch(snapshot, patch));

    expect(next.timeline).toEqual([{ Message: "msg-before-tool" }, { Tool: "tool-1" }]);
    expect(next.messages[0].body).toBe("I will inspect the file first.");
  });

  it("carries pending steers from the patch replacement list", () => {
    const snapshot = makeSnapshot({
      pending_steers: [{ message_id: "steer-1", body: "改为处理登录" }],
    });
    // Backend moves the steer into the timeline and clears pending_steers.
    const patch = makePatch(snapshot, { pending_steers: [] });

    const next = applySnapshotPatch(snapshot, patch);

    expect(next.pending_steers).toEqual([]);
  });

  it("renders a newly queued steer from the patch", () => {
    const snapshot = makeSnapshot({ pending_steers: [] });
    const patch = makePatch(snapshot, {
      pending_steers: [{ message_id: "steer-1", body: "追加：先看日志" }],
    });

    const next = applySnapshotPatch(snapshot, patch);

    expect(next.pending_steers).toEqual([
      { message_id: "steer-1", body: "追加：先看日志" },
    ]);
  });
});
