import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { cleanup, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { BrowserPanel, derivePhase, withheldHeadline } from "./BrowserPanel";
import { onCapabilityState } from "../../lib/events";
import type { BrowserSessionState, CapabilityStateEvent } from "../../types";

vi.mock("../../lib/events", () => ({
  onCapabilityState: vi.fn(),
}));

vi.mock("../../lib/tauri", () => ({
  browserNavigate: vi.fn().mockResolvedValue(undefined),
  browserRefresh: vi.fn().mockResolvedValue(undefined),
  browserClose: vi.fn().mockResolvedValue(undefined),
}));

function state(overrides: Partial<BrowserSessionState> = {}): BrowserSessionState {
  return {
    session_id: "session-1",
    status: "active",
    mode: "launch",
    current_url: "https://example.test/docs",
    page_title: "Docs",
    panel_rendition: null,
    latest_screenshot: null,
    version: 1,
    ...overrides,
  };
}

describe("derivePhase", () => {
  it("treats a missing state as idle rather than broken", () => {
    expect(derivePhase(null)).toEqual({ kind: "idle" });
  });

  it("maps an active resource to live", () => {
    expect(derivePhase(state())).toEqual({ kind: "live" });
  });

  it("distinguishes starting from idle so the panel can explain the delay", () => {
    // The browser starts on the agent's first tool call, not at session
    // creation, so the user needs to be told that rather than shown an error.
    expect(derivePhase(state({ status: "idle" }))).toEqual({ kind: "starting" });
  });

  it("maps closing and closed to the same closed phase", () => {
    expect(derivePhase(state({ status: "closing" }))).toEqual({ kind: "closed" });
    expect(derivePhase(state({ status: "closed" }))).toEqual({ kind: "closed" });
  });

  it("maps failure to a failed phase carrying the reason", () => {
    const phase = derivePhase(state({ status: "failed" }));
    expect(phase.kind).toBe("failed");
  });

  it("prefers a withheld reason over any state", () => {
    // The capability being off outranks a stale state object: showing a live
    // panel for a feature the user disabled would be worse than showing why.
    const phase = derivePhase(state(), "disabled", "Browser tools are off");
    expect(phase).toEqual({
      kind: "unavailable",
      reason: "disabled",
      detail: "Browser tools are off",
    });
  });
});

describe("withheldHeadline", () => {
  it("gives every reason a distinct headline", () => {
    const reasons = [
      "disabled",
      "remote-workspace",
      "unsupported-agent",
      "unavailable",
    ] as const;
    const headlines = reasons.map(withheldHeadline);
    expect(new Set(headlines).size).toBe(reasons.length);
    expect(headlines.every((headline) => headline.length > 0)).toBe(true);
  });
});

describe("BrowserPanel", () => {
  let emit: (event: CapabilityStateEvent) => void;
  let unlisten: () => void;

  beforeEach(() => {
    unlisten = vi.fn(() => {});
    vi.mocked(onCapabilityState).mockImplementation((callback) => {
      emit = callback;
      return Promise.resolve(unlisten);
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the latest rendition when one exists", () => {
    render(
      <BrowserPanel
        onClose={vi.fn()}
        state={state({ panel_rendition: "aVZCT1I=" })}
      />,
    );

    const image = screen.getByRole("img");
    expect(image.getAttribute("src")).toBe("data:image/png;base64,aVZCT1I=");
  });

  it("explains an empty rendition instead of showing a blank surface", () => {
    render(<BrowserPanel onClose={vi.fn()} state={state()} />);
    expect(
      screen.getByText(/Waiting for the first page/i),
    ).toBeTruthy();
  });

  it("shows the withheld reason and its detail", () => {
    render(
      <BrowserPanel
        onClose={vi.fn()}
        state={null}
        withheldDetail="Run `npx playwright install chromium` once."
        withheldReason="unavailable"
      />,
    );

    expect(screen.getByText("Browser tools are not ready")).toBeTruthy();
    expect(screen.getByText(/npx playwright install/)).toBeTruthy();
  });

  it("disables the address bar until the browser is live", () => {
    const { rerender } = render(
      <BrowserPanel onClose={vi.fn()} state={state({ status: "idle" })} />,
    );
    expect(screen.getByLabelText("Address").hasAttribute("disabled")).toBe(true);

    rerender(<BrowserPanel onClose={vi.fn()} state={state()} />);
    expect(screen.getByLabelText("Address").hasAttribute("disabled")).toBe(false);
  });

  it("does not overwrite what the user is typing", async () => {
    render(<BrowserPanel onClose={vi.fn()} state={state()} />);
    const input = screen.getByLabelText("Address") as HTMLInputElement;

    fireEvent.change(input, { target: { value: "https://typed.test" } });
    // A state event arrives while the field is mid-edit.
    emit({
      kind: "browser",
      state: state({ current_url: "https://elsewhere.test", version: 2 }),
    });

    await waitFor(() => {
      expect((screen.getByLabelText("Address") as HTMLInputElement).value).toBe(
        "https://typed.test",
      );
    });
  });

  it("ignores an event older than the state it already shows", async () => {
    render(<BrowserPanel onClose={vi.fn()} state={state({ version: 5 })} />);

    emit({
      kind: "browser",
      state: state({ current_url: "https://stale.test", version: 2 }),
    });

    await waitFor(() => {
      expect(
        (screen.getByLabelText("Address") as HTMLInputElement).value,
      ).toContain("example.test");
    });
  });

  it("clears the panel when the session's browser closes", async () => {
    render(
      <BrowserPanel onClose={vi.fn()} state={state({ panel_rendition: "aVZCT1I=" })} />,
    );
    expect(screen.getByRole("img")).toBeTruthy();

    emit({ kind: "browserClosed", session_id: "session-1" });

    await waitFor(() => {
      expect(screen.queryByRole("img")).toBeNull();
    });
  });

  it("ignores a close event for a different session", async () => {
    render(
      <BrowserPanel onClose={vi.fn()} state={state({ panel_rendition: "aVZCT1I=" })} />,
    );

    emit({ kind: "browserClosed", session_id: "session-2" });

    await waitFor(() => {
      expect(screen.getByRole("img")).toBeTruthy();
    });
  });

  it("unsubscribes on unmount", async () => {
    const { unmount } = render(<BrowserPanel onClose={vi.fn()} state={state()} />);
    await waitFor(() => expect(onCapabilityState).toHaveBeenCalled());
    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalled());
  });

  it("calls the dismiss handler", () => {
    const onClose = vi.fn();
    render(<BrowserPanel onClose={onClose} state={state()} />);
    fireEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(onClose).toHaveBeenCalled();
  });
});
