import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Composer } from "./Composer";
import { editorGetContent, sessionSendPrompt } from "../../lib/tauri";
import type { UiSnapshot } from "../../types";

vi.mock("../../lib/tauri", () => ({
  editorGetContent: vi.fn(),
  sessionCancel: vi.fn(),
  sessionReconnect: vi.fn(),
  sessionSendPrompt: vi.fn(),
  sessionSetConfigControl: vi.fn(),
}));

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
    prompt_capabilities: { image: true, embedded_context: true },
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

describe("Composer", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "Image",
      class Image {
        onerror: (() => void) | null = null;
        set src(_value: string) {
          setTimeout(() => this.onerror?.(), 0);
        }
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it("marks the composer compact when used over expanded review", () => {
    const { container } = render(
      <Composer snapshot={makeSnapshot()} onStateChange={vi.fn()} compact />,
    );

    expect(container.querySelector(".composer")).toHaveClass("is-compact");
  });

  it("renders image attachments as clickable previews without visible file names", async () => {
    const imageUrl = "data:image/png;base64,iVBORw0KGgo=";
    vi.mocked(editorGetContent).mockResolvedValue({
      content: imageUrl,
      kind: "image",
      mime_type: "image/png",
      path: "assets/screenshot.png",
      version: { content_hash: "hash", modified_ms: null, size: 12 },
    });

    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[{ id: "ref-1", path: "assets/screenshot.png" }]}
      />,
    );

    const previewButton = await screen.findByRole("button", {
      name: "预览 screenshot.png",
    });
    expect(screen.queryByText("screenshot.png")).not.toBeInTheDocument();

    fireEvent.click(previewButton);

    const dialog = await screen.findByRole("dialog", { name: "图片预览" });
    await waitFor(() => {
      expect(within(dialog).getByAltText("screenshot.png")).toHaveAttribute("src", imageUrl);
    });
  });

  it("renders workspace file references as mention chips", async () => {
    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[
          {
            id: "ref-1",
            path: "src/features/composer/Composer.tsx",
            startLine: 4,
            endLine: 8,
          },
        ]}
      />,
    );

    expect(await screen.findByText("@src/features/composer/Composer.tsx#L4-L8")).toBeInTheDocument();
    expect(screen.queryByText("REF")).not.toBeInTheDocument();
    expect(editorGetContent).not.toHaveBeenCalled();
  });

  it("sends workspace references as structured mentions without eager file content", async () => {
    vi.mocked(sessionSendPrompt).mockResolvedValue(undefined);

    render(
      <Composer
        snapshot={makeSnapshot()}
        onStateChange={vi.fn()}
        referenceRequests={[
          {
            id: "ref-1",
            path: "src/features/composer/Composer.tsx",
            startLine: 4,
            endLine: 8,
            text: "export function Composer() {}",
          },
        ]}
      />,
    );

    await screen.findByText("@src/features/composer/Composer.tsx#L4-L8");
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "看看这里" } });
    fireEvent.click(screen.getByRole("button", { name: "发送提示" }));

    await waitFor(() => {
      expect(sessionSendPrompt).toHaveBeenCalledWith([
        {
          type: "workspace_file",
          path: "src/features/composer/Composer.tsx",
          start_line: 4,
          end_line: 8,
        },
        { type: "text", text: "看看这里" },
      ]);
    });
    expect(editorGetContent).not.toHaveBeenCalled();
  });
});
