import { describe, expect, it } from "vitest";
import {
  browserLeafName,
  deriveBrowserPresentation,
  isBrowserTool,
} from "./browser-presentation";
import type { ToolInvocation } from "../../types";

function tool(overrides: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: "t1",
    call_id: "c1",
    parent_call_id: null,
    name: "mcp__playwright-mcp__browser_navigate",
    kind: "other",
    summary: "",
    status: "Succeeded",
    is_subagent: false,
    detail_text: "",
    logs: [],
    diff_paths: [],
    diff_previews: [],
    raw_input: null,
    raw_output: null,
    terminal_output: null,
    error: null,
    ...overrides,
  } as ToolInvocation;
}

describe("browserLeafName", () => {
  it("strips the namespace and server segment", () => {
    expect(
      browserLeafName("mcp__playwright-mcp__browser_take_screenshot"),
    ).toBe("browser_take_screenshot");
  });

  it("rejects a non-browser server", () => {
    // A different MCP server's tool is not a browser tool, even though it
    // shares the naming convention.
    expect(browserLeafName("mcp__kodex-web-tools__web_search")).toBeNull();
  });

  it("rejects an unnamespaced tool", () => {
    expect(browserLeafName("browser_click")).toBeNull();
    expect(browserLeafName("Bash")).toBeNull();
  });

  it("rejects a namespaced name with no tool segment", () => {
    expect(browserLeafName("mcp__playwright-mcp__")).toBeNull();
  });
});

describe("isBrowserTool", () => {
  it("recognises the browser family and nothing else", () => {
    expect(isBrowserTool({ name: "mcp__playwright-mcp__browser_click" })).toBe(true);
    expect(isBrowserTool({ name: "mcp__kodex-image__view_image" })).toBe(false);
    expect(isBrowserTool({ name: "Read" })).toBe(false);
  });
});

describe("deriveBrowserPresentation", () => {
  it("names a navigation and surfaces the URL", () => {
    const presentation = deriveBrowserPresentation(
      tool({ raw_input: JSON.stringify({ url: "https://example.test/docs" }) }),
    );
    expect(presentation.verb).toBe("Navigate");
    expect(presentation.subject).toBe("https://example.test/docs");
  });

  it("names a click and surfaces the selector", () => {
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_click",
        raw_input: JSON.stringify({ selector: "#submit" }),
      }),
    );
    expect(presentation.verb).toBe("Click");
    expect(presentation.subject).toBe("#submit");
  });

  it("marks a screenshot as a capture and suppresses its text output", () => {
    // The card renders the image as a thumbnail, so repeating the provider's
    // text as the body would be noise.
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_take_screenshot",
        raw_output: JSON.stringify({
          content: [{ type: "text", text: "captured 1280x720" }],
        }),
      }),
    );
    expect(presentation.verb).toBe("Screenshot");
    expect(presentation.isCapture).toBe(true);
    expect(presentation.detail).toBeNull();
  });

  it("marks a snapshot as a capture too", () => {
    const presentation = deriveBrowserPresentation(
      tool({ name: "mcp__playwright-mcp__browser_snapshot" }),
    );
    expect(presentation.isCapture).toBe(true);
    expect(presentation.verb).toBe("Snapshot");
  });

  it("keeps a non-capture tool's output as the detail line", () => {
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_click",
        raw_output: "clicked the submit button",
      }),
    );
    expect(presentation.detail).toBe("clicked the submit button");
  });

  it("reports a failure", () => {
    const failed = deriveBrowserPresentation(
      tool({ status: "Failed", error: "selector not found" }),
    );
    expect(failed.isError).toBe(true);

    const interrupted = deriveBrowserPresentation(
      tool({ status: "Interrupted" }),
    );
    expect(interrupted.isError).toBe(true);
  });

  it("falls back to a generic verb for an unrecognised tool", () => {
    // A provider upgrade can add tools; the card must still render.
    const presentation = deriveBrowserPresentation(
      tool({ name: "mcp__playwright-mcp__browser_teleport" }),
    );
    expect(presentation.verb).toBe("Browser");
  });

  it("still surfaces an argument for an unrecognised tool", () => {
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_teleport",
        raw_input: JSON.stringify({ url: "https://example.test" }),
      }),
    );
    expect(presentation.subject).toBe("https://example.test");
  });

  it("truncates a very long subject", () => {
    const presentation = deriveBrowserPresentation(
      tool({
        raw_input: JSON.stringify({ url: `https://example.test/${"a".repeat(400)}` }),
      }),
    );
    expect(presentation.subject!.length).toBeLessThanOrEqual(96);
    expect(presentation.subject!.endsWith("…")).toBe(true);
  });

  it("survives malformed raw input", () => {
    const presentation = deriveBrowserPresentation(
      tool({ raw_input: "{not json", raw_output: "also not json" }),
    );
    expect(presentation.verb).toBe("Navigate");
    expect(presentation.subject).toBeNull();
    expect(presentation.detail).toBe("also not json");
  });

  it("carries screenshot handles through for thumbnail rendering", () => {
    // The card renders these as thumbnails; the bytes never leave attachment
    // storage, so a handle is the whole contract.
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_take_screenshot",
        screenshots: [
          {
            path: "/tmp/shots/capture-abc.png",
            width: 1280,
            height: 720,
            byte_size: 204_800,
            media_type: "image/png",
          },
        ],
      } as Partial<ToolInvocation>),
    );

    expect(presentation.screenshots).toHaveLength(1);
    expect(presentation.screenshots[0].width).toBe(1280);
    expect(presentation.isCapture).toBe(true);
  });

  it("defaults to no captures for a tool that made none", () => {
    expect(deriveBrowserPresentation(tool()).screenshots).toEqual([]);
  });

  it("reads the key press argument", () => {
    const presentation = deriveBrowserPresentation(
      tool({
        name: "mcp__playwright-mcp__browser_press_key",
        raw_input: JSON.stringify({ key: "Enter" }),
      }),
    );
    expect(presentation.verb).toBe("Press");
    expect(presentation.subject).toBe("Enter");
  });
});
