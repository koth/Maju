import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownBody from "./MarkdownBody";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  convertFileSrc: (path: string) => `http://asset.localhost/${encodeURIComponent(path)}`,
}));

afterEach(cleanup);

function renderedImageSource(content: string): string | null {
  const { container } = render(<MarkdownBody content={content} />);
  return container.querySelector("img.md-image")?.getAttribute("src") ?? null;
}

describe("Markdown local image paths", () => {
  it("keeps the separator before a dot-directory in a Windows path", () => {
    const path = String.raw`C:\Users\yvonchen\AppData\Local\Temp\.playwright-mcp\zhihu-reopen.png`;
    expect(renderedImageSource(`![zhihu](${path})`)).toBe(
      `http://asset.localhost/${encodeURIComponent(path)}`,
    );
  });

  it("keeps an angle-bracketed Windows path with spaces intact", () => {
    const path = String.raw`C:\Users\Jane Doe\AppData\Local\Temp\.playwright-mcp\shot.png`;
    expect(renderedImageSource(`![shot](<${path}>)`)).toBe(
      `http://asset.localhost/${encodeURIComponent(path)}`,
    );
  });

  it("restores a file URL whose local path contains a dot-directory", () => {
    const path = String.raw`C:\Users\test\AppData\Local\Temp\.playwright-mcp\shot.png`;
    const fileUrl = String.raw`file:///C:\Users\test\AppData\Local\Temp\.playwright-mcp\shot.png`;
    expect(renderedImageSource(`![cached](${fileUrl})`)).toBe(
      `http://asset.localhost/${encodeURIComponent(path)}`,
    );
  });

  it("does not rewrite image syntax inside a code fence", () => {
    const path = String.raw`C:\Users\me\Temp\.playwright-mcp\shot.png`;
    const { container } = render(<MarkdownBody content={`\`\`\`md\n![shot](${path})\n\`\`\``} />);
    expect(container.querySelector("img.md-image")).toBeNull();
    expect(container.textContent).toContain(".playwright-mcp");
  });
});
