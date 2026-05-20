import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { buildLineDiffRows } from "./ReviewPanel";
import { ReviewPanel } from "./ReviewPanel";
import type { ChangedFile, ChangeSetSummary, FileChangeRecord, FileChangeSummary, UiSnapshot } from "../../types";
import {
  fsListDir,
  sessionGetChangeSetFileDiff,
  sessionListChangeSetFiles,
  sessionListChangeSets,
} from "../../lib/tauri";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>(
    "../../lib/tauri",
  );
  return {
    ...actual,
    fsListDir: vi.fn(),
    gitStage: vi.fn(),
    sessionListChangeSets: vi.fn(),
    sessionListChangeSetFiles: vi.fn(),
    sessionGetChangeSetFileDiff: vi.fn(),
  };
});

vi.stubGlobal(
  "ResizeObserver",
  class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
);

function makeSnapshot(overrides: Partial<UiSnapshot> = {}): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "test", root: "/repo" },
    session: {
      id: "s-1",
      workspace_id: "ws-1",
      title: "test",
      model: "test-model",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
    session_config: { hydrated: true, controls: [] },
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

function makeChangeSet(
  id: string,
  source: ChangeSetSummary["source"],
  updatedAt: string,
): ChangeSetSummary {
  return {
    id,
    source,
    session_id: "s-1",
    workspace_root: "/repo",
    message_id: source === "AgentTurn" ? `${id}-message` : null,
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

function makeSummary(changeSetId: string, path: string): FileChangeSummary {
  return {
    change_set_id: changeSetId,
    path,
    change_type: "Modified",
    added_lines: 1,
    removed_lines: 0,
    quality: "Exact",
    updated_at: "2026-05-12T00:00:00Z",
  };
}

function makeRecord(changeSetId: string, path: string): FileChangeRecord {
  return {
    ...makeSummary(changeSetId, path),
    old_text: "old\n",
    new_text: "new\n",
  };
}

function makeChangedFile(path: string, section: ChangedFile["section"]): ChangedFile {
  return {
    path,
    section,
    stats: { added: 1, removed: 0 },
    patch_status: section === "Staged" ? "Staged" : "Proposed",
    hunks: [],
  };
}

beforeEach(() => {
  vi.mocked(fsListDir).mockResolvedValue([]);
  vi.mocked(sessionListChangeSets).mockResolvedValue([]);
  vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
    change_set_id: "empty",
    files: [],
  });
  vi.mocked(sessionGetChangeSetFileDiff).mockImplementation(async ({ change_set_id, path }) =>
    makeRecord(change_set_id, path),
  );
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("ReviewPanel inline diff", () => {
  it("keeps unchanged lines between distant large-file edits as context", () => {
    const oldLines = Array.from({ length: 700 }, (_, index) => `line ${index + 1}`);
    const newLines = [...oldLines];
    const insertedModelLines = Array.from(
      { length: 39 },
      (_, index) => `added model line ${index + 1}`,
    );
    newLines.splice(29, 14, ...insertedModelLines);
    newLines.splice(640, 1, "line 616 changed");

    const rows = buildLineDiffRows(oldLines, newLines);

    expect(rows.filter((line) => line.kind === "removed")).toHaveLength(15);
    expect(rows.filter((line) => line.kind === "added")).toHaveLength(40);
    expect(rows.find((line) => line.oldLine === 85 && line.text === "line 85")?.kind).toBe(
      "context",
    );
    expect(rows.find((line) => line.oldLine === 500 && line.text === "line 500")?.kind).toBe(
      "context",
    );
  });
});

describe("ReviewPanel scoped change sets", () => {
  it("renders visible review scopes from their own change sets only", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("turn-1", "AgentTurn", "2026-05-12T03:00:00Z"),
      makeChangeSet("conversation-1", "AgentConversation", "2026-05-12T02:00:00Z"),
      makeChangeSet("manual-1", "ManualEdit", "2026-05-12T01:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "turn-1"
          ? [makeSummary("turn-1", "src/turn.ts")]
          : change_set_id === "conversation-1"
            ? [makeSummary("conversation-1", "src/conversation.ts")]
            : [makeSummary("manual-1", "src/manual.ts")],
    }));
    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [
            {
              id: "turn-1-message",
              role: "Assistant",
              body: "done",
            },
          ],
          timeline: [{ Message: "turn-1-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/turn.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/conversation.ts")).toHaveLength(0);
    expect(screen.queryAllByText("src/manual.ts")).toHaveLength(0);
    await waitFor(() =>
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "turn-1",
        path: "src/turn.ts",
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: /上轮对话/ }));
    expect(screen.queryByRole("menuitem", { name: "整体对话" })).toBeNull();
    fireEvent.click(screen.getByRole("menuitem", { name: "手工修改" }));
    await waitFor(() =>
      expect(screen.queryAllByText("src/manual.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/turn.ts")).toHaveLength(0);
    expect(screen.queryAllByText("src/conversation.ts")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "src/manual.ts" })).toBeNull();
  });

  it("does not fall back to an older agent turn when the latest assistant turn has no files", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("old-turn", "AgentTurn", "2026-05-12T03:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "old-turn",
      files: [makeSummary("old-turn", "src/old.ts")],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [
            {
              id: "old-turn-message",
              role: "Assistant",
              body: "edited files",
            },
            {
              id: "latest-message",
              role: "Assistant",
              body: "no file changes this time",
            },
          ],
          timeline: [{ Message: "old-turn-message" }, { Message: "latest-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    expect(await screen.findByText("上轮对话暂无文件变化")).toBeTruthy();
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
  });

  it("shows the previous completed turn while the current turn is active", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("previous-turn", "AgentTurn", "2026-05-12T03:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "previous-turn",
      files: [makeSummary("previous-turn", "src/previous.ts")],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          session: {
            ...makeSnapshot().session,
            status: "Streaming",
          },
          messages: [
            {
              id: "previous-turn-message",
              role: "Assistant",
              body: "edited files",
            },
            {
              id: "current-user",
              role: "User",
              body: "new request",
            },
          ],
          timeline: [{ Message: "previous-turn-message" }, { Message: "current-user" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/previous.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("上轮对话暂无文件变化")).toBeNull();
  });

  it("does not skip a no-change previous turn to show an older active-turn diff", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("old-turn", "AgentTurn", "2026-05-12T03:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "old-turn",
      files: [makeSummary("old-turn", "src/old.ts")],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          session: {
            ...makeSnapshot().session,
            status: "Streaming",
          },
          messages: [
            {
              id: "old-turn-message",
              role: "Assistant",
              body: "edited files",
            },
            {
              id: "previous-no-change",
              role: "Assistant",
              body: "no file changes this time",
            },
            {
              id: "current-user",
              role: "User",
              body: "new request",
            },
          ],
          timeline: [
            { Message: "old-turn-message" },
            { Message: "previous-no-change" },
            { Message: "current-user" },
          ],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    expect(await screen.findByText("上轮对话暂无文件变化")).toBeTruthy();
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
  });

  it("opens Git rows with live scoped change set ids", async () => {
    const onFileSelect = vi.fn();

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          repository: {
            branch: "main",
            head: "abc",
            changed_files: [
              makeChangedFile("src/unstaged.ts", "Unstaged"),
              makeChangedFile("src/staged.ts", "Staged"),
              makeChangedFile("src/untracked.ts", "Untracked"),
            ],
          },
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={onFileSelect}
        onFileOpen={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /Git/ }));

    await waitFor(() => expect(screen.getByText("unstaged.ts")).toBeTruthy());
    fireEvent.click(screen.getByText("unstaged.ts"));
    fireEvent.click(screen.getByText("staged.ts"));
    fireEvent.click(screen.getByText("untracked.ts"));

    expect(onFileSelect).toHaveBeenCalledWith(
      "src/unstaged.ts",
      "git-worktree:unstaged",
    );
    expect(onFileSelect).toHaveBeenCalledWith("src/staged.ts", "git-worktree:staged");
    expect(onFileSelect).toHaveBeenCalledWith(
      "src/untracked.ts",
      "git-worktree:untracked",
    );
  });

  it("reloads the file tree when the active workspace changes", async () => {
    vi.mocked(fsListDir)
      .mockResolvedValueOnce([
        { name: "old-project.ts", kind: "File", path: "old-project.ts" },
      ])
      .mockResolvedValueOnce([
        { name: "new-project.ts", kind: "File", path: "new-project.ts" },
      ]);

    const { rerender } = render(
      <ReviewPanel
        snapshot={makeSnapshot()}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /所有文件/ }));
    expect(await screen.findByText("old-project.ts")).toBeTruthy();

    rerender(
      <ReviewPanel
        snapshot={makeSnapshot({
          workspace: { id: "ws-2", name: "next", root: "/next-repo" },
          session: {
            ...makeSnapshot().session,
            id: "s-2",
            workspace_id: "ws-2",
          },
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    expect(await screen.findByText("new-project.ts")).toBeTruthy();
    expect(screen.queryByText("old-project.ts")).toBeNull();
    expect(fsListDir).toHaveBeenCalledTimes(2);
    expect(fsListDir).toHaveBeenNthCalledWith(1, "");
    expect(fsListDir).toHaveBeenNthCalledWith(2, "");
  });
});
