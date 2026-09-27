import type { ScreenshotHandle, ToolInvocation } from "../../types";

/**
 * How a browser tool call reads in the conversation timeline.
 *
 * Agent-facing tool names are namespaced and verbose
 * (`mcp__playwright-mcp__browser_take_screenshot`). Showing that verbatim
 * would make the timeline unreadable, so the namespaced name is mapped to a
 * short verb and the interesting argument becomes the title.
 */
export interface BrowserToolPresentation {
  /** Short verb shown on the card, e.g. `Navigate`. */
  verb: string;
  /** The argument worth showing next to the verb, e.g. a URL or a selector. */
  subject: string | null;
  /** Compact one-line body, or null when the verb says it all. */
  detail: string | null;
  /** Whether the call captured an image worth thumbnailing. */
  isCapture: boolean;
  /**
   * Captures attached to this call, if the backend reported any. The card
   * renders these as thumbnails; the bytes stay in attachment storage.
   */
  screenshots: ScreenshotHandle[];
  /** Whether the call reports a failure. */
  isError: boolean;
}

const BROWSER_TOOL_PREFIX = "mcp__";
const SERVER_NAME = "playwright-mcp";

/** Names the provider advertises, mapped to the verb shown on the card. */
const VERB_BY_LEAF: Record<string, string> = {
  browser_navigate: "Navigate",
  browser_navigate_back: "Back",
  browser_navigate_forward: "Forward",
  browser_click: "Click",
  browser_type: "Type",
  browser_press_key: "Press",
  browser_fill_form: "Fill form",
  browser_select_option: "Select",
  browser_hover: "Hover",
  browser_drag: "Drag",
  browser_handle_dialog: "Dialog",
  browser_wait_for: "Wait",
  browser_file_upload: "Upload",
  browser_console_messages: "Console",
  browser_network_requests: "Network",
  browser_snapshot: "Snapshot",
  browser_take_screenshot: "Screenshot",
  browser_screenshot: "Screenshot",
  browser_install: "Install",
};

/** Arguments worth surfacing, in preference order per verb. */
const SUBJECT_KEYS_BY_LEAF: Record<string, string[]> = {
  browser_navigate: ["url"],
  browser_click: ["selector", "ref", "element"],
  browser_type: ["selector", "ref", "element", "text"],
  browser_hover: ["selector", "ref", "element"],
  browser_drag: ["selector", "ref", "element"],
  browser_fill_form: ["selector", "ref", "element"],
  browser_select_option: ["selector", "ref", "element"],
  browser_press_key: ["key"],
  browser_wait_for: ["text", "selector", "time"],
  browser_file_upload: ["selector", "path"],
};

/** The provider tool name inside a namespaced model-facing name. */
export function browserLeafName(toolName: string): string | null {
  const trimmed = toolName.trim();
  if (!trimmed.startsWith(BROWSER_TOOL_PREFIX)) return null;

  const rest = trimmed.slice(BROWSER_TOOL_PREFIX.length);
  // `mcp__<server>__<tool>`; a different server is a different tool family.
  const separator = rest.indexOf("__");
  if (separator < 0) return null;
  const server = rest.slice(0, separator);
  if (server !== SERVER_NAME) return null;

  const leaf = rest.slice(separator + 2);
  return leaf.length > 0 ? leaf : null;
}

export function isBrowserTool(tool: Pick<ToolInvocation, "name">): boolean {
  return browserLeafName(tool.name) !== null;
}

function readString(source: unknown, key: string): string | null {
  if (!source || typeof source !== "object") return null;
  const value = (source as Record<string, unknown>)[key];
  if (typeof value === "string" && value.trim()) return value.trim();
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return null;
}

/** Pull the first present argument out of a tool call, however it was passed. */
function subjectFor(leaf: string, tool: ToolInvocation): string | null {
  const keys = SUBJECT_KEYS_BY_LEAF[leaf];
  const sources: unknown[] = [
    safeParse(tool.raw_input ?? ""),
    tool.raw_output ? safeParse(tool.raw_output) : null,
  ];
  if (keys) {
    for (const source of sources) {
      for (const key of keys) {
        const found = readString(source, key);
        if (found) return found;
      }
    }
    return null;
  }
  // An unrecognised tool still gets its most likely argument rather than a
  // bare verb, so the card is not empty.
  for (const source of sources) {
    for (const key of ["url", "selector", "query", "text", "value"]) {
      const found = readString(source, key);
      if (found) return found;
    }
  }
  return null;
}

function safeParse(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function truncate(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, max - 1)}…`;
}

/** Build the card presentation for a browser tool call. */
export function deriveBrowserPresentation(tool: ToolInvocation): BrowserToolPresentation {
  const leaf = browserLeafName(tool.name) ?? "";
  const verb = VERB_BY_LEAF[leaf] ?? "Browser";
  const subject = subjectFor(leaf, tool);
  const isCapture =
    leaf.endsWith("screenshot") || leaf.endsWith("snapshot");

  const text = (tool.raw_output ?? tool.detail_text ?? "").trim();
  const failed =
    tool.status === "Failed" || tool.status === "Interrupted" || Boolean(tool.error);

  return {
    verb,
    screenshots: tool.screenshots ?? [],
    subject: subject ? truncate(subject, 96) : null,
    // A capture's output is the image itself, which the card renders as a
    // thumbnail; repeating it as text would just be noise.
    detail: isCapture || !text ? null : truncate(text, 240),
    isCapture,
    isError: failed,
  };
}
