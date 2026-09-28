import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserViewFrameEvent } from "../../types";
import { BrowserLiveView } from "./BrowserLiveView";
import { browserViewInput, browserViewSetSize, openExternalUrl } from "../../lib/tauri";

const frameHandlers: Array<(event: BrowserViewFrameEvent) => void> = [];

vi.mock("../../lib/events", () => ({
  onBrowserViewFrame: vi.fn((handler: (event: BrowserViewFrameEvent) => void) => {
    frameHandlers.push(handler);
    return Promise.resolve(() => {});
  }),
  onBrowserViewStatus: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    browserViewSetSize: vi.fn().mockResolvedValue(undefined),
    browserViewInput: vi.fn().mockResolvedValue(undefined),
    openExternalUrl: vi.fn().mockResolvedValue(undefined),
  };
});

const mockedInput = vi.mocked(browserViewInput);

describe("BrowserLiveView", () => {
  beforeEach(() => {
    frameHandlers.length = 0;
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      },
    );
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 400,
      height: 300,
      left: 0,
      top: 0,
      right: 400,
      bottom: 300,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  function renderView() {
    return render(
      <BrowserLiveView
        sessionId="session-1"
        targetId="target-1"
        url="https://example.com/docs"
        title="Docs"
      />,
    );
  }

  it("shows the page title and url", () => {
    renderView();

    expect(screen.getByText("Docs")).toBeTruthy();
    expect(screen.getByText("https://example.com/docs")).toBeTruthy();
  });

  it("sizes the page to the panel slot on mount", () => {
    renderView();

    expect(browserViewSetSize).toHaveBeenCalledWith("session-1", "target-1", 400, 300);
  });

  it("forwards pointer presses to the page as CDP mouse events", () => {
    renderView();

    fireEvent.pointerDown(screen.getByRole("application"), {
      button: 0,
      clientX: 12,
      clientY: 34,
      detail: 1,
    });

    expect(mockedInput).toHaveBeenCalledWith(
      "session-1",
      "target-1",
      expect.objectContaining({
        kind: "mouse",
        type: "mousePressed",
        button: "left",
        click_count: 1,
      }),
    );
  });

  it("forwards typing to the page and swallows the DOM key", () => {
    renderView();

    fireEvent.keyDown(screen.getByRole("application"), { key: "a", code: "KeyA" });

    expect(mockedInput).toHaveBeenCalledWith(
      "session-1",
      "target-1",
      expect.objectContaining({
        kind: "key",
        type: "keyDown",
        key: "a",
        code: "KeyA",
        text: "a",
      }),
    );
  });

  it("paints screencast frames of its own page only", () => {
    const { container } = renderView();
    const img = container.querySelector("img") as HTMLImageElement;

    for (const handler of frameHandlers) {
      handler({
        session_id: "session-1",
        target_id: "some-other-page",
        frame: "IGNORED",
        seq: 1,
      });
      handler({
        session_id: "session-1",
        target_id: "target-1",
        frame: "FRAMEBYTES",
        seq: 2,
      });
    }

    expect(img.src).toContain("data:image/jpeg;base64,FRAMEBYTES");
  });

  it("offers the system browser as a fallback for the page", () => {
    renderView();

    fireEvent.click(screen.getByRole("button", { name: "在系统浏览器中打开" }));

    expect(openExternalUrl).toHaveBeenCalledWith("https://example.com/docs");
  });
});
