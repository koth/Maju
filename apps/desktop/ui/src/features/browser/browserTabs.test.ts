import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { openExternalUrl } from "../../lib/tauri";
import {
  hasPanelOpener,
  openLinkInPanelOrExternal,
  registerPanelOpener,
} from "./browserTabs";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    openExternalUrl: vi.fn(),
  };
});

const mockedOpenExternal = vi.mocked(openExternalUrl);

describe("openLinkInPanelOrExternal", () => {
  beforeEach(() => {
    mockedOpenExternal.mockResolvedValue(undefined);
  });

  afterEach(() => {
    registerPanelOpener(null);
    vi.clearAllMocks();
  });

  it("routes http links to the right-panel browser when an opener is registered", async () => {
    const opener = vi.fn(async () => true);
    registerPanelOpener(opener);

    await openLinkInPanelOrExternal("https://example.com/docs");

    expect(opener).toHaveBeenCalledWith("https://example.com/docs");
    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("falls back to the system browser when the panel refuses the link", async () => {
    registerPanelOpener(async () => false);

    await openLinkInPanelOrExternal("https://example.com/docs");

    expect(mockedOpenExternal).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("falls back to the system browser when the panel open throws", async () => {
    registerPanelOpener(async () => {
      throw new Error("browser tools are not available");
    });

    await openLinkInPanelOrExternal("https://example.com/docs");

    expect(mockedOpenExternal).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("opens externally when no panel opener is registered", async () => {
    expect(hasPanelOpener()).toBe(false);

    await openLinkInPanelOrExternal("https://example.com/docs");

    expect(mockedOpenExternal).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("sends non-http links straight to the system handler", async () => {
    const opener = vi.fn(async () => true);
    registerPanelOpener(opener);

    await openLinkInPanelOrExternal("mailto:hi@example.com");

    expect(opener).not.toHaveBeenCalled();
    expect(mockedOpenExternal).toHaveBeenCalledWith("mailto:hi@example.com");
  });

  it("ignores empty hrefs", async () => {
    await openLinkInPanelOrExternal("   ");

    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("does not throw when the system handler is unavailable too", async () => {
    mockedOpenExternal.mockRejectedValue(new Error("no handler"));

    await expect(openLinkInPanelOrExternal("https://example.com")).resolves.toBeUndefined();
  });
});
