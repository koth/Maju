import { describe, expect, it, vi } from "vitest";
import { markdownImageSrc } from "./MarkdownBody";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

describe("markdownImageSrc", () => {
  it("turns an absolute Windows path into an asset URL", () => {
    expect(markdownImageSrc("C:\\work\\shot.png")).toContain(
      encodeURIComponent("C:\\work\\shot.png"),
    );
  });

  it("turns a file:// URL into an asset URL", () => {
    expect(markdownImageSrc("file:///C:/work/shot.png")).toContain(
      encodeURIComponent("C:\\work\\shot.png"),
    );
  });

  it("resolves a workspace-relative path against the workspace root", () => {
    // What a browser tool writes: `baidu-home.png` next to its working
    // directory, which is the workspace the conversation resolves against.
    expect(markdownImageSrc("baidu-home.png", "D:\\work\\kodex")).toContain(
      encodeURIComponent("D:\\work\\kodex\\baidu-home.png"),
    );
  });

  it("leaves sources the webview can already fetch alone", () => {
    expect(markdownImageSrc("data:image/png;base64,AAAA")).toBe(
      "data:image/png;base64,AAAA",
    );
    expect(markdownImageSrc("https://example.com/a.png")).toBe(
      "https://example.com/a.png",
    );
    expect(markdownImageSrc("asset://localhost/x.png")).toBe("asset://localhost/x.png");
  });

  it("keeps a relative path as written when no workspace is known", () => {
    expect(markdownImageSrc("baidu-home.png")).toBe("baidu-home.png");
  });

  it("returns an empty source untouched", () => {
    expect(markdownImageSrc("")).toBe("");
  });
});
