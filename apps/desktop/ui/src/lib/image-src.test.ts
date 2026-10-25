import { describe, expect, it, vi } from "vitest";
import { webviewImageSrc } from "./image-src";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

function asset(path: string): string {
  return `asset://localhost/${encodeURIComponent(path)}`;
}

describe("webviewImageSrc", () => {
  it("uses the native spelling of a Windows path, including duplicate separators", () => {
    expect(webviewImageSrc("C:\\work\\shot.png")).toBe(asset("C:\\work\\shot.png"));
    expect(webviewImageSrc("C:\\\\work\\\\shot.png")).toBe(asset("C:\\work\\shot.png"));
    expect(webviewImageSrc("C:%5Cwork%5Cshot.png")).toBe(asset("C:\\work\\shot.png"));
  });

  it("decodes file URLs before converting them, including spaces in names", () => {
    expect(webviewImageSrc("file:///C:/work/original%20image.png")).toBe(
      asset("C:\\work\\original image.png"),
    );
    expect(webviewImageSrc("file:///Users/test/original%20image.png")).toBe(
      asset("/Users/test/original image.png"),
    );
  });

  it("accepts an extension-less DSH attachment object path unchanged", () => {
    const objectPath =
      "C:\\Users\\yvonchen\\.kodex\\dsh\\attachments\\v1\\objects\\2a\\2a2bff17c5fb28062325ee1463ae796a6a9a1be2a9a78b8179d416adbed87c6a";
    expect(webviewImageSrc(objectPath)).toBe(asset(objectPath));
  });

  it("turns a file URL host into a UNC path", () => {
    expect(webviewImageSrc("file://server/share/shot.png")).toBe(
      asset("\\\\server\\share\\shot.png"),
    );
  });

  it("joins relative paths with native Windows and POSIX workspace separators", () => {
    expect(webviewImageSrc("images/shot.png", "D:\\work\\kodex")).toBe(
      asset("D:\\work\\kodex\\images\\shot.png"),
    );
    expect(webviewImageSrc("images\\shot.png", "/workspace/kodex")).toBe(
      asset("/workspace/kodex/images/shot.png"),
    );
  });

  it("preserves a literal percent that is not an escape", () => {
    expect(webviewImageSrc("file:///C:/work/100%shot.png")).toBe(
      asset("C:\\work\\100%shot.png"),
    );
  });

  it("leaves already fetchable sources and unresolved relative paths alone", () => {
    expect(webviewImageSrc("data:image/png;base64,AAAA")).toBe(
      "data:image/png;base64,AAAA",
    );
    expect(webviewImageSrc("https://example.com/a.png")).toBe(
      "https://example.com/a.png",
    );
    expect(webviewImageSrc("asset://localhost/x.png")).toBe("asset://localhost/x.png");
    expect(webviewImageSrc("blob:https://example.com/id")).toBe(
      "blob:https://example.com/id",
    );
    expect(webviewImageSrc("relative.png")).toBe("relative.png");
    expect(webviewImageSrc("")).toBe("");
  });
});
