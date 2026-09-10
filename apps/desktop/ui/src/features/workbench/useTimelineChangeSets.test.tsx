import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChangeSetSummary, FileChangeSummary, UiSnapshot } from "../../types";
import { sessionListChangeSetFiles, sessionListChangeSets } from "../../lib/tauri";
import { useTimelineChangeSets } from "./useTimelineChangeSets";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    sessionListChangeSets: vi.fn(),
    sessionListChangeSetFiles: vi.fn(),
  };
});

function makeSummary(
  changeSetId: string,
  path: string,
  updatedAt = "2026-05-12T00:00:00Z",
): FileChangeSummary {
  return {
    change_set_id: changeSetId,
    path,
    change_type: "Modified",
    added_lines: 1,
    removed_lines: 0,
    quality: "Exact",
    updated_at: updatedAt,
  };
}

function makeTurnChangeSet(
  id: string,
  messageId: string,
  updatedAt = "2026-05-12T00:00:00Z",
): ChangeSetSummary {
  return {
    id,
    source: "AgentTurn",
    session_id: "s-1",
    workspace_root: "/repo",
    message_id: messageId,
    tool_call_id: null,
    owner_key: null,
    label: id,
    added_lines: 1,
    removed_lines: 0,
    file_count: 1,
    updated_at: updatedAt,
    status: "Complete",
  };
}

function snapshotFor(sessionId: string): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "test", root: "/repo" },
    session: {
      id: sessionId,
      workspace_id: "ws-1",
      title: "test",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
    session_config: { hydrated: true, controls: [] },
    prompt_capabilities: { image: false, embedded_context: false, session_steer: false },
    available_commands: [],
    agent_plan: [],
    messages: [{ id: "m-1", role: "Assistant", body: "done" }],
    timeline: [{ Message: "m-1" }],
    tools: [],
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
  };
}

beforeEach(() => {
  vi.mocked(sessionListChangeSets).mockReset();
  vi.mocked(sessionListChangeSetFiles).mockReset();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("useTimelineChangeSets", () => {
  it("serves a session's change-set files from cache when it is re-opened", async () => {
    // Switching to another session and back is the ordinary way to review two
    // sessions; the second visit must not re-list every historical change set.
    // `sessionListChangeSets` takes an optional request, so destructure from a
    // defaulted one — the hook always passes a session-scoped request.
    vi.mocked(sessionListChangeSets).mockImplementation(async (request) => {
      const { source, session_id } = request ?? {};
      return source === "AgentTurn"
        ? [makeTurnChangeSet(`turn-${session_id}`, `msg-${session_id}`)]
        : [];
    });
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files: [makeSummary(change_set_id, `src/${change_set_id}.ts`)],
    }));

    const snapshotRef = { current: snapshotFor("s-1") as UiSnapshot | null };
    const { result, rerender } = renderHook(
      ({ value }: { value: UiSnapshot }) =>
        useTimelineChangeSets({
          snapshot: value,
          snapshotRef,
          workspaceReady: true,
          onGitRefresh: () => {},
        }),
      { initialProps: { value: snapshotFor("s-1") } },
    );

    await waitFor(() => expect(result.current.timelineTurnChangeSets["msg-s-1"]).toBeTruthy());
    expect(vi.mocked(sessionListChangeSetFiles)).toHaveBeenCalledTimes(1);

    const other = snapshotFor("s-2");
    snapshotRef.current = other;
    rerender({ value: other });
    await waitFor(() => expect(result.current.timelineTurnChangeSets["msg-s-2"]).toBeTruthy());
    const callsAfterOtherSession = vi.mocked(sessionListChangeSetFiles).mock.calls.length;
    expect(callsAfterOtherSession).toBe(2);

    const back = snapshotFor("s-1");
    snapshotRef.current = back;
    rerender({ value: back });
    await waitFor(() => expect(result.current.timelineTurnChangeSets["msg-s-1"]).toBeTruthy());

    expect(
      vi.mocked(sessionListChangeSetFiles).mock.calls.length,
      "re-opening the first session must reuse the cached file list",
    ).toBe(callsAfterOtherSession);
    expect(result.current.timelineTurnChangeSets["msg-s-1"].files[0].path).toBe(
      "src/turn-s-1.ts",
    );
  });
});
