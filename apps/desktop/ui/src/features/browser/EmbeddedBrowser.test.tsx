import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EmbeddedBrowser } from "./EmbeddedBrowser";
import {
  browserPanelActivate,
  browserPanelBounds,
  browserPanelHide,
  browserPanelHistory,
  browserPanelNavigate,
  browserPanelReload,
} from "../../lib/tauri";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    browserPanelActivate: vi.fn().mockResolvedValue(undefined),
    browserPanelBounds: vi.fn().mockResolvedValue(undefined),
    browserPanelHide: vi.fn().mockResolvedValue(undefined),
    browserPanelNavigate: vi.fn().mockResolvedValue(undefined),
    browserPanelReload: vi.fn().mockResolvedValue(undefined),
    browserPanelHistory: vi.fn().mockResolvedValue(undefined),
  };
});

const mockedActivate = vi.mocked(browserPanelActivate);
const mockedBounds = vi.mocked(browserPanelBounds);
const mockedHide = vi.mocked(browserPanelHide);
const mockedNavigate = vi.mocked(browserPanelNavigate);
const mockedHistory = vi.mocked(browserPanelHistory);
const mockedReload = vi.mocked(browserPanelReload);

/** Run the frame the component schedules its measurement on. */
function paint() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}
describe("EmbeddedBrowser", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      },
    );
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 420,
      height: 800,
      left: 100,
      top: 64,
      right: 520,
      bottom: 864,
      x: 100,
      y: 64,
      toJSON: () => ({}),
    } as DOMRect);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("puts the webview where the slot is and shows the tab", async () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" title="a.dev" />);

    await waitFor(() =>
      expect(mockedBounds).toHaveBeenCalledWith(100, 64, 420, 800),
    );
    expect(mockedActivate).toHaveBeenCalledWith("tab-1");
  });

  it("measures again after the window is resized", async () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    await waitFor(() => expect(mockedBounds).toHaveBeenCalledTimes(1));

    fireEvent(window, new Event("resize"));
    await paint();

    await waitFor(() => expect(mockedBounds).toHaveBeenCalledTimes(2));
  });

  it("shows the page again when the panel comes back", async () => {
    const { rerender } = render(
      <EmbeddedBrowser tabId="tab-1" url="https://a.dev/" layoutSignal="collapsed" />,
    );
    await waitFor(() => expect(mockedBounds).toHaveBeenCalledTimes(1));

    rerender(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" layoutSignal="shown" />);

    await waitFor(() => expect(mockedBounds).toHaveBeenCalledTimes(2));
  });

  it("hides the native webview when the panel shows something else", async () => {
    const { unmount } = render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    await waitFor(() => expect(mockedActivate).toHaveBeenCalled());

    unmount();

    await waitFor(() => expect(mockedHide).toHaveBeenCalled());
  });

  it("follows the tab, not the component, when the panel switches pages", async () => {
    const { rerender } = render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    await waitFor(() => expect(mockedActivate).toHaveBeenCalledWith("tab-1"));

    rerender(<EmbeddedBrowser tabId="tab-2" url="https://b.dev/" />);

    await waitFor(() => expect(mockedActivate).toHaveBeenLastCalledWith("tab-2"));
  });

  it("shows the page's url in the address bar", () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/one" />);

    expect(screen.getByLabelText("网址")).toHaveValue("https://a.dev/one");
  });

  it("navigates where the user typed", async () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    const field = screen.getByLabelText("网址");

    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "example.com/x" } });
    fireEvent.submit(field.closest("form") as HTMLFormElement);

    await waitFor(() =>
      expect(mockedNavigate).toHaveBeenCalledWith("tab-1", "example.com/x"),
    );
  });

  it("does not navigate to nothing", async () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    const field = screen.getByLabelText("网址");

    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "   " } });
    fireEvent.submit(field.closest("form") as HTMLFormElement);

    expect(mockedNavigate).not.toHaveBeenCalled();
  });

  it("reports what the backend said when a navigation is refused", async () => {
    mockedNavigate.mockRejectedValueOnce("这不是一个网址：nope");
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    const field = screen.getByLabelText("网址");

    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "nope" } });
    fireEvent.submit(field.closest("form") as HTMLFormElement);

    expect(await screen.findByRole("alert")).toHaveTextContent("这不是一个网址：nope");
  });

  it("leaves the address bar alone while the user is typing in it", () => {
    const { rerender } = render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);
    const field = screen.getByLabelText("网址");
    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "半" } });

    rerender(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/redirected" />);

    expect(screen.getByLabelText("网址")).toHaveValue("半");
  });

  it("steps through history and reloads the page it is on", async () => {
    render(<EmbeddedBrowser tabId="tab-1" url="https://a.dev/" />);

    fireEvent.click(screen.getByRole("button", { name: "后退" }));
    fireEvent.click(screen.getByRole("button", { name: "前进" }));
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));

    await waitFor(() => {
      expect(mockedHistory).toHaveBeenCalledWith("tab-1", "back");
      expect(mockedHistory).toHaveBeenCalledWith("tab-1", "forward");
      expect(mockedReload).toHaveBeenCalledWith("tab-1");
    });
  });
});
