import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { openExternalUrl } from "../../lib/tauri";
import {
  hasPanelOpener,
  openLinkInPanelOrExternal,
  registerPanelOpener,
  trimLinkPunctuation,
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

  it("keeps the click in the panel when the panel refuses the link", async () => {
    registerPanelOpener(async () => false);

    await openLinkInPanelOrExternal("https://example.com/docs");

    // A wired-up panel is the destination: the external window is what the
    // panel replaced, so a refusal must not fall back to it.
    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("keeps the click in the panel when the panel open throws", async () => {
    registerPanelOpener(async () => {
      throw new Error("browser tools are not available");
    });

    await openLinkInPanelOrExternal("https://example.com/docs");

    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("never opens a web link in the system browser, panel or not", async () => {
    expect(hasPanelOpener()).toBe(false);

    await openLinkInPanelOrExternal("https://example.com/docs");

    // With no panel the click is reported, not handed to another browser
    // window: two browsers on screen is the complaint the panel answers.
    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("sends non-http links straight to the system handler", async () => {
    const opener = vi.fn(async () => true);
    registerPanelOpener(opener);

    await openLinkInPanelOrExternal("mailto:hi@example.com");

    expect(opener).not.toHaveBeenCalled();
    expect(mockedOpenExternal).toHaveBeenCalledWith("mailto:hi@example.com", "link-scheme");
  });

  it("ignores empty hrefs", async () => {
    await openLinkInPanelOrExternal("   ");

    expect(mockedOpenExternal).not.toHaveBeenCalled();
  });

  it("does not throw when the system handler is unavailable too", async () => {
    mockedOpenExternal.mockRejectedValue(new Error("no handler"));

    await expect(openLinkInPanelOrExternal("mailto:hi@example.com")).resolves.toBeUndefined();
  });

  it("hands the panel the URL without the prose that followed it", async () => {
    const opener = vi.fn(async () => true);
    registerPanelOpener(opener);

    await openLinkInPanelOrExternal(
      "https://www.baidu.com/%EF%BC%88%E9%A1%B5%E9%9D%A2%E6%A0%87%E9%A2%98%EF%BC%9A%E7%99%BE%E5%BA%A6%E4%B8%80%E4%B8%8B%EF%BC%8C%E4%BD%A0%E5%B0%B1%E7%9F%A5%E9%81%93%EF%BC%89",
    );

    expect(opener).toHaveBeenCalledWith("https://www.baidu.com/");
  });
});

describe("trimLinkPunctuation", () => {
  it("keeps a plain URL untouched", () => {
    expect(trimLinkPunctuation("https://example.com/a?b=1#c")).toBe(
      "https://example.com/a?b=1#c",
    );
  });

  it("drops the parenthetical an autolink swallowed", () => {
    // Raw in the message text …
    expect(trimLinkPunctuation("https://www.baidu.com/（页面标题：百度一下，你就知道）")).toBe(
      "https://www.baidu.com/",
    );
    // … and percent-encoded by the markdown pipeline, which is what a click
    // actually carries.
    expect(
      trimLinkPunctuation(
        "https://www.baidu.com/%EF%BC%88%E9%A1%B5%E9%9D%A2%E6%A0%87%E9%A2%98%EF%BC%9A%E7%99%BE%E5%BA%A6%E4%B8%80%E4%B8%8B%EF%BC%8C%E4%BD%A0%E5%B0%B1%E7%9F%A5%E9%81%93%EF%BC%89",
      ),
    ).toBe("https://www.baidu.com/");
  });

  it("drops trailing prose punctuation, raw or encoded", () => {
    expect(trimLinkPunctuation("https://example.com/x。")).toBe("https://example.com/x");
    expect(trimLinkPunctuation("https://example.com/x%E3%80%82")).toBe(
      "https://example.com/x",
    );
    // Punctuation in the middle of a sentence is not trailing, so it stays.
    expect(trimLinkPunctuation("https://example.com/x，然后")).toBe(
      "https://example.com/x，然后",
    );
  });

  it("drops an unbalanced closing ASCII paren", () => {
    expect(trimLinkPunctuation("https://example.com/x)")).toBe("https://example.com/x");
    expect(trimLinkPunctuation("https://en.wikipedia.org/wiki/Foo_(bar)")).toBe(
      "https://en.wikipedia.org/wiki/Foo_(bar)",
    );
  });

  it("keeps an encoded paren that is inside the path", () => {
    expect(trimLinkPunctuation("https://example.com/%EF%BC%88x%EF%BC%89/y")).toBe(
      "https://example.com/%EF%BC%88x%EF%BC%89/y",
    );
  });
});
