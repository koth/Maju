import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { buildLineDiffRows } from "./ReviewPanel";
import { ReviewPanel } from "./ReviewPanel";
import type { ReviewPanelActiveTab, ReviewPanelOpenTab, ReviewPreferredChangeSet } from "./ReviewPanel";
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

vi.mock("../editor/DiffTab", () => ({
  DiffTab: ({
    change,
    fileTreeVisible,
    onToggleFileTree,
  }: {
    change: FileChangeRecord;
    fileTreeVisible?: boolean;
    onToggleFileTree?: () => void;
  }) => (
    <div>
      <div>diff tab: {change.path}</div>
      {onToggleFileTree && (
        <button
          type="button"
          aria-label={fileTreeVisible ? "隐藏 Git 文件树" : "显示 Git 文件树"}
          onClick={onToggleFileTree}
        >
          tree
        </button>
      )}
    </div>
  ),
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
  it("toggles the expanded panel control from the tab bar", () => {
    const onPanelExpandedChange = vi.fn();
    const baseProps = {
      snapshot: makeSnapshot(),
      refreshing: false,
      hydrated: true,
      onRefresh: () => {},
      onFileSelect: () => {},
      onFileOpen: () => {},
      onPanelExpandedChange,
    };
    const { rerender } = render(<ReviewPanel {...baseProps} panelExpanded={false} />);

    const expandButton = screen.getByRole("button", { name: "展开审查面板" });
    expect(expandButton).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(expandButton);
    expect(onPanelExpandedChange).toHaveBeenCalledWith(true);

    rerender(<ReviewPanel {...baseProps} panelExpanded />);
    const restoreButton = screen.getByRole("button", { name: "还原审查面板" });
    expect(restoreButton).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(restoreButton);
    expect(onPanelExpandedChange).toHaveBeenLastCalledWith(false);
  });

  it("does not force review content to the bottom when the panel is expanded", async () => {
    const scrollHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollHeight",
    );
    const clientHeightDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "clientHeight",
    );
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        return this instanceof HTMLElement && this.classList.contains("review-session-list")
          ? 640
          : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get() {
        return this instanceof HTMLElement && this.classList.contains("review-session-list")
          ? 180
          : 0;
      },
    });

    try {
      vi.mocked(sessionListChangeSets).mockResolvedValue([
        makeChangeSet("turn-1", "AgentTurn", "2026-05-12T03:00:00Z"),
      ]);
      vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
        change_set_id: "turn-1",
        files: [makeSummary("turn-1", "src/turn.ts")],
      });

      render(
        <ReviewPanel
          snapshot={makeSnapshot({
            messages: [{ id: "turn-1-message", role: "Assistant", body: "done" }],
            timeline: [{ Message: "turn-1-message" }],
          })}
          refreshing={false}
          hydrated
          panelExpanded
          onRefresh={() => {}}
          onFileSelect={() => {}}
          onFileOpen={() => {}}
        />,
      );

      const list = await waitFor(() => {
        const node = document.querySelector<HTMLElement>(".review-session-list");
        if (!node) throw new Error("review list not mounted");
        return node;
      });
      await new Promise((resolve) => window.setTimeout(resolve, 420));
      expect(list.scrollTop).toBe(0);
    } finally {
      if (scrollHeightDescriptor) {
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", scrollHeightDescriptor);
      } else {
        delete (HTMLElement.prototype as unknown as { scrollHeight?: number }).scrollHeight;
      }
      if (clientHeightDescriptor) {
        Object.defineProperty(HTMLElement.prototype, "clientHeight", clientHeightDescriptor);
      } else {
        delete (HTMLElement.prototype as unknown as { clientHeight?: number }).clientHeight;
      }
    }
  });

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
    expect(screen.getByRole("button", { name: "src/manual.ts" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });

  it("toggles a review file diff by clicking the file header row", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("turn-1", "AgentTurn", "2026-05-12T03:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "turn-1",
      files: [makeSummary("turn-1", "src/turn.ts")],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [{ id: "turn-1-message", role: "Assistant", body: "done" }],
          timeline: [{ Message: "turn-1-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    const header = await screen.findByRole("button", { name: "src/turn.ts" });
    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByLabelText("src/turn.ts 差异预览")).toBeTruthy();

    fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByLabelText("src/turn.ts 差异预览")).toBeNull();
  });

  it("scrolls inline diffs horizontally with arrow keys without blocking pointer selection", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      makeChangeSet("turn-1", "AgentTurn", "2026-05-12T03:00:00Z"),
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "turn-1",
      files: [makeSummary("turn-1", "src/turn.ts")],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [{ id: "turn-1-message", role: "Assistant", body: "done" }],
          timeline: [{ Message: "turn-1-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    const diff = await screen.findByLabelText("src/turn.ts 差异预览");
    const code = document.createElement("div");
    code.setAttribute("data-code", "");
    Object.defineProperty(code, "clientWidth", { configurable: true, value: 120 });
    Object.defineProperty(code, "scrollWidth", { configurable: true, value: 360 });
    diff.appendChild(code);
    diff.scrollLeft = 0;
    code.scrollLeft = 0;

    fireEvent.mouseEnter(diff);
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(diff.scrollLeft).toBe(0);
    expect(code.scrollLeft).toBe(80);

    fireEvent.keyDown(diff, { key: "ArrowRight" });
    expect(diff.scrollLeft).toBe(0);
    expect(code.scrollLeft).toBe(160);

    fireEvent.keyDown(diff, { key: "ArrowLeft" });
    expect(diff.scrollLeft).toBe(0);
    expect(code.scrollLeft).toBe(80);

    expect(fireEvent.pointerDown(diff, { pointerId: 1, button: 0, clientX: 160 })).toBe(true);
    expect(fireEvent.pointerMove(diff, { pointerId: 1, clientX: 100 })).toBe(true);
  });

  it("shows newest review files first and auto-expands recent files within budget", async () => {
    vi.mocked(sessionListChangeSets).mockResolvedValue([
      {
        ...makeChangeSet("turn-1", "AgentTurn", "2026-05-12T03:00:00Z"),
        file_count: 4,
      },
    ]);
    vi.mocked(sessionListChangeSetFiles).mockResolvedValue({
      change_set_id: "turn-1",
      files: [
        makeSummary("turn-1", "src/a.ts", "2026-05-12T01:00:00Z"),
        makeSummary("turn-1", "src/z.ts", "2026-05-12T03:00:00Z"),
        {
          ...makeSummary("turn-1", "src/huge.ts", "2026-05-12T02:30:00Z"),
          added_lines: 800,
        },
        makeSummary("turn-1", "src/m.ts", "2026-05-12T02:00:00Z"),
      ],
    });

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [{ id: "turn-1-message", role: "Assistant", body: "done" }],
          timeline: [{ Message: "turn-1-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    const headers = await screen.findAllByRole("button", { name: /^src\/.*\.ts$/ });
    expect(headers.map((header) => header.getAttribute("aria-label"))).toEqual([
      "src/z.ts",
      "src/huge.ts",
      "src/m.ts",
      "src/a.ts",
    ]);
    expect(headers[0]).toHaveAttribute("aria-expanded", "true");
    expect(headers[1]).toHaveAttribute("aria-expanded", "false");
    expect(headers[2]).toHaveAttribute("aria-expanded", "true");
    expect(headers[3]).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByLabelText("src/z.ts 差异预览")).toBeTruthy();
    expect(screen.queryByLabelText("src/huge.ts 差异预览")).toBeNull();
    expect(screen.getByLabelText("src/m.ts 差异预览")).toBeTruthy();
    expect(screen.getByLabelText("src/a.ts 差异预览")).toBeTruthy();
    await waitFor(() =>
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "turn-1",
        path: "src/z.ts",
      }),
    );
    await waitFor(() => {
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "turn-1",
        path: "src/m.ts",
      });
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "turn-1",
        path: "src/a.ts",
      });
    });
    expect(sessionGetChangeSetFileDiff).not.toHaveBeenCalledWith({
      change_set_id: "turn-1",
      path: "src/huge.ts",
    });
  });

  it("falls back to the latest agent turn with files when the latest assistant turn has no files", async () => {
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

    await waitFor(() =>
      expect(screen.queryAllByText("src/old.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("上轮对话暂无文件变化")).toBeNull();
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

  it("shows the nearest changed turn while the current active turn follows a no-change assistant turn", async () => {
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

    await waitFor(() =>
      expect(screen.queryAllByText("src/old.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("上轮对话暂无文件变化")).toBeNull();
  });

  it("releases a focused historical change set when newer agent changes arrive", async () => {
    const oldTurn = makeChangeSet("old-turn", "AgentTurn", "2026-05-12T03:00:00Z");
    const newTurn = makeChangeSet("new-turn", "AgentTurn", "2026-05-12T04:00:00Z");
    const newerTurn = makeChangeSet("new-turn", "AgentTurn", "2026-05-12T04:01:00Z");

    vi.mocked(sessionListChangeSets)
      .mockResolvedValueOnce([oldTurn, newTurn])
      .mockResolvedValueOnce([oldTurn, newerTurn]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "old-turn"
          ? [makeSummary("old-turn", "src/old.ts")]
          : [makeSummary("new-turn", "src/new.ts")],
    }));

    const { rerender } = render(
      <ReviewPanel
        snapshot={makeSnapshot({
          messages: [
            { id: "old-turn-message", role: "Assistant", body: "old edit" },
            { id: "new-turn-message", role: "Assistant", body: "new edit" },
          ],
          timeline: [{ Message: "old-turn-message" }, { Message: "new-turn-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        focusRequest={{ changeSetId: "old-turn", token: 1 }}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/old.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/new.ts")).toHaveLength(0);

    rerender(
      <ReviewPanel
        snapshot={makeSnapshot({
          revision: 2,
          messages: [
            { id: "old-turn-message", role: "Assistant", body: "old edit" },
            { id: "new-turn-message", role: "Assistant", body: "new edit" },
          ],
          timeline: [{ Message: "old-turn-message" }, { Message: "new-turn-message" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        focusRequest={{ changeSetId: "old-turn", token: 1 }}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/new.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
  });

  it("refreshes scoped change sets when focus moves to a new turn", async () => {
    const oldTurn = makeChangeSet("old-turn", "AgentTurn", "2026-05-12T04:00:00Z");
    const newTurn = makeChangeSet("new-turn", "AgentTurn", "2026-05-12T04:00:00Z");

    vi.mocked(sessionListChangeSets)
      .mockResolvedValueOnce([oldTurn])
      .mockResolvedValueOnce([oldTurn, newTurn]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "old-turn"
          ? [makeSummary("old-turn", "src/old.ts")]
          : [makeSummary("new-turn", "src/new.ts")],
    }));

    const snapshot = makeSnapshot({
      messages: [
        { id: "old-turn-message", role: "Assistant", body: "old edit" },
        { id: "new-turn-message", role: "Assistant", body: "new edit" },
      ],
      timeline: [{ Message: "old-turn-message" }, { Message: "new-turn-message" }],
    });

    const { rerender } = render(
      <ReviewPanel
        snapshot={snapshot}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        focusRequest={{ changeSetId: "old-turn", token: 1 }}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/old.ts").length).toBeGreaterThan(0),
    );

    rerender(
      <ReviewPanel
        snapshot={snapshot}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        focusRequest={{ changeSetId: "new-turn", token: 2 }}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/new.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
  });

  it("prioritizes pending agent changes over a focused historical change set", async () => {
    const oldTurn = makeChangeSet("old-turn", "AgentTurn", "2026-05-12T03:00:00Z");
    const pendingTurn: ChangeSetSummary = {
      ...makeChangeSet("pending-turn", "AgentTurn", "2026-05-12T04:00:00Z"),
      message_id: null,
      owner_key: "user-message:current-user",
      status: "Pending",
    };

    vi.mocked(sessionListChangeSets).mockResolvedValue([oldTurn, pendingTurn]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "old-turn"
          ? [makeSummary("old-turn", "src/old.ts")]
          : [makeSummary("pending-turn", "src/pending.ts")],
    }));

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          session: {
            ...makeSnapshot().session,
            status: "Streaming",
          },
          messages: [
            { id: "old-turn-message", role: "Assistant", body: "old edit" },
            { id: "current-user", role: "User", body: "new request" },
          ],
          timeline: [{ Message: "old-turn-message" }, { Message: "current-user" }],
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        focusRequest={{ changeSetId: "old-turn", token: 1 }}
      />,
    );

    await waitFor(() =>
      expect(screen.queryAllByText("src/pending.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
  });

  it("ignores stale pending agent changes from a previous user turn", async () => {
    const previousTurn = makeChangeSet(
      "previous-turn",
      "AgentTurn",
      "2026-05-12T03:00:00Z",
    );
    const stalePendingTurn: ChangeSetSummary = {
      ...makeChangeSet("stale-pending-turn", "AgentTurn", "2026-05-12T04:00:00Z"),
      message_id: null,
      owner_key: "user-message:previous-user",
      status: "Pending",
    };

    vi.mocked(sessionListChangeSets).mockResolvedValue([previousTurn, stalePendingTurn]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "stale-pending-turn"
          ? [makeSummary("stale-pending-turn", "src/stale.ts")]
          : [makeSummary("previous-turn", "src/previous.ts")],
    }));

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          session: {
            ...makeSnapshot().session,
            status: "Streaming",
          },
          messages: [
            { id: "previous-turn-message", role: "Assistant", body: "old edit" },
            { id: "current-user", role: "User", body: "new request" },
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
    expect(screen.queryAllByText("src/stale.ts")).toHaveLength(0);
  });

  it("opens Git rows as review diff tabs", async () => {
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

    expect(onFileSelect).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "git-worktree:unstaged",
        path: "src/unstaged.ts",
      }),
    );
    expect(screen.getByRole("button", { name: "打开差异 unstaged.ts" })).toBeTruthy();
    expect(screen.getByText("diff tab: src/unstaged.ts")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Git$/ })).toBeNull();
    expect(screen.getByLabelText("Git 文件树")).toBeTruthy();
    expect(screen.getByRole("button", { name: "隐藏 Git 文件树" })).toBeTruthy();
    expect(screen.getAllByText("staged.ts").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "隐藏 Git 文件树" }));
    expect(screen.queryByLabelText("Git 文件树")).toBeNull();
    expect(screen.getByRole("button", { name: "显示 Git 文件树" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "显示 Git 文件树" }));
    expect(screen.getByLabelText("Git 文件树")).toBeTruthy();
    expect(screen.getByRole("button", { name: "隐藏 Git 文件树" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "关闭 unstaged.ts" }));
    expect(screen.queryByText("diff tab: src/unstaged.ts")).toBeNull();
    expect(screen.getByRole("button", { name: /^Git$/ })).toHaveClass("review-tab-active");
  });

  it("opens untracked Git rows as review diff tabs", async () => {
    const onFileSelect = vi.fn();

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          repository: {
            branch: "main",
            head: "abc",
            changed_files: [
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
    fireEvent.click(await screen.findByText("untracked.ts"));

    expect(onFileSelect).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(sessionGetChangeSetFileDiff).toHaveBeenCalledWith({
        change_set_id: "git-worktree:untracked",
        path: "src/untracked.ts",
      }),
    );
  });

  it("sends files to context from right-side file tree context menus", async () => {
    const onAddComposerReference = vi.fn();
    vi.mocked(fsListDir).mockResolvedValue([
      { name: "app.ts", kind: "File", path: "src/app.ts" },
    ]);

    render(
      <ReviewPanel
        snapshot={makeSnapshot({
          prompt_capabilities: { image: false, embedded_context: true, session_steer: false },
          repository: {
            branch: "main",
            head: "abc",
            changed_files: [
              makeChangedFile("src/unstaged.ts", "Unstaged"),
              makeChangedFile("src/untracked.ts", "Untracked"),
            ],
          },
        })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
        onAddComposerReference={onAddComposerReference}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /所有文件/ }));
    fireEvent.contextMenu(await screen.findByText("app.ts"), {
      clientX: 14,
      clientY: 18,
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "发送到上下文" }));
    expect(onAddComposerReference).toHaveBeenCalledWith("src/app.ts");

    fireEvent.click(screen.getByRole("button", { name: /Git/ }));
    fireEvent.contextMenu(await screen.findByText("unstaged.ts"), {
      clientX: 18,
      clientY: 22,
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "发送到上下文" }));
    expect(onAddComposerReference).toHaveBeenCalledWith("src/unstaged.ts");

    fireEvent.contextMenu(screen.getByText("untracked.ts"), {
      clientX: 20,
      clientY: 24,
    });
    expect(screen.getByRole("menuitem", { name: "跟踪文件" })).toBeTruthy();
    fireEvent.click(screen.getByRole("menuitem", { name: "发送到上下文" }));
    expect(onAddComposerReference).toHaveBeenCalledWith("src/untracked.ts");
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

  it("does not list files for disconnected remote workspace snapshots", async () => {
    render(
      <ReviewPanel
        snapshot={makeSnapshot({ workspace_connected: false })}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /所有文件/ }));

    expect(screen.getByText("远程工作区未连接")).toBeTruthy();
    expect(fsListDir).not.toHaveBeenCalled();
  });

  it("opens file tree selections as review tabs when a file renderer is provided", async () => {
    const onFileOpen = vi.fn();
    vi.mocked(fsListDir).mockResolvedValue([
      { name: "app.ts", kind: "File", path: "src/app.ts" },
    ]);

    render(
      <ReviewPanel
        snapshot={makeSnapshot()}
        refreshing={false}
        hydrated
        onRefresh={() => {}}
        onFileSelect={() => {}}
        onFileOpen={onFileOpen}
        renderFileTab={(path, context) => (
          <div>
            <div>review editor: {path}</div>
            {context.onToggleFileTree && (
              <button
                type="button"
                aria-label={context.fileTreeVisible ? "隐藏文件树" : "显示文件树"}
                onClick={context.onToggleFileTree}
              >
                tree
              </button>
            )}
          </div>
        )}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /所有文件/ }));
    fireEvent.click(await screen.findByText("app.ts"));

    expect(onFileOpen).not.toHaveBeenCalled();
    expect(screen.getByText("review editor: src/app.ts")).toBeTruthy();
    expect(screen.getByRole("button", { name: "打开文件 app.ts" })).toBeTruthy();
    expect(screen.getByLabelText("文件树")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "隐藏文件树" }));
    expect(screen.queryByLabelText("文件树")).toBeNull();
    expect(screen.getByRole("button", { name: "显示文件树" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "显示文件树" }));
    expect(screen.getByLabelText("文件树")).toBeTruthy();
    expect(screen.getByRole("button", { name: "隐藏文件树" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "关闭 app.ts" }));
    expect(screen.queryByText("review editor: src/app.ts")).toBeNull();
  });

  it("preserves open and active review tabs when expanding remounts the panel", async () => {
    vi.mocked(fsListDir).mockResolvedValue([
      { name: "app.ts", kind: "File", path: "src/app.ts" },
    ]);

    function ControlledReviewPanel() {
      const [expanded, setExpanded] = useState(false);
      const [activeTab, setActiveTab] = useState<ReviewPanelActiveTab>({
        kind: "base",
        tab: "Review",
      });
      const [openTabs, setOpenTabs] = useState<ReviewPanelOpenTab[]>([]);
      const panel = (
        <ReviewPanel
          snapshot={makeSnapshot()}
          refreshing={false}
          hydrated
          panelExpanded={expanded}
          onRefresh={() => {}}
          onFileSelect={() => {}}
          onFileOpen={() => {}}
          onPanelExpandedChange={setExpanded}
          renderFileTab={(path) => <div>review editor: {path}</div>}
          activeTab={activeTab}
          openTabs={openTabs}
          onActiveTabChange={setActiveTab}
          onOpenTabsChange={setOpenTabs}
          focusRequest={{ changeSetId: "turn-1", token: 1 }}
        />
      );

      return expanded ? (
        <section aria-label="expanded placement">{panel}</section>
      ) : (
        <aside aria-label="side placement">{panel}</aside>
      );
    }

    render(<ControlledReviewPanel />);

    fireEvent.click(screen.getByRole("button", { name: /所有文件/ }));
    fireEvent.click(await screen.findByText("app.ts"));
    expect(screen.getByText("review editor: src/app.ts")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "展开审查面板" }));

    expect(screen.getByText("review editor: src/app.ts")).toBeTruthy();
    expect(screen.getByRole("button", { name: "还原审查面板" })).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "打开文件 app.ts" }).closest(".review-open-file-tab"),
    ).toHaveClass("review-tab-active");
  });

  it("does not revive a released historical focus when expanding remounts the panel", async () => {
    const oldTurn = makeChangeSet("old-turn", "AgentTurn", "2026-05-12T03:00:00Z");
    const newTurn = makeChangeSet("new-turn", "AgentTurn", "2026-05-12T04:00:00Z");
    const newerTurn = makeChangeSet("new-turn", "AgentTurn", "2026-05-12T04:01:00Z");

    vi.mocked(sessionListChangeSets)
      .mockResolvedValueOnce([oldTurn, newTurn])
      .mockResolvedValue([oldTurn, newerTurn]);
    vi.mocked(sessionListChangeSetFiles).mockImplementation(async ({ change_set_id }) => ({
      change_set_id,
      files:
        change_set_id === "old-turn"
          ? [makeSummary("old-turn", "src/old.ts")]
          : [makeSummary("new-turn", "src/new.ts")],
    }));

    function ControlledReviewPanel() {
      const [expanded, setExpanded] = useState(false);
      const [revision, setRevision] = useState(1);
      const [activeTab, setActiveTab] = useState<ReviewPanelActiveTab>({
        kind: "base",
        tab: "Review",
      });
      const [openTabs, setOpenTabs] = useState<ReviewPanelOpenTab[]>([]);
      const [preferredChangeSet, setPreferredChangeSet] =
        useState<ReviewPreferredChangeSet | null>({
          id: "old-turn",
          token: 1,
          consumedSignature: null,
        });
      const panel = (
        <ReviewPanel
          snapshot={makeSnapshot({
            revision,
            messages: [
              { id: "old-turn-message", role: "Assistant", body: "old edit" },
              { id: "new-turn-message", role: "Assistant", body: "new edit" },
            ],
            timeline: [{ Message: "old-turn-message" }, { Message: "new-turn-message" }],
          })}
          refreshing={false}
          hydrated
          panelExpanded={expanded}
          onRefresh={() => {}}
          onFileSelect={() => {}}
          onFileOpen={() => {}}
          onPanelExpandedChange={setExpanded}
          activeTab={activeTab}
          openTabs={openTabs}
          onActiveTabChange={setActiveTab}
          onOpenTabsChange={setOpenTabs}
          focusRequest={{ changeSetId: "old-turn", token: 1 }}
          preferredChangeSet={preferredChangeSet}
          onPreferredChangeSetChange={setPreferredChangeSet}
        />
      );

      return (
        <>
          <button type="button" onClick={() => setRevision(2)}>
            simulate newer changes
          </button>
          {expanded ? (
            <section aria-label="expanded placement">{panel}</section>
          ) : (
            <aside aria-label="side placement">{panel}</aside>
          )}
        </>
      );
    }

    render(<ControlledReviewPanel />);

    await waitFor(() =>
      expect(screen.queryAllByText("src/old.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/new.ts")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "simulate newer changes" }));
    await waitFor(() =>
      expect(screen.queryAllByText("src/new.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "展开审查面板" }));

    await waitFor(() =>
      expect(screen.queryAllByText("src/new.ts").length).toBeGreaterThan(0),
    );
    expect(screen.queryAllByText("src/old.ts")).toHaveLength(0);
    expect(screen.getByRole("button", { name: "还原审查面板" })).toBeTruthy();
  });
});
