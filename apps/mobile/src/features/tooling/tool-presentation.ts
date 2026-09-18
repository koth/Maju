import type { ToolInvocation } from "../../types";

// Mobile port of the desktop tooling/tool-presentation.ts, reduced to what
// the RN collapsed tool row needs: a bullet tone, a short verb, a one-line
// header title, and the detail/output lines shown when expanded.

export type ToolTone = "running" | "ok" | "danger" | "warning";

export interface ToolPresentation {
  tone: ToolTone;
  verb: string;
  title: string;
  command: string | null;
  outputLines: string[];
  hasDetail: boolean;
}

const MAX_TITLE_CHARS = 96;
const MAX_OUTPUT_LINES = 10;

const EDIT_NAME_PATTERN = /str_replace|apply_patch|edit|write|create_file|multi_?edit/i;
const READ_ONLY_COMMAND =
  /^(ls|dir|cat|head|tail|grep|rg|find|tree|pwd|which|where|echo|man|wc|stat|file|du|df)\b|^(git|hg|svn) (status|log|diff|show|branch|blame|shortlog|describe)\b/;

export function deriveToolPresentation(tool: ToolInvocation): ToolPresentation {
  const running = tool.status === "Pending" || tool.status === "Running";
  const tone: ToolTone =
    tool.status === "Succeeded"
      ? "ok"
      : tool.status === "Failed"
        ? "danger"
        : tool.status === "Interrupted"
          ? "warning"
          : "running";

  const command = extractCommand(tool);
  const verb = verbFor(tool, running);
  const title = headerTitle(tool, command);
  return {
    tone,
    verb,
    title,
    command,
    outputLines: extractOutputLines(tool),
    hasDetail: hasExpandableDetail(tool),
  };
}

/// What a call *did*, independent of whether it is still running. The
/// activity summary and the row verb are both derived from this, so a
/// collapsed run can never describe its members with a different vocabulary
/// than the rows it stands in for (desktop `classifyTool`, reduced to the
/// signals the phone actually receives).
export type ToolActivityCategory = "exploring" | "executing" | "editing" | "asking";

export function classifyToolActivity(tool: ToolInvocation): ToolActivityCategory {
  if (isQuestionTool(tool)) return "asking";
  if (isEditTool(tool)) return "editing";
  const command = extractCommand(tool);
  if (command && isExplorationCommand(command)) return "exploring";
  if (isExploreTool(tool)) return "exploring";
  return "executing";
}

/// Read-only tool names — the desktop `isExploreTool` list. Without this a
/// file `Read` (no shell command to inspect) fell through to "executing" and
/// its row claimed it "Ran" a file.
const EXPLORE_TOOL_PATTERN = /read|view|open|search|list|glob|grep|find|webfetch|fetch/i;

function isExploreTool(tool: ToolInvocation): boolean {
  if (EXPLORE_TOOL_PATTERN.test(`${tool.kind} ${tool.name}`)) return true;
  return rawInputHasPath(tool);
}

function rawInputHasPath(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input) as Record<string, unknown>;
    return Boolean(input.file_path || input.filePath || input.path);
  } catch {
    return false;
  }
}

const RUNNING_VERBS: Record<ToolActivityCategory, string> = {
  exploring: "Searching",
  executing: "Running",
  editing: "Editing",
  asking: "Asking",
};

/// Past-tense verbs, shared with the activity summary.
export const FINISHED_VERBS: Record<ToolActivityCategory, string> = {
  exploring: "Searched",
  executing: "Ran",
  editing: "Edited",
  asking: "Asked",
};

function verbFor(tool: ToolInvocation, running: boolean): string {
  if (tool.status === "Failed") return "Failed";
  if (tool.status === "Interrupted") return "Interrupted";
  const category = classifyToolActivity(tool);
  return running ? RUNNING_VERBS[category] : FINISHED_VERBS[category];
}

function isEditTool(tool: ToolInvocation): boolean {
  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  return (
    EDIT_NAME_PATTERN.test(tool.name.toLowerCase()) ||
    identity.includes("edit") ||
    identity.includes("write") ||
    tool.diff_previews.length > 0
  );
}


function isQuestionTool(tool: ToolInvocation): boolean {
  return (tool.permission_input?.questions.length ?? 0) > 0;
}

function isExplorationCommand(command: string): boolean {
  return READ_ONLY_COMMAND.test(command.trim().toLowerCase());
}

// Command-like tools carry a shell command (desktop: isCommandLikeTool).
// str_replace_editor has a `command` sub-selector but is never a shell tool.
function isCommandLikeTool(tool: ToolInvocation): boolean {
  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  if (tool.name.toLowerCase() === "str_replace_editor") return false;
  return (
    identity.includes("bash") ||
    identity.includes("execute") ||
    identity.includes("command") ||
    identity.includes("terminal") ||
    tool.name.startsWith("`")
  );
}

function extractCommand(tool: ToolInvocation): string | null {
  if (isCommandLikeTool(tool)) {
    const fromInput = commandFromJson(tool.raw_input);
    if (fromInput) return fromInput;
    if (tool.name.startsWith("`") && tool.name.endsWith("`")) {
      const inner = tool.name.slice(1, -1).trim();
      if (inner) return inner;
    }
  }
  return null;
}

function commandFromJson(rawInput: string | null): string | null {
  if (!rawInput) return null;
  try {
    const parsed = JSON.parse(rawInput) as Record<string, unknown>;
    for (const key of ["command", "cmd", "shell_command", "command_line"]) {
      const value = parsed[key];
      if (typeof value === "string" && value.trim()) return value.trim();
      if (Array.isArray(value)) {
        const parts = value.filter((part): part is string => typeof part === "string");
        if (parts.length > 0) return parts.join(" ");
      }
    }
  } catch {
    // raw_input is not JSON; fall through.
  }
  return null;
}

function headerTitle(tool: ToolInvocation, command: string | null): string {
  if (command) return truncate(command.replace(/\s+/g, " "), MAX_TITLE_CHARS);
  if (tool.diff_previews.length > 0) return truncate(tool.diff_previews[0].path, MAX_TITLE_CHARS);
  const summary = tool.summary.trim();
  if (summary && !isGenericTitle(summary)) return truncate(summary.replace(/\s+/g, " "), MAX_TITLE_CHARS);
  return truncate(tool.detail_text.split("\n")[0].replace(/\s+/g, " "), MAX_TITLE_CHARS) || tool.name;
}

// Desktop `isGenericTitle`: summaries like "completed" carry no information.
function isGenericTitle(summary: string): boolean {
  return /^(completed|done|ok|success(ed)?|finished|ran( successfully)?|read)\.?$/i.test(summary.trim());
}

function extractOutputLines(tool: ToolInvocation): string[] {
  const source =
    tool.terminal_output?.output ||
    tool.error ||
    tool.detail_text ||
    tool.logs
      .map((entry) => (entry.body ? `${entry.title} ${entry.body}` : entry.title))
      .join("\n");
  if (!source) return [];
  const lines = source
    .replace(/\n+$/, "")
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0);
  return lines.slice(0, MAX_OUTPUT_LINES);
}

function hasExpandableDetail(tool: ToolInvocation): boolean {
  return (
    tool.diff_previews.length > 0 ||
    tool.logs.length > 0 ||
    !!tool.raw_input ||
    !!tool.raw_output ||
    !!tool.detail_text ||
    extractOutputLines(tool).length > 0
  );
}

function truncate(value: string, max: number): string {
  return value.length > max ? `${value.slice(0, max - 1)}\u2026` : value;
}
// end of file