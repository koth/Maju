import { describe, expect, it } from "vitest";
import type { UiSnapshot } from "../../types";
import { replaceStreamingMessageBody } from "../conversation/streaming-message-store";
import { materializeStreamingMessageBodies } from "./useWorkbenchSnapshot";

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
    prompt_capabilities: { image: false, embedded_context: false },
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
