import { act, renderHook, waitFor } from "@testing-library/react";
import { useRef, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UiSnapshot } from "../../types";
import { useWorkbenchGit } from "./useWorkbenchGit";

const { gitRefresh } = vi.hoisted(() => ({ gitRefresh: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ gitRefresh }));

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

function useHarness(initial: UiSnapshot) {
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(initial);
  const snapshotRef = useRef<UiSnapshot | null>(initial);
  snapshotRef.current = snapshot;
  const git = useWorkbenchGit({
    snapshot,
    setSnapshot,
    snapshotRef,
    workspaceReady: true,
  });
  return { snapshot, git };
}

describe("useWorkbenchGit", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    gitRefresh.mockResolvedValue(makeSnapshot().repository);
  });

  it("re-hydrates after a reset that changes nothing else", async () => {
    // Resetting used to clear `gitHydrated` without re-running the hydration
    // effect: callers that reset on a switch which keeps the same session id
    // (creating a chat reuses the chats placeholder session, clicking the
    // already active session) left every dependency identical, so hydration
    // stayed cleared forever and the review panel's change-set list went blank
    // while the timeline still showed the turn's files.
    const { result } = renderHook(() => useHarness(makeSnapshot()));

    await waitFor(() => expect(result.current.git.gitHydrated).toBe(true));
    expect(gitRefresh).toHaveBeenCalledTimes(1);

    act(() => result.current.git.resetGitHydration());

    await waitFor(() => expect(result.current.git.gitHydrated).toBe(true));
    expect(gitRefresh).toHaveBeenCalledTimes(2);
    expect(result.current.git.gitRefreshing).toBe(false);
  });
});
