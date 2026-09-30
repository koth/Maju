import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BrowserPanelNewWindowEvent,
  BrowserPanelTab,
  BrowserPanelTabsEvent,
} from "../../types";
import {
  browserPanelClose,
  browserPanelOpen,
  browserPanelState,
} from "../../lib/tauri";
import { onBrowserPanelNewWindow, onBrowserPanelTabs } from "../../lib/events";
import type { ReviewPanelOpenTab } from "../review/ReviewPanel";
import {
  reconcilePanelTabs,
  usePanelBrowserTabs,
  webTab,
} from "./usePanelBrowserTabs";
import { hostOf, pageLabel } from "./browserTabs";

vi.mock("../../lib/events", () => ({
  onBrowserPanelTabs: vi.fn(() => Promise.resolve(() => {})),
  onBrowserPanelUrl: vi.fn(() => Promise.resolve(() => {})),
  onBrowserPanelNewWindow: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    browserPanelOpen: vi.fn().mockResolvedValue({
      tab_id: "tab-1",
      url: "https://a.dev/",
      title: "a.dev",
    }),
    browserPanelClose: vi.fn().mockResolvedValue(undefined),
    browserPanelState: vi.fn().mockResolvedValue({ tabs: [], active: null, endpoint: null }),
  };
});

const mockedOpen = vi.mocked(browserPanelOpen);
const mockedClose = vi.mocked(browserPanelClose);
const mockedState = vi.mocked(browserPanelState);

function panelTab(id: string, url: string, title = ""): BrowserPanelTab {
  return { tab_id: id, url, title };
}

const fileTab: ReviewPanelOpenTab = { kind: "file", path: "src/a.ts" };
const diffTab: ReviewPanelOpenTab = { kind: "diff", path: "src/b.ts", changeSetId: "cs-1" };

describe("reconcilePanelTabs", () => {
  it("appends new panel tabs after non-web tabs", () => {
    const next = reconcilePanelTabs([fileTab], [panelTab("tab-1", "https://a.dev/", "a.dev")]);

    expect(next).toEqual([
      fileTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
    ]);
  });

  it("refreshes kept tabs in place and preserves their order", () => {
    const current: ReviewPanelOpenTab[] = [
      fileTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
      { kind: "web", id: "tab-2", url: "https://b.dev/", title: "b.dev" },
    ];

    const next = reconcilePanelTabs(current, [
      panelTab("tab-2", "https://b.dev/v2", "b.dev"),
      panelTab("tab-1", "https://a.dev/next", "a.dev"),
    ]);

    expect(next).toEqual([
      fileTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/next", title: "a.dev" },
      { kind: "web", id: "tab-2", url: "https://b.dev/v2", title: "b.dev" },
    ]);
  });

  it("drops tabs the browser no longer has", () => {
    const current: ReviewPanelOpenTab[] = [
      diffTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
      { kind: "web", id: "tab-2", url: "https://b.dev/", title: "b.dev" },
    ];

    const next = reconcilePanelTabs(current, [panelTab("tab-2", "https://b.dev/", "b.dev")]);

    expect(next).toEqual([
      diffTab,
      { kind: "web", id: "tab-2", url: "https://b.dev/", title: "b.dev" },
    ]);
  });

  it("never touches non-web tabs", () => {
    expect(reconcilePanelTabs([fileTab, diffTab], [])).toEqual([fileTab, diffTab]);
  });

  it("leaves a file the user opened beside a page beside it", () => {
    // Opening a web link and then a file gives [file, page, diff]; the next
    // event from the browser must not shuffle the file to the front.
    const current: ReviewPanelOpenTab[] = [
      fileTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
      diffTab,
    ];

    const next = reconcilePanelTabs(current, [
      panelTab("tab-1", "https://a.dev/next", "A Dev"),
    ]);

    expect(next).toEqual([
      fileTab,
      { kind: "web", id: "tab-1", url: "https://a.dev/next", title: "A Dev" },
      diffTab,
    ]);
  });

  it("keeps the last known url across a tab that reports an empty one", () => {
    const current: ReviewPanelOpenTab[] = [
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
    ];

    const next = reconcilePanelTabs(current, [panelTab("tab-1", "", "")]);

    expect(next).toEqual([
      { kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" },
    ]);
  });
});

describe("webTab", () => {
  it("labels a page by its host when the browser reports no title", () => {
    expect(webTab(panelTab("tab-1", "https://a.dev/x/y"))).toEqual({
      kind: "web",
      id: "tab-1",
      url: "https://a.dev/x/y",
      title: "a.dev",
    });
  });

  it("falls back to the url itself when it has no host", () => {
    expect(hostOf("about:blank")).toBe("about:blank");
  });

  it("names the empty page the panel opens for the user", () => {
    expect(pageLabel("about:blank", "")).toBe("新标签页");
    expect(pageLabel("https://a.dev/", "a.dev")).toBe("a.dev");
  });
});

describe("usePanelBrowserTabs", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedOpen.mockResolvedValue({ tab_id: "tab-1", url: "https://a.dev/", title: "a.dev" });
    mockedState.mockResolvedValue({ tabs: [], active: null, endpoint: null });
  });

  function mount() {
    const setOpenTabs = vi.fn();
    const setActiveTab = vi.fn();
    const rendered = renderHook(() =>
      usePanelBrowserTabs({ setOpenTabs, setActiveTab }),
    );
    return { ...rendered, setOpenTabs, setActiveTab };
  }

  it("adds a tab and shows it when a link opens a page", async () => {
    const { result, setOpenTabs, setActiveTab } = mount();

    await expect(result.current.openPage("https://a.dev")).resolves.toBe(true);

    expect(mockedOpen).toHaveBeenCalledWith("https://a.dev");
    expect(setOpenTabs).toHaveBeenCalled();
    expect(setActiveTab).toHaveBeenCalledWith({
      kind: "web",
      id: "tab-1",
      url: "https://a.dev/",
      title: "a.dev",
    });
  });

  it("reports a failed open instead of falling back to the system browser", async () => {
    mockedOpen.mockRejectedValueOnce("这不是一个网址");
    const { result } = mount();

    await expect(result.current.openPage("nope")).resolves.toBe(false);
  });

  it("closes the page and leaves the panel on its own tabs", async () => {
    const { result, setActiveTab } = mount();

    result.current.closePage("tab-1");

    await waitFor(() => expect(mockedClose).toHaveBeenCalledWith("tab-1"));
    const update = setActiveTab.mock.calls[0][0] as (tab: unknown) => unknown;
    expect(update({ kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" })).toEqual({
      kind: "base",
      tab: "Review",
    });
  });

  it("keeps showing the page the browser already had", async () => {
    mockedState.mockResolvedValueOnce({
      tabs: [panelTab("tab-2", "https://b.dev/", "b.dev")],
      active: "tab-2",
      endpoint: "http://127.0.0.1:5000",
    });
    const { setOpenTabs, setActiveTab } = mount();

    await waitFor(() => expect(setOpenTabs).toHaveBeenCalled());
    const update = setActiveTab.mock.calls[0][0] as (tab: unknown) => unknown;
    expect(update({ kind: "base", tab: "Review" })).toEqual({
      kind: "web",
      id: "tab-2",
      url: "https://b.dev/",
      title: "b.dev",
    });
  });

  it("stays out of the way when the browser has no tabs", async () => {
    const { setOpenTabs, setActiveTab } = mount();

    await waitFor(() => expect(mockedState).toHaveBeenCalled());
    expect(setOpenTabs).toHaveBeenCalledWith(expect.any(Function));
    expect(setActiveTab).not.toHaveBeenCalled();
  });

  it("opens a page that asked for a window of its own as a tab", async () => {
    let handler: ((event: BrowserPanelNewWindowEvent) => void) | undefined;
    vi.mocked(onBrowserPanelNewWindow).mockImplementationOnce((callback) => {
      handler = callback;
      return Promise.resolve(() => {});
    });
    const { result } = mount();

    await waitFor(() => expect(handler).toBeDefined());
    handler?.({ tab_id: "tab-1", url: "https://b.dev/" });

    await waitFor(() =>
      expect(mockedOpen).toHaveBeenCalledWith("https://b.dev/"),
    );
    expect(result.current.openPage).toBeDefined();
  });

  it("labels a tab with the title the browser reported", () => {
    const next = reconcilePanelTabs(
      [{ kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" }],
      [panelTab("tab-1", "https://a.dev/", "A Dev — Home")],
    );

    expect(next).toEqual([
      {
        kind: "web",
        id: "tab-1",
        url: "https://a.dev/",
        title: "A Dev — Home",
      },
    ]);
  });

  it("renames the page being shown when the browser reports its title", async () => {
    let handler: ((event: BrowserPanelTabsEvent) => void) | undefined;
    vi.mocked(onBrowserPanelTabs).mockImplementationOnce((callback) => {
      handler = callback;
      return Promise.resolve(() => {});
    });
    const { setActiveTab } = mount();

    await waitFor(() => expect(handler).toBeDefined());
    handler?.({
      tabs: [panelTab("tab-1", "https://a.dev/", "A Dev — Home")],
      active: "tab-1",
    });

    // The page was labelled by its host while it loaded; the document's own
    // title has to replace it in the tab the user is looking at, not only in
    // the strip.
    const update = setActiveTab.mock.calls[0][0] as (tab: unknown) => unknown;
    expect(
      update({ kind: "web", id: "tab-1", url: "https://a.dev/", title: "a.dev" }),
    ).toEqual({
      kind: "web",
      id: "tab-1",
      url: "https://a.dev/",
      title: "A Dev — Home",
    });
  });
});
