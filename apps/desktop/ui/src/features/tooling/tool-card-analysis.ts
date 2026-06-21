import type { DiffHunk, ToolDiffPreview, ToolInvocation, ToolStatus } from "../../types";
import { DIFF_CONTEXT_LINES, formatPatchRange, getDiffStats } from "./compact-patch";
import { normalizeToolCommand } from "./tool-presentation";

const MAX_OUTPUT_LINES = 5;

export interface ExplorationResult {
  root: string | null;
  items: string[];
  omitted: number;
}

/**
 * Extract a short, human-readable title for the header line.
 * Prefers stable input metadata and file paths over completion output.
 * Keeps raw commands in the expanded detail instead of the header.
 */
export function extractHeaderTitle(tool: ToolInvocation, trackedDiffPaths: string[]): string {
  if (isQuestionTool(tool)) {
    return questionToolAnswerTitle(tool) ?? "提问";
  }

  if (isTodoWriteTool(tool)) {
    return "任务计划";
  }

  const inputTitle = extractInputTitle(tool);
  if (inputTitle) return truncate(inputTitle, 80);

  // For edit tools, show workspace-relative path for context
  if (trackedDiffPaths.length > 0) {
    return truncate(trackedDiffPaths[trackedDiffPaths.length - 1].replace(/\\/g, "/"), 80);
  }

  const logPath = pathFromToolLogs(tool);
  if (logPath) {
    return truncate(logPath, 80);
  }

  const namePath = extractPathFromToolName(tool.name);
  if (namePath) {
    return displayPath(namePath);
  }

  // Use summary only if it looks like a real description, not output or file content.
  if (
    tool.summary &&
    !tool.summary.startsWith("Editing ") &&
    !looksLikeToolOutput(tool.summary) &&
    tool.summary !== tool.name &&
    !isGenericTitle(tool.summary) &&
    isUsefulTitle(tool.summary) &&
    !looksLikeCommand(tool.summary) &&
    !looksLikeDisplayPayload(tool.summary) &&
    !looksLikePath(tool.summary)
  ) {
    return truncate(tool.summary, 80);
  }

  // If name is a backtick-wrapped command, use just the tool kind
  if (tool.name.startsWith("`")) {
    return tool.kind || "命令";
  }

  if (isCommandTool(tool)) {
    return commandToolLabel(tool);
  }

  // If name itself is useful and not just a generic label like "Tool"
  if (
    isUsefulTitle(tool.name) &&
    !isGenericTitle(tool.name) &&
    !looksLikeCommand(tool.name) &&
    !looksLikeDisplayPayload(tool.name)
  ) {
    return truncate(tool.name, 80);
  }

  // Last resort: tool kind or generic label
  return tool.kind || tool.name || "工具";
}

/**
 * Extract a human-readable title from raw_input for the header.
 * Prefers description > short file name > pattern. NOT raw commands.
 */
export function extractInputTitle(tool: ToolInvocation): string | null {
  if (!tool.raw_input) return null;
  try {
    const input = JSON.parse(tool.raw_input);

    // Description is the best header title (human-readable summary)
    if (input.description && typeof input.description === "string") {
      return input.description;
    }

    // File path: keep the path in the header so exploration has useful context.
    if (input.file_path || input.filePath || input.path) {
      const p = String(input.file_path || input.filePath || input.path);
      return displayPath(p);
    }

    // Pattern (grep/glob)
    if (input.pattern && typeof input.pattern === "string") {
      const path = input.path || input.include;
      return path ? `${input.pattern} in ${displayPath(String(path))}` : input.pattern;
    }

    // URL, prompt, query
    if (input.url && typeof input.url === "string") return truncate(input.url, 60);
    if (input.prompt && typeof input.prompt === "string") return truncate(input.prompt, 60);
    if (input.query && typeof input.query === "string") return truncate(input.query, 60);

    // Commands belong in the expanded detail, not the header.
  } catch {
    const path = rawInputFilePath(tool);
    if (path) {
      return displayPath(path);
    }
    if (
      tool.raw_input &&
      !looksLikeJsonPayload(tool.raw_input) &&
      looksLikePath(tool.raw_input) &&
      !looksLikeCommand(tool.raw_input)
    ) {
      return displayPath(tool.raw_input);
    }
    if (
      tool.raw_input &&
      !looksLikeJsonPayload(tool.raw_input) &&
      isUsefulTitle(tool.raw_input) &&
      !looksLikeCommand(tool.raw_input) &&
      !looksLikePath(tool.raw_input)
    ) {
      return tool.raw_input;
    }
  }
  return null;
}

export function isGenericTitle(text: string): boolean {
  const lower = text.trim().toLowerCase();
  return (
    lower === "tool" ||
    lower === "bash" ||
    lower === "shell" ||
    lower === "terminal" ||
    lower === "command" ||
    lower === "completed" ||
    lower === "succeeded" ||
    lower === "executing" ||
    lower === "working" ||
    lower === "tool failed"
  );
}

export function extractPathFromToolName(name: string): string | null {
  const match = name.match(/^(?:Write|Read|Edit)\s+`?(.+?)`?$/i);
  return match?.[1] ?? null;
}

/** Check if a string is a useful human-readable title (not garbage/fragments) */
export function isUsefulTitle(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length < 3) return false;
  // JSON fragments: starts/ends with braces, brackets, quotes
  if (/^[{}\[\]"'`]$/.test(trimmed)) return false;
  if (/^[{}\[\]"'`]/.test(trimmed) && trimmed.length < 6) return false;
  // Pure punctuation or whitespace
  if (/^[\s\W]+$/.test(trimmed)) return false;
  return true;
}

/** Check if a string looks like a file path (contains slashes or backslashes) */
export function looksLikePath(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed || looksLikeMarkupFragment(trimmed) || !isUsableWritePath(trimmed)) return false;
  return /[/\\]/.test(trimmed);
}

export function looksLikeMarkupFragment(text: string): boolean {
  const trimmed = text.trim();
  return /[<>]/.test(trimmed) || /<\/?[a-z][^>]*>?/i.test(trimmed);
}

export function looksLikeCommand(text: string): boolean {
  const trimmed = normalizeToolCommand(text.trim());
  if (!trimmed) return false;
  if (looksLikeJsonPayload(trimmed)) return false;
  if (trimmed.startsWith("`") && trimmed.endsWith("`")) return true;
  if (/[;&|]/.test(trimmed)) return true;
  return /^(?:bash|sh|zsh|cmd|powershell|pwsh|npm|pnpm|yarn|bun|cargo|git|ls|dir|cd|mkdir|rm|cp|mv|python|node|npx)\b/i.test(
    trimmed
  );
}

export function looksLikeDisplayPayload(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;
  if (looksLikeJsonPayload(trimmed)) return true;
  if (trimmed.includes("\n")) return true;
  if (/^\d+\s*[→:|]\s*/.test(trimmed)) return true;
  if (/^#{1,6}\s+/.test(trimmed)) return true;
  if (/^(?:import|export|function|class|const|let|var|use|pub)\s/.test(trimmed)) return true;
  if (/^Successfully\s+(?:edited|wrote|updated)\s+file:/i.test(trimmed)) return true;
  return false;
}

export function looksLikeJsonPayload(text: string): boolean {
  const trimmed = text.trim();
  return trimmed.startsWith("{") || trimmed.startsWith("[");
}

/** Returns true for strings that look like tool completion output, not a description */
export function looksLikeToolOutput(text: string): boolean {
  const trimmed = text.trim();
  if (/^Exit code:\s*\d/i.test(trimmed)) return true;
  if (/^Completed\s*\|/i.test(trimmed)) return true;
  if (/^Exited with code\s/i.test(trimmed)) return true;
  return false;
}

/**
 * Extract the detailed command/path for the expandable section.
 * Returns null if there's nothing meaningful to show beyond the header.
 */
export function extractCommandDetail(tool: ToolInvocation, trackedDiffPaths: string[]): string | null {
  // For edit tools, show full file path
  if (trackedDiffPaths.length > 0) {
    return trackedDiffPaths[trackedDiffPaths.length - 1];
  }

  if (tool.raw_input) {
    try {
      const input = JSON.parse(tool.raw_input);

      if (input.command) {
        return normalizeToolCommand(String(input.command));
      }

      if (input.file_path || input.filePath || input.path) {
        return String(input.file_path || input.filePath || input.path);
      }

      if (input.pattern) {
        const pattern = String(input.pattern);
        const path = input.path || input.include;
        return path ? `${pattern} in ${path}` : pattern;
      }

      if (input.url) return String(input.url);
      if (input.query) return String(input.query);
    } catch {
      const path = rawInputFilePath(tool);
      if (path) return path;
      return tool.raw_input;
    }
  }

  // If the name itself looks like a command (backtick-wrapped), show it
  if (tool.name.startsWith("`") && tool.name.endsWith("`")) {
    return tool.name.slice(1, -1);
  }

  return null;
}

export function displayPath(path: string): string {
  return path.trim().replace(/^[`'"]+|[`'"]+$/g, "").replace(/\\/g, "/");
}

export function getExplorationResult(
  tool: ToolInvocation,
  cmdDetail: string | null,
  detailLines: string[],
  outputLines: string[],
  rawOutputLines: string[],
): ExplorationResult | null {
  const root = firstDisplayPath(cmdDetail ?? tool.summary ?? detailLines[0] ?? null);
  const items = uniqueStrings([
    ...pathsFromRawPayload(tool.raw_output),
    ...pathsFromRawPayload(tool.detail_text),
    ...pathsFromRawPayload(tool.summary),
    ...outputLines.flatMap(pathsFromText),
    ...rawOutputLines.flatMap(pathsFromText),
    ...detailLines.slice(root ? 1 : 0).flatMap(pathsFromText),
  ])
    .map(displayPath)
    .filter((path) => path && path !== root);

  if (!root && items.length === 0) return null;

  const visibleLimit = 8;
  return {
    root,
    items: items.slice(0, visibleLimit),
    omitted: Math.max(0, items.length - visibleLimit),
  };
}

export function pathsFromRawPayload(raw: string | null | undefined): string[] {
  if (!raw?.trim()) return [];
  const parsed = parseJsonValue(raw);
  if (Array.isArray(parsed)) {
    return parsed.filter((item): item is string => typeof item === "string" && looksLikePath(item));
  }
  if (parsed && typeof parsed === "object") {
    const record = parsed as Record<string, unknown>;
    return [
      ...pathsFromUnknown(record.path),
      ...pathsFromUnknown(record.file_path),
      ...pathsFromUnknown(record.filePath),
      ...pathsFromUnknown(record.paths),
      ...pathsFromUnknown(record.files),
      ...pathsFromUnknown(record.matches),
      ...pathsFromText(String(record.output ?? record.formatted_output ?? "")),
    ];
  }
  return pathsFromText(raw);
}

export function pathsFromUnknown(value: unknown): string[] {
  if (typeof value === "string") return looksLikePath(value) ? [value] : [];
  if (Array.isArray(value)) return value.flatMap(pathsFromUnknown);
  if (value && typeof value === "object") {
    return Object.values(value as Record<string, unknown>).flatMap(pathsFromUnknown);
  }
  return [];
}

export function pathsFromText(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed || looksLikeDisplayPayload(trimmed)) return [];
  const parsed = parseJsonValue(trimmed);
  if (parsed !== null) return pathsFromUnknown(parsed);
  return trimmed
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => looksLikePath(line) && !looksLikeCommand(line));
}

export function firstDisplayPath(value: string | null): string | null {
  if (!value) return null;
  const paths = pathsFromText(value);
  const path = paths[0] ?? (looksLikePath(value) ? value : null);
  return path ? displayPath(path) : null;
}

export function pathFromToolLogs(tool: ToolInvocation): string | null {
  for (const entry of [...tool.logs].reverse()) {
    const path = pathFromLogText(entry.body);
    if (path) return path;
  }
  return null;
}

export function pathFromLogText(text: string): string | null {
  for (const line of splitLogPathLines(text)) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    const labeled = trimmed.match(
      /^(?:Requested\s+)?(?:Write|Read|Edit|Update|编辑|已编辑)\s+(.+)$/i,
    );
    if (labeled) {
      const value = cleanPathCandidate(labeled[1]);
      if (looksLikePath(value) && !looksLikeJsonPayload(value)) {
        return displayPath(value);
      }
    }

    const pathMatch = trimmed.match(
      /(?:[a-zA-Z]:[\\/][^\s`'"]+|\/[a-zA-Z](?:\/[^\s`'"]+)+|(?:[\w.-]+[\\/])+(?:[\w .@()[\]-]+))/,
    );
    if (pathMatch) {
      const value = cleanPathCandidate(pathMatch[0]);
      if (looksLikePath(value)) {
        return displayPath(value);
      }
    }
  }
  return null;
}

export function splitLogPathLines(text: string): string[] {
  return text.split(/\r?\n/);
}

export function cleanPathCandidate(candidate: string): string {
  let cleaned = candidate.trim().replace(/^[`'"]+|[`'".,;:)]+$/g, "");
  if (!/^[a-zA-Z]:[\\/]/.test(cleaned)) {
    cleaned = cleaned.split(/\\r\\n|\\n|\\r/)[0] ?? cleaned;
  }
  cleaned = cleaned.replace(/:\d+(?::\d+)?$/, "");
  return cleaned;
}

export function parseJsonValue(raw: string): unknown | null {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

export function uniqueStrings(values: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const value of values) {
    const normalized = value.trim();
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    result.push(normalized);
  }
  return result;
}

/** Truncate to max chars, single line */
export function truncate(text: string, max: number): string {
  const firstLine = text.split("\n")[0] ?? text;
  if (firstLine.length > max) {
    return firstLine.slice(0, max - 3) + "...";
  }
  return firstLine;
}

export type ToolCategory = "exploring" | "editing" | "executing" | "asking";

export function commandHeaderTitle(
  command: string | null,
  category: ToolCategory,
  tool: ToolInvocation,
): string {
  if (command && category === "exploring") {
    const parsedTarget = parsedCommandTarget(tool);
    if (parsedTarget) return truncate(displayPath(parsedTarget), 96);

    const target = extractExplorationCommandTarget(command);
    if (target) return truncate(displayPath(target), 96);
  }
  return truncate(command ?? commandSummaryTitle(tool) ?? commandToolLabel(tool), 96);
}

export function commandSummaryTitle(tool: ToolInvocation): string | null {
  const summary = tool.summary.trim();
  if (
    summary &&
    !isGenericTitle(summary) &&
    !looksLikeToolOutput(summary) &&
    !looksLikeDisplayPayload(summary)
  ) {
    return summary;
  }
  return pathFromToolLogs(tool);
}

export function classifyCommandPresentation(command: string | null): ToolCategory {
  if (!command) return "executing";
  return isExplorationCommand(command) ? "exploring" : "executing";
}

export function isExplorationCommand(command: string): boolean {
  if (commandWritePaths(command).length > 0 || commandCleanupPaths(command).length > 0) {
    return false;
  }
  const tokens = tokenizeCommandLine(command);
  const commandName = firstCommandNameFromTokens(tokens);
  return (
    commandName === "awk" ||
    commandName === "cut" ||
    commandName === "egrep" ||
    commandName === "fgrep" ||
    commandName === "file" ||
    commandName === "grep" ||
    commandName === "head" ||
    commandName === "less" ||
    commandName === "more" ||
    commandName === "pwd" ||
    commandName === "rg" ||
    commandName === "ripgrep" ||
    commandName === "sort" ||
    commandName === "stat" ||
    commandName === "tail" ||
    commandName === "tree" ||
    commandName === "uniq" ||
    commandName === "wc" ||
    commandName === "where" ||
    commandName === "which" ||
    (commandName === "find" && findCommandLooksReadOnly(tokens)) ||
    (commandName === "sed" && sedCommandLooksReadOnly(tokens)) ||
    (commandName === "git" && gitCommandLooksReadOnly(tokens)) ||
    commandName === "get-content" ||
    commandName === "gc" ||
    commandName === "cat" ||
    commandName === "type" ||
    commandName === "get-childitem" ||
    commandName === "gci" ||
    commandName === "ls" ||
    commandName === "dir" ||
    commandName === "select-string" ||
    commandName === "select-object" ||
    commandName === "test-path"
  );
}

export function extractExplorationCommandTarget(command: string): string | null {
  const tokens = tokenizeCommandLine(command);
  if (tokens.length === 0) return null;
  const commandName = firstCommandNameFromTokens(tokens);

  for (let i = 1; i < tokens.length - 1; i += 1) {
    const lower = tokens[i].toLowerCase();
    if (lower === "-path" || lower === "-literalpath") {
      return tokens[i + 1];
    }
  }

  if (explorationCommandPrefersLastPositionalTarget(commandName)) {
    const target = lastPositionalCommandToken(tokens);
    if (target) return target;
  }

  for (let i = 1; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === "|" || token === ";" || token === "&&" || token === "||") break;
    if (token.startsWith("-")) {
      i += powershellSwitchLooksValued(token) ? 1 : 0;
      continue;
    }
    return token;
  }

  return null;
}

export function firstCommandName(command: string): string {
  return firstCommandNameFromTokens(tokenizeCommandLine(command));
}

export function firstCommandNameFromTokens(tokens: string[]): string {
  const first = tokens[0] ?? "";
  const base = displayPath(first).split("/").pop() ?? first;
  return base.replace(/\.exe$/i, "").toLowerCase();
}

export function findCommandLooksReadOnly(tokens: string[]): boolean {
  return !tokens.some((token) =>
    ["-delete", "-exec", "-execdir", "-ok", "-okdir"].includes(token.toLowerCase()),
  );
}

export function sedCommandLooksReadOnly(tokens: string[]): boolean {
  return !tokens.some((token) => token.toLowerCase().startsWith("-i"));
}

export function gitCommandLooksReadOnly(tokens: string[]): boolean {
  const subcommand = tokens.slice(1).find((token) => !token.startsWith("-"))?.toLowerCase();
  return !!subcommand && [
    "blame",
    "diff",
    "grep",
    "log",
    "ls-files",
    "rev-parse",
    "show",
    "status",
  ].includes(subcommand);
}

export function explorationCommandPrefersLastPositionalTarget(commandName: string): boolean {
  return [
    "awk",
    "cut",
    "egrep",
    "fgrep",
    "file",
    "grep",
    "head",
    "rg",
    "ripgrep",
    "sed",
    "stat",
    "tail",
    "wc",
  ].includes(commandName);
}

export function lastPositionalCommandToken(tokens: string[]): string | null {
  let last: string | null = null;
  for (let i = 1; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === "|" || token === ";" || token === "&&" || token === "||") break;
    if (token.startsWith("-")) {
      i += powershellSwitchLooksValued(token) ? 1 : 0;
      continue;
    }
    last = token;
  }
  return last;
}

export function powershellSwitchLooksValued(token: string): boolean {
  const lower = token.toLowerCase();
  return [
    "-depth",
    "-erroraction",
    "-exclude",
    "-filter",
    "-first",
    "-include",
    "-totalcount",
  ].includes(lower);
}

export function tokenizeCommandLine(command: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quote: '"' | "'" | null = null;

  for (let i = 0; i < command.length; i += 1) {
    const char = command[i];
    const next = command[i + 1];

    if (char === "\\" && quote === '"' && next === '"') {
      current += '"';
      i += 1;
      continue;
    }

    if (char === '"' || char === "'") {
      if (quote === char) {
        quote = null;
        continue;
      }
      if (!quote) {
        quote = char;
        continue;
      }
    }

    if (!quote && /\s/.test(char)) {
      if (current) {
        tokens.push(current);
        current = "";
      }
      continue;
    }

    current += char;
  }

  if (current) tokens.push(current);
  return tokens;
}

export function classifyTool(tool: ToolInvocation): ToolCategory {
  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  const subagentType = getSubagentType(tool);

  if (isQuestionTool(tool)) {
    return "asking";
  }

  if (isTodoWriteTool(tool)) {
    return "executing";
  }

  if (isCodeBuddySkillTool(tool)) {
    return "executing";
  }

  if (rawInputHasEditPayload(tool)) {
    return "editing";
  }

  if (isExplicitEditToolInvocation(tool)) {
    return "editing";
  }

  if (rawInputHasReadOnlyParsedCommand(tool)) {
    return "exploring";
  }

  if (subagentType === "explore") {
    return "exploring";
  }

  if (isCommandTool(tool)) {
    return "executing";
  }
  if (isExploreTool(tool, `${identity} ${tool.summary}`.toLowerCase())) {
    return "exploring";
  }
  return "executing";
}

export function isExplicitEditTool(identity: string): boolean {
  return /(^|[\s._:-])(?:edit|write|patch|apply[_-]?patch)([\s._:-]|$)/.test(
    identity
  );
}

export function isExplicitEditToolInvocation(tool: ToolInvocation): boolean {
  return isExplicitEditTool(`${tool.kind} ${tool.name}`.toLowerCase());
}

export function rawInputHasEditPayload(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  if (rawInputHasReadOnlyParsedCommand(tool)) return false;
  const input = parseJsonValue(tool.raw_input);
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    const path = rawInputFilePath(tool);
    if (!path) return false;
    return rawTextHasAnyKey(
      tool.raw_input,
      "old_string",
      "oldString",
      "before",
      "oldText",
      "new_string",
      "newString",
      "after",
      "newText",
      "content",
      "new_content",
      "newContent",
      "replacement",
    );
  }

  const path = rawInputFilePath(tool);
  if (!path) return false;

  return (
    stringField(input, "old_string", "oldString", "before", "oldText") != null ||
    stringField(input, "new_string", "newString", "after", "newText") != null ||
    stringField(input, "content", "new_content", "newContent", "replacement") != null
  );
}

export function isCodeBuddySkillTool(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  const input = parseJsonValue(tool.raw_input);
  if (!input || typeof input !== "object" || Array.isArray(input)) return false;
  const skill = stringField(input, "skill");
  return skill != null && skill.trim().length > 0;
}

export function rawInputFilePath(tool: ToolInvocation): string | null {
  if (!tool.raw_input) return null;
  const input = parseJsonValue(tool.raw_input);
  return (
    stringField(input, "file_path", "filePath", "path") ??
    stringFieldFromRawText(tool.raw_input, "file_path", "filePath", "path")
  );
}

export function rawInputHasReadOnlyParsedCommand(tool: ToolInvocation): boolean {
  const entries = parsedCommandEntriesFromRawPayload(tool.raw_input);
  return entries.length > 0 && entries.every(parsedCommandEntryIsReadOnly);
}

export function parsedCommandTarget(tool: ToolInvocation): string | null {
  for (const raw of [tool.raw_input, tool.raw_output]) {
    for (const entry of parsedCommandEntriesFromRawPayload(raw)) {
      const target = stringField(
        entry,
        "path",
        "file_path",
        "filePath",
        "target",
        "target_path",
        "targetPath",
      );
      if (target && looksLikePath(target)) return target;
    }
  }
  return null;
}

export function parsedCommandEntriesFromRawPayload(
  raw: string | null | undefined,
): Record<string, unknown>[] {
  if (!raw) return [];
  const input = parseJsonValue(raw);
  if (!input || typeof input !== "object" || Array.isArray(input)) return [];
  const parsedCmd = (input as Record<string, unknown>).parsed_cmd;
  if (Array.isArray(parsedCmd)) {
    return parsedCmd.filter(
      (entry): entry is Record<string, unknown> =>
        !!entry && typeof entry === "object" && !Array.isArray(entry),
    );
  }
  if (parsedCmd && typeof parsedCmd === "object") {
    return [parsedCmd as Record<string, unknown>];
  }
  return [];
}

export function parsedCommandEntryIsReadOnly(entry: Record<string, unknown>): boolean {
  const type = stringField(entry, "type", "kind")?.toLowerCase();
  return !!type && [
    "read",
    "search",
    "list",
    "glob",
    "inspect",
    "stat",
  ].includes(type);
}

export function isTodoWriteTool(tool: ToolInvocation): boolean {
  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  if (
    identity.includes("todo write") ||
    identity.includes("todowrite") ||
    identity.includes("todo: todo write")
  ) {
    return true;
  }

  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input);
    return (
      typeof input.content === "string" &&
      /(?:^|\n)\s*[-*]\s+\[[^\]]*\]\s+\S/.test(input.content)
    );
  } catch {
    return false;
  }
}

export function getTrackedDiffPaths(tool: ToolInvocation, commandEditPaths: string[] = []): string[] {
  if (
    !isExplicitEditToolInvocation(tool) &&
    !rawInputHasEditPayload(tool) &&
    commandEditPaths.length === 0
  ) {
    return [];
  }
  if (commandEditPaths.length > 0) {
    const diffPaths = uniqueStrings([
      ...tool.diff_paths,
      ...tool.diff_previews.map((preview) => preview.path),
      ...diffPreviewsFromRawOutput(tool.raw_output).map((preview) => preview.path),
    ]);
    return uniqueStrings([
      ...commandEditPaths,
      ...diffPaths.filter((path) =>
        commandEditPaths.some((editPath) => sameOrNestedPath(editPath, path)),
      ),
    ]);
  }
  return uniqueStrings([
    ...commandEditPaths,
    ...tool.diff_paths,
    ...diffPreviewsFromRawOutput(tool.raw_output).map((preview) => preview.path),
    ...(rawInputFilePath(tool) ? [rawInputFilePath(tool)!] : []),
    ...(pathFromToolLogs(tool) ? [pathFromToolLogs(tool)!] : []),
  ]);
}

export function getTrackedDiffPreviews(tool: ToolInvocation, commandEditPaths: string[] = []): ToolDiffPreview[] {
  if (
    !isExplicitEditToolInvocation(tool) &&
    !rawInputHasEditPayload(tool) &&
    commandEditPaths.length === 0
  ) return [];
  const previews = (tool.diff_previews ?? []).filter((preview) => !looksLikeBogusWholeFilePreview(preview));
  if (commandEditPaths.length > 0) {
    const matched = previews.filter((preview) =>
      commandEditPaths.some((path) => sameOrNestedPath(path, preview.path)),
    );
    return matched;
  }
  if (previews.length > 0) return previews;
  const inputPreview = diffPreviewFromRawInput(tool);
  if (inputPreview) return [inputPreview];
  return diffPreviewsFromRawOutput(tool.raw_output);
}

export function getCommandMutationDiffPaths(tool: ToolInvocation, command: string | null): string[] {
  if (!command) return [];
  const applyPatchPaths = pathsFromApplyPatchCommand(command);
  if (applyPatchPaths.length > 0) {
    return applyPatchPaths;
  }

  const writePaths = commandWritePaths(command);
  if (writePaths.length > 0) {
    return writePaths.map(displayPath);
  }

  const pathspecs = gitWorkingTreeMutationPathspecs(command);
  if (pathspecs.length === 0) return [];

  const changedPaths = uniqueStrings([
    ...tool.diff_paths.map(String),
    ...(tool.diff_previews ?? []).map((preview) => preview.path),
  ]);
  if (changedPaths.length === 0) return [];

  return changedPaths.filter((changedPath) =>
    pathspecs.some((pathspec) => sameOrNestedPath(pathspec, changedPath)),
  );
}

export function pathsFromApplyPatchCommand(command: string): string[] {
  return diffPreviewsFromApplyPatchCommand(command).map((preview) => displayPath(preview.path));
}

export function diffPreviewsFromApplyPatchCommand(command: string | null): ToolDiffPreview[] {
  const patch = extractApplyPatchText(command);
  if (!patch) return [];
  return diffPreviewsFromApplyPatchText(patch);
}

export function extractApplyPatchText(command: string | null): string | null {
  if (!command || !command.includes("apply_patch")) return null;
  const start = command.indexOf("*** Begin Patch");
  if (start < 0) return null;
  const endMarker = "*** End Patch";
  const end = command.indexOf(endMarker, start);
  if (end < 0) return null;
  return command.slice(start, end + endMarker.length);
}

export function diffPreviewsFromApplyPatchText(patch: string): ToolDiffPreview[] {
  const lines = patch.replace(/\r\n/g, "\n").split("\n");
  const previews: ToolDiffPreview[] = [];
  let current: ToolDiffPreview | null = null;
  let currentHunk: DiffHunk | null = null;
  let addedFileLines: DiffHunk["lines"] | null = null;

  const flushAddedFile = () => {
    if (!current || !addedFileLines) return;
    current.hunks.push({
      heading: `@@ -0,0 +1,${Math.max(addedFileLines.length, 1)} @@`,
      lines: addedFileLines,
    });
    addedFileLines = null;
  };
  const startFile = (path: string) => {
    flushAddedFile();
    const preview: ToolDiffPreview = { path: displayPath(path), hunks: [] };
    current = preview;
    previews.push(preview);
    currentHunk = null;
    return preview;
  };

  for (const line of lines) {
    const fileMatch = line.match(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/);
    if (fileMatch) {
      const activePreview = startFile(fileMatch[1]);
      addedFileLines = line.includes("*** Add File:") ? [] : null;
      if (line.includes("*** Delete File:")) {
        activePreview.hunks.push({
          heading: "@@ -1 +0,0 @@",
          lines: [{ kind: "Removed", content: "[file deleted]" }],
        });
      }
      continue;
    }

    if (!current || line.startsWith("*** Begin Patch") || line.startsWith("*** End Patch")) {
      continue;
    }
    const activePreview = current as ToolDiffPreview;

    if (addedFileLines) {
      if (line.startsWith("+")) {
        addedFileLines.push({ kind: "Added", content: line.slice(1) });
      }
      continue;
    }

    if (line.startsWith("@@")) {
      currentHunk = { heading: line, lines: [] };
      activePreview.hunks.push(currentHunk);
      continue;
    }

    if (!currentHunk) {
      currentHunk = { heading: "@@", lines: [] };
      activePreview.hunks.push(currentHunk);
    }

    if (line.startsWith("+")) {
      currentHunk.lines.push({ kind: "Added", content: line.slice(1) });
    } else if (line.startsWith("-")) {
      currentHunk.lines.push({ kind: "Removed", content: line.slice(1) });
    } else if (line.startsWith(" ")) {
      currentHunk.lines.push({ kind: "Context", content: line.slice(1) });
    }
  }

  flushAddedFile();
  return previews.filter((preview) => preview.hunks.some((hunk) => hunk.lines.length > 0));
}

export function diffPreviewsFromRawOutput(rawOutput: string | null | undefined): ToolDiffPreview[] {
  if (!rawOutput) return [];
  const parsed = parseJsonValue(rawOutput);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return [];
  const changes = (parsed as Record<string, unknown>).changes;
  if (!changes || typeof changes !== "object" || Array.isArray(changes)) return [];

  return Object.entries(changes as Record<string, unknown>)
    .map(([path, change]) => {
      if (!change || typeof change !== "object" || Array.isArray(change)) return null;
      const unifiedDiff = stringField(change, "unified_diff", "unifiedDiff", "diff");
      if (!unifiedDiff) return null;
      const preview = diffPreviewFromUnifiedDiff(path, unifiedDiff);
      return preview.hunks.length > 0 ? preview : null;
    })
    .filter((preview): preview is ToolDiffPreview => preview != null);
}

export function diffPreviewFromUnifiedDiff(path: string, unifiedDiff: string): ToolDiffPreview {
  const preview: ToolDiffPreview = { path: displayPath(path), hunks: [] };
  let currentHunk: DiffHunk | null = null;

  for (const line of unifiedDiff.replace(/\r\n/g, "\n").split("\n")) {
    if (line.startsWith("diff --git ")) continue;
    if (line.startsWith("--- ") || line.startsWith("+++ ")) continue;
    if (line.startsWith("\\ No newline")) continue;

    if (line.startsWith("@@")) {
      currentHunk = { heading: line, lines: [] };
      preview.hunks.push(currentHunk);
      continue;
    }
    if (!currentHunk) continue;

    if (line.startsWith("+")) {
      currentHunk.lines.push({ kind: "Added", content: line.slice(1) });
    } else if (line.startsWith("-")) {
      currentHunk.lines.push({ kind: "Removed", content: line.slice(1) });
    } else if (line.startsWith(" ")) {
      currentHunk.lines.push({ kind: "Context", content: line.slice(1) });
    }
  }

  preview.hunks = preview.hunks.filter((hunk) => hunk.lines.length > 0);
  return preview;
}

export function commandWritePaths(command: string): string[] {
  const stripped = stripShellHereDocuments(stripPowerShellHereStrings(command));
  const paths: string[] = [];
  for (const segment of stripped.split(/[;\n]/)) {
    const lower = segment.toLowerCase();
    if (containsCommandToken(lower, "set-content") || containsCommandToken(lower, "add-content")) {
      paths.push(...extractParamValues(segment, ["-literalpath", "-filepath", "-path"]));
    } else if (containsCommandToken(lower, "out-file")) {
      paths.push(...extractParamValues(segment, ["-filepath", "-path"]));
    } else if (
      containsCommandToken(lower, "new-item") &&
      hasParamValue(lower, "-itemtype", "file")
    ) {
      paths.push(...extractParamValues(segment, ["-literalpath", "-path"]));
    }
  }
  collectShellRedirectionPaths(stripped, paths);
  return uniqueStrings(paths.filter(isUsableWritePath));
}

export function filterCompletedCommandEditPaths(
  tool: ToolInvocation,
  command: string | null,
  paths: string[],
): string[] {
  if (!command || paths.length === 0 || !isTerminalToolStatus(tool.status)) {
    return paths;
  }

  const cleanupPaths = commandCleanupPaths(command);
  if (cleanupPaths.length === 0) return paths;
  const cwd = commandWorkingDirectory(tool);

  const diffPaths = uniqueStrings([
    ...tool.diff_paths,
    ...tool.diff_previews.map((preview) => preview.path),
    ...diffPreviewsFromRawOutput(tool.raw_output).map((preview) => preview.path),
  ]);

  return paths.filter((path) => {
    if (diffPaths.some((diffPath) => pathsReferToSameTarget(path, diffPath, cwd))) {
      return true;
    }
    return !cleanupPaths.some((cleanupPath) => pathsReferToSameTarget(path, cleanupPath, cwd));
  });
}

export function isTerminalToolStatus(status: ToolStatus): boolean {
  return status === "Succeeded" || status === "Failed" || status === "Interrupted";
}

export function commandCleanupPaths(command: string): string[] {
  const stripped = stripShellHereDocuments(stripPowerShellHereStrings(command));
  const paths: string[] = [];
  for (const segment of stripped.split(/[;\n]/)) {
    const tokens = tokenizeCommandLine(segment);
    if (tokens.length === 0) continue;
    const commandName = tokens[0].toLowerCase();
    if (commandName === "rm" || commandName === "unlink" || commandName === "del" || commandName === "erase") {
      paths.push(...shellRemoveCommandPaths(tokens.slice(1)));
      continue;
    }
    const lower = segment.toLowerCase();
    if (containsCommandToken(lower, "remove-item") || containsCommandToken(lower, "ri")) {
      paths.push(...extractParamValues(segment, ["-literalpath", "-path"]));
      const positional = tokens
        .slice(1)
        .filter((token) => !token.startsWith("-") && token !== "|" && token !== "&&" && token !== "||");
      if (positional.length > 0) {
        paths.push(positional[0]);
      }
    }
  }
  return uniqueStrings(paths.map(displayPath).filter(isUsableWritePath));
}

export function shellRemoveCommandPaths(tokens: string[]): string[] {
  const paths: string[] = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === "--") continue;
    if (token === "|" || token === "&&" || token === "||") break;
    if (token.startsWith("-")) continue;
    if (isUsableWritePath(token)) paths.push(token);
  }
  return paths;
}

export function commandWorkingDirectory(tool: ToolInvocation): string | null {
  for (const raw of [tool.raw_input, tool.raw_output]) {
    if (!raw) continue;
    const parsed = parseJsonValue(raw);
    const cwd = stringField(parsed, "cwd", "working_directory", "workingDirectory");
    if (cwd && looksLikePath(cwd)) return displayPath(cwd);
  }
  return null;
}

interface ShellHereDocMarker {
  delimiter: string;
  stripTabs: boolean;
}

export function stripShellHereDocuments(command: string): string {
  const lines = command.replace(/\r\n/g, "\n").split("\n");
  const output: string[] = [];
  const pending: ShellHereDocMarker[] = [];

  for (const line of lines) {
    if (pending.length === 0) {
      output.push(line);
      pending.push(...extractShellHereDocMarkers(line));
      continue;
    }

    const active = pending[0];
    const comparable = active.stripTabs ? line.replace(/^\t+/, "") : line;
    if (comparable === active.delimiter) {
      pending.shift();
    }
  }

  return output.join("\n");
}

export function extractShellHereDocMarkers(line: string): ShellHereDocMarker[] {
  const markers: ShellHereDocMarker[] = [];
  let index = 0;
  while (index < line.length) {
    const markerIndex = line.indexOf("<<", index);
    if (markerIndex < 0) break;
    if (line[markerIndex + 2] === "<") {
      index = markerIndex + 3;
      continue;
    }

    let cursor = markerIndex + 2;
    const stripTabs = line[cursor] === "-";
    if (stripTabs) cursor += 1;
    while (cursor < line.length && /\s/.test(line[cursor])) cursor += 1;

    const parsed = parseShellHereDocDelimiter(line, cursor);
    if (parsed) {
      markers.push({ delimiter: parsed.delimiter, stripTabs });
      index = parsed.end;
    } else {
      index = cursor + 1;
    }
  }
  return markers;
}

export function parseShellHereDocDelimiter(
  line: string,
  start: number,
): { delimiter: string; end: number } | null {
  if (start >= line.length) return null;
  const quote = line[start];
  if (quote === "'" || quote === '"') {
    const end = line.indexOf(quote, start + 1);
    if (end < 0) return null;
    const delimiter = line.slice(start + 1, end).trim();
    return delimiter ? { delimiter, end: end + 1 } : null;
  }

  let end = start;
  while (end < line.length && !/[\s;|&<>]/.test(line[end])) {
    end += 1;
  }
  const delimiter = line.slice(start, end).replace(/\\/g, "").trim();
  return delimiter ? { delimiter, end } : null;
}

export function stripPowerShellHereStrings(command: string): string {
  let output = "";
  let index = 0;
  while (index < command.length) {
    const marker = command.startsWith('@"', index)
      ? '"'
      : command.startsWith("@'", index)
        ? "'"
        : null;
    if (!marker) {
      output += command[index];
      index += 1;
      continue;
    }

    index += 2;
    const lfMarker = `\n${marker}@`;
    const crlfMarker = `\r\n${marker}@`;
    const lfIndex = command.indexOf(lfMarker, index);
    const crlfIndex = command.indexOf(crlfMarker, index);
    const end =
      lfIndex >= 0 && crlfIndex >= 0
        ? Math.min(lfIndex, crlfIndex)
        : lfIndex >= 0
          ? lfIndex
          : crlfIndex >= 0
            ? crlfIndex
            : -1;
    if (end < 0) break;
    index = end;
    if (command.startsWith(crlfMarker, index)) {
      index += crlfMarker.length;
    } else {
      index += lfMarker.length;
    }
    output += " ";
  }
  return output;
}

export function containsCommandToken(text: string, token: string): boolean {
  let offset = 0;
  while (offset < text.length) {
    const index = text.indexOf(token, offset);
    if (index < 0) return false;
    const before = text[index - 1];
    const after = text[index + token.length];
    const beforeOk = !before || !isCommandWordChar(before);
    const afterOk = !after || !isCommandWordChar(after);
    if (beforeOk && afterOk) return true;
    offset = index + token.length;
  }
  return false;
}

export function isCommandWordChar(char: string): boolean {
  return /[a-z0-9_-]/i.test(char);
}

export function hasParamValue(segmentLower: string, param: string, expected: string): boolean {
  return extractParamValues(segmentLower, [param]).some(
    (value) => value.toLowerCase() === expected.toLowerCase(),
  );
}

export function extractParamValues(segment: string, params: string[]): string[] {
  const lower = segment.toLowerCase();
  const values: string[] = [];
  for (const param of params) {
    let offset = 0;
    while (offset < lower.length) {
      const index = lower.indexOf(param, offset);
      if (index < 0) break;
      const before = lower[index - 1];
      const after = lower[index + param.length];
      const beforeOk = !before || /\s|\|/.test(before);
      const afterOk = !after || /\s|:/.test(after);
      if (beforeOk && afterOk) {
        const value = parseCommandValueAt(segment, index + param.length);
        if (value) values.push(value);
      }
      offset = index + param.length;
    }
  }
  return values;
}

export function parseCommandValueAt(text: string, start: number): string | null {
  let index = start;
  while (index < text.length && (/[\s:]/.test(text[index]))) {
    index += 1;
  }
  if (index >= text.length) return null;

  const quote = text[index];
  if (quote === '"' || quote === "'") {
    const end = text.indexOf(quote, index + 1);
    return end >= 0 ? text.slice(index + 1, end) : text.slice(index + 1);
  }

  let end = index;
  while (end < text.length && !/[\s;|)]/.test(text[end])) {
    end += 1;
  }
  return text.slice(index, end);
}

export function collectShellRedirectionPaths(command: string, paths: string[]) {
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < command.length; i += 1) {
    const char = command[i];
    const next = command[i + 1];

    if (char === "\\" && quote === '"' && next) {
      i += 1;
      continue;
    }

    if (char === '"' || char === "'") {
      if (quote === char) {
        quote = null;
        continue;
      }
      if (!quote) {
        quote = char;
        continue;
      }
    }

    if (quote || char !== ">") continue;

    if (i > 0 && /\d/.test(command[i - 1])) continue;
    if (next === "&") continue;
    if (command[i + 1] === ">") i += 1;
    const value = parseCommandValueAt(command, i + 1);
    if (value) paths.push(value);
  }
}

export function isUsableWritePath(path: string): boolean {
  const trimmed = path.trim();
  if (!trimmed || /[\r\n]/.test(trimmed)) return false;
  if (/[<>]/.test(trimmed)) return false;
  if (/^file:\/\//i.test(trimmed)) return false;
  if (/^[a-zA-Z]:[\\/]{2,}/.test(trimmed)) return false;
  if (/^[$({]/.test(trimmed)) return false;
  if (trimmed === "/" || looksLikePureTraversalPath(trimmed)) return false;
  return !["$null", "null", "nul", "/dev/null"].includes(trimmed.toLowerCase());
}

export function looksLikePureTraversalPath(path: string): boolean {
  const normalized = displayPath(path).replace(/[)"']+$/g, "");
  return /^\.{1,2}(?:\/\.{1,2})*$/.test(normalized);
}

export function gitWorkingTreeMutationPathspecs(command: string): string[] {
  const tokens = tokenizeCommandLine(command);
  const segments = splitCommandSegments(tokens);
  const pathspecs: string[] = [];

  for (const segment of segments) {
    const gitIndex = segment.findIndex(isGitExecutableToken);
    if (gitIndex < 0) continue;
    const subcommand = segment[gitIndex + 1]?.toLowerCase();
    const args = segment.slice(gitIndex + 2);

    if (subcommand === "checkout") {
      pathspecs.push(...pathspecsAfterDoubleDash(args));
    } else if (subcommand === "restore") {
      pathspecs.push(...restorePathspecs(args));
    }
  }

  return uniqueStrings(pathspecs.map(displayPath).filter(Boolean));
}

export function splitCommandSegments(tokens: string[]): string[][] {
  const segments: string[][] = [];
  let current: string[] = [];
  for (const token of tokens) {
    if (token === "&&" || token === "||" || token === ";" || token === "|") {
      if (current.length > 0) segments.push(current);
      current = [];
      continue;
    }
    current.push(token);
  }
  if (current.length > 0) segments.push(current);
  return segments;
}

export function isGitExecutableToken(token: string): boolean {
  const base = displayPath(token).split("/").pop()?.toLowerCase() ?? token.toLowerCase();
  return base === "git" || base === "git.exe";
}

export function pathspecsAfterDoubleDash(args: string[]): string[] {
  const separatorIndex = args.indexOf("--");
  if (separatorIndex < 0) return [];
  return args.slice(separatorIndex + 1).filter(isLikelyPathspec);
}

export function restorePathspecs(args: string[]): string[] {
  const afterSeparator = pathspecsAfterDoubleDash(args);
  if (afterSeparator.length > 0) return afterSeparator;

  const pathspecs: string[] = [];
  for (let i = 0; i < args.length; i += 1) {
    const token = args[i];
    if (token.startsWith("-")) {
      if (gitRestoreOptionTakesValue(token) && i + 1 < args.length) {
        i += 1;
      }
      continue;
    }
    if (isLikelyPathspec(token)) {
      pathspecs.push(token);
    }
  }
  return pathspecs;
}

export function gitRestoreOptionTakesValue(token: string): boolean {
  const lower = token.toLowerCase();
  return lower === "-s" || lower === "--source" || lower === "--pathspec-from-file";
}

export function isLikelyPathspec(token: string): boolean {
  const path = displayPath(token);
  return !!path && path !== "--" && !path.startsWith("-");
}

export function sameOrNestedPath(pathspec: string, changedPath: string): boolean {
  const spec = normalizePathForCompare(pathspec);
  const changed = normalizePathForCompare(changedPath);
  if (!spec || !changed) return false;
  if (spec === "." || spec === "*") return true;
  if (changed === spec) return true;
  if (changed.endsWith(`/${spec}`)) return true;
  return spec.endsWith("/") && (changed.startsWith(spec) || changed.endsWith(`/${spec}`));
}

export function pathsReferToSameTarget(left: string, right: string, cwd: string | null = null): boolean {
  const normalizedLeft = normalizePathForCompare(resolvePathAgainstCwd(left, cwd));
  const normalizedRight = normalizePathForCompare(resolvePathAgainstCwd(right, cwd));
  return !!normalizedLeft && normalizedLeft === normalizedRight;
}

export function normalizePathForCompare(path: string): string {
  return normalizePathSegments(displayPath(path))
    .replace(/^[a-zA-Z]:\//, "")
    .replace(/^\.\//, "")
    .replace(/\/+$/g, "")
    .toLowerCase();
}

export function resolvePathAgainstCwd(path: string, cwd: string | null): string {
  const displayed = displayPath(path);
  if (!cwd || isAbsoluteDisplayPath(displayed)) return displayed;
  return normalizePathSegments(`${displayPath(cwd).replace(/\/+$/g, "")}/${displayed}`);
}

export function isAbsoluteDisplayPath(path: string): boolean {
  return path.startsWith("/") || /^[a-zA-Z]:\//.test(path);
}

export function normalizePathSegments(path: string): string {
  const displayed = displayPath(path);
  const drive = displayed.match(/^[a-zA-Z]:\//)?.[0] ?? "";
  const absolute = displayed.startsWith("/");
  const rest = drive ? displayed.slice(drive.length) : absolute ? displayed.slice(1) : displayed;
  const segments: string[] = [];
  for (const part of rest.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (segments.length > 0 && segments[segments.length - 1] !== "..") {
        segments.pop();
      } else if (!absolute && !drive) {
        segments.push(part);
      }
      continue;
    }
    segments.push(part);
  }
  const prefix = drive || (absolute ? "/" : "");
  return `${prefix}${segments.join("/")}`;
}

export function looksLikeBogusWholeFilePreview(preview: ToolDiffPreview): boolean {
  const stats = getDiffStats([preview]);
  if (!(stats.added >= 100 && (stats.removed === 0 || stats.added > stats.removed * 4))) {
    return false;
  }
  return preview.hunks.some((hunk) => hunkRangeLooksLikeFragmentToWholeFile(hunk.heading));
}

function hunkRangeLooksLikeFragmentToWholeFile(heading: string): boolean {
  const range = parseUnifiedHunkRange(heading);
  if (!range) return false;
  return range.oldCount > 0 && range.newCount >= 100 && range.newCount > range.oldCount * 4;
}

function parseUnifiedHunkRange(heading: string): { oldCount: number; newCount: number } | null {
  const match = heading.match(/^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/);
  if (!match) return null;
  const oldCount = match[2] == null ? 1 : Number(match[2]);
  const newCount = match[4] == null ? 1 : Number(match[4]);
  if (!Number.isFinite(oldCount) || !Number.isFinite(newCount)) return null;
  return { oldCount, newCount };
}

export function diffPreviewFromRawInput(tool: ToolInvocation): ToolDiffPreview | null {
  if (!tool.raw_input) return null;
  try {
    const input = JSON.parse(tool.raw_input);
    const oldText = stringField(input, "old_string", "oldString", "before", "oldText");
    const newText = stringField(input, "new_string", "newString", "after", "newText");
    const path = stringField(input, "file_path", "filePath", "path") ?? tool.diff_paths[0] ?? tool.name;
    if (oldText == null || newText == null || oldText === newText) return null;
    if (looksLikeFragmentToWholeFile(oldText, newText)) return null;
    return {
      path,
      hunks: compactTextDiffToHunks(oldText, newText),
    };
  } catch {
    return null;
  }
}

export function stringField(input: unknown, ...keys: string[]): string | null {
  if (!input || typeof input !== "object") return null;
  const record = input as Record<string, unknown>;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string") return value;
  }
  return null;
}

export function stringFieldFromRawText(raw: string, ...keys: string[]): string | null {
  for (const key of keys) {
    const escapedKey = escapeRegExp(key);
    const match = raw.match(new RegExp(`"(${escapedKey})"\\s*:\\s*"((?:\\\\.|[^"\\\\])*)"`));
    if (!match) continue;
    return decodeJsonStringFragment(match[2]);
  }
  return null;
}

export function rawTextHasAnyKey(raw: string, ...keys: string[]): boolean {
  return keys.some((key) =>
    new RegExp(`"${escapeRegExp(key)}"\\s*:`).test(raw),
  );
}

export function decodeJsonStringFragment(value: string): string {
  try {
    return JSON.parse(`"${value}"`);
  } catch {
    return value
      .replace(/\\"/g, '"')
      .replace(/\\\\/g, "\\")
      .replace(/\\r\\n/g, "\n")
      .replace(/\\n/g, "\n")
      .replace(/\\r/g, "\n");
  }
}

export function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function looksLikeFragmentToWholeFile(oldText: string, newText: string): boolean {
  const oldLines = oldText.split(/\r?\n/).length;
  const newLines = newText.split(/\r?\n/).length;
  return oldLines > 0 && newLines >= 100 && oldLines * 4 < newLines;
}

export function compactTextDiffToHunks(oldText: string, newText: string): DiffHunk[] {
  const oldLines = oldText.split(/\r?\n/);
  const newLines = newText.split(/\r?\n/);
  let prefix = 0;
  while (
    prefix < oldLines.length &&
    prefix < newLines.length &&
    oldLines[prefix] === newLines[prefix]
  ) {
    prefix += 1;
  }

  let suffix = 0;
  while (
    suffix + prefix < oldLines.length &&
    suffix + prefix < newLines.length &&
    oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const start = Math.max(0, prefix - DIFF_CONTEXT_LINES);
  const oldChangeEnd = oldLines.length - suffix;
  const newChangeEnd = newLines.length - suffix;
  const oldEnd = Math.min(oldLines.length, oldChangeEnd + DIFF_CONTEXT_LINES);
  const newEnd = Math.min(newLines.length, newChangeEnd + DIFF_CONTEXT_LINES);
  const lines: DiffHunk["lines"] = [];

  for (let i = start; i < prefix; i += 1) {
    lines.push({ kind: "Context", content: oldLines[i] });
  }
  for (let i = prefix; i < oldChangeEnd; i += 1) {
    lines.push({ kind: "Removed", content: oldLines[i] });
  }
  for (let i = prefix; i < newChangeEnd; i += 1) {
    lines.push({ kind: "Added", content: newLines[i] });
  }
  for (let i = Math.max(prefix, oldChangeEnd); i < oldEnd; i += 1) {
    lines.push({ kind: "Context", content: oldLines[i] });
  }

  if (!lines.some((line) => line.kind === "Added" || line.kind === "Removed")) {
    return [];
  }

  const oldCount = oldEnd - start;
  const newCount = newEnd - start;
  return [
    {
      heading: `@@ -${formatPatchRange(start + 1, oldCount)} +${formatPatchRange(start + 1, newCount)} @@`,
      lines,
    },
  ];
}

export function getSubagentType(tool: ToolInvocation): string | null {
  if (!tool.is_subagent || !tool.raw_input) return null;
  try {
    const input = JSON.parse(tool.raw_input);
    return typeof input.subagent_type === "string"
      ? input.subagent_type.toLowerCase()
      : null;
  } catch {
    return null;
  }
}

export function normalizeComparableText(text: string | null | undefined): string {
  return (text ?? "").replace(/\r\n/g, "\n").trim();
}

export function getDetailLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  const detail = normalizeComparableText(tool.detail_text);
  if (!detail) return { lines: [], omitted: 0 };

  const allLines = detail.split("\n");
  if (allLines.length <= MAX_OUTPUT_LINES) {
    return { lines: allLines, omitted: 0 };
  }

  const tail = allLines.slice(-MAX_OUTPUT_LINES);
  return { lines: tail, omitted: allLines.length - MAX_OUTPUT_LINES };
}

export function getVisibleLogEntries(tool: ToolInvocation): {
  entries: ToolInvocation["logs"];
  omitted: number;
} {
  const detail = normalizeComparableText(tool.detail_text);
  const rawOutput = normalizeComparableText(tool.raw_output);
  const entries = tool.logs.filter((entry) => {
    const body = normalizeComparableText(entry.body);
    if (!body) return false;
    if (detail && body === detail) return false;
    if (rawOutput && body === rawOutput) return false;
    return true;
  });

  if (entries.length <= MAX_OUTPUT_LINES) {
    return { entries, omitted: 0 };
  }

  const tail = entries.slice(-MAX_OUTPUT_LINES);
  return { entries: tail, omitted: entries.length - MAX_OUTPUT_LINES };
}

export function isExploreTool(tool: ToolInvocation, lower: string): boolean {
  if (
    lower.includes("read") ||
    lower.includes("view") ||
    lower.includes("open") ||
    lower.includes("search") ||
    lower.includes("list") ||
    lower.includes("glob") ||
    lower.includes("grep") ||
    lower.includes("find") ||
    lower.includes("webfetch") ||
    lower.includes("fetch")
  ) {
    return true;
  }
  return rawInputHasPath(tool);
}

export function rawInputHasPath(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input);
    return !!(input.file_path || input.filePath || input.path);
  } catch {
    return looksLikePath(tool.raw_input) && !looksLikeCommand(tool.raw_input);
  }
}

export function rawInputHasCommand(tool: ToolInvocation): boolean {
  if (!tool.raw_input) return false;
  try {
    const input = JSON.parse(tool.raw_input);
    return typeof input.command === "string" && input.command.trim().length > 0;
  } catch {
    return looksLikeCommand(tool.raw_input);
  }
}

export function isCommandTool(tool: ToolInvocation): boolean {
  const kind = tool.kind.trim().toLowerCase();
  const name = tool.name.trim().toLowerCase();
  return (
    tool.name.trim().startsWith("`") ||
    kind === "bash" ||
    name === "bash" ||
    (kind === "execute" && rawInputHasCommand(tool)) ||
    kind === "command" ||
    kind === "terminal" ||
    name === "command" ||
    name === "terminal"
  );
}

export function commandToolLabel(tool: ToolInvocation): string {
  const name = tool.name.trim();
  const kind = tool.kind.trim();
  if (name && !name.startsWith("`") && !isGenericTitle(name)) {
    return truncate(name, 80);
  }
  if (kind && !isGenericTitle(kind) && kind.toLowerCase() !== "execute") {
    return truncate(kind, 80);
  }
  return "Command";
}

export function toolVerb(status: ToolStatus, category: ToolCategory): string {
  if (status === "Failed") return "失败";
  if (status === "Interrupted") return "已中断";

  const running = status === "Running" || status === "Pending";
  switch (category) {
    case "exploring":
      return running ? "探索中" : "已探索";
    case "editing":
      return running ? "编辑中" : "已编辑";
    case "asking":
      return running ? "提问中" : "已提问";
    case "executing":
      return running ? "运行中" : "已运行";
  }
}

export function isQuestionTool(tool: ToolInvocation): boolean {
  if ((tool.permission_input?.questions.length ?? 0) > 0) {
    return true;
  }

  const identity = `${tool.kind} ${tool.name}`.toLowerCase();
  if (
    identity.includes("askuserquestion") ||
    identity.includes("request_user_input") ||
    identity.includes("request user input")
  ) {
    return true;
  }

  const rawInput = tool.raw_input ? parseJsonValue(tool.raw_input) : null;
  return (
    !!rawInput &&
    typeof rawInput === "object" &&
    !Array.isArray(rawInput) &&
    Array.isArray((rawInput as Record<string, unknown>).questions)
  );
}

function questionToolAnswerTitle(tool: ToolInvocation): string | null {
  return (
    answerFromPermissionDecision(tool.permission_decision) ??
    answerFromPermissionDecision(tool.summary)
  );
}

function answerFromPermissionDecision(value: string | null | undefined): string | null {
  if (!value) return null;
  const match = value.trim().match(/^Permission selected:\s*(.+)$/i);
  const answer = match?.[1]?.trim();
  if (!answer || isGenericPermissionAnswer(answer)) return null;
  return truncate(answer, 80);
}

function isGenericPermissionAnswer(answer: string): boolean {
  const normalized = answer.trim().toLowerCase();
  return [
    "allow",
    "allow all",
    "allow once",
    "approve",
    "approved",
    "yes",
    "reject",
    "deny",
    "denied",
    "no",
    "cancel",
    "cancelled",
    "canceled",
  ].includes(normalized);
}

export function statusBullet(
  status: ToolStatus
): { char: string; className: string } {
  switch (status) {
    case "Pending":
    case "Running":
      return { char: "•", className: "tc-bullet-active" };
    case "Succeeded":
      return { char: "•", className: "tc-bullet-ok" };
    case "Failed":
      return { char: "•", className: "tc-bullet-err" };
    case "Interrupted":
      return { char: "•", className: "tc-bullet-warn" };
  }
}

export function getOutputLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  if (tool.terminal_output) {
    const raw = tool.terminal_output.output.trim();
    if (!raw) {
      const code = tool.terminal_output.exit_code;
      if (code !== null && code !== 0) {
        return { lines: [`(退出码 ${code})`], omitted: 0 };
      }
      return { lines: [], omitted: 0 };
    }
    const allLines = raw.split("\n");
    if (allLines.length <= MAX_OUTPUT_LINES) {
      return { lines: allLines, omitted: 0 };
    }
    const head = allLines.slice(0, MAX_OUTPUT_LINES);
    return { lines: head, omitted: allLines.length - MAX_OUTPUT_LINES };
  }

  return { lines: [], omitted: 0 };
}

/** Get displayable lines from raw_output (for non-terminal tools) */
export function getRawOutputLines(tool: ToolInvocation): {
  lines: string[];
  omitted: number;
} {
  // Skip if terminal_output exists (handled by getOutputLines)
  if (tool.terminal_output) return { lines: [], omitted: 0 };

  const raw = tool.raw_output?.trim();
  if (!raw) return { lines: [], omitted: 0 };
  const normalizedRaw = normalizeComparableText(raw);
  if (normalizedRaw === normalizeComparableText(tool.detail_text)) {
    return { lines: [], omitted: 0 };
  }
  if (tool.logs.some((entry) => normalizeComparableText(entry.body) === normalizedRaw)) {
    return { lines: [], omitted: 0 };
  }

  // Skip vague/unhelpful outputs
  if (isVagueError(raw)) return { lines: [], omitted: 0 };
  if (diffPreviewsFromRawOutput(raw).length > 0) return { lines: [], omitted: 0 };
  // Skip outputs that just repeat the summary
  if (raw === tool.summary) return { lines: [], omitted: 0 };
  // Skip very short outputs that add no value (like "Completed", "OK")
  if (raw.length < 10 && !raw.includes("\n")) return { lines: [], omitted: 0 };

  const allLines = raw.split("\n");
  if (allLines.length <= MAX_OUTPUT_LINES) {
    return { lines: allLines, omitted: 0 };
  }
  const head = allLines.slice(0, MAX_OUTPUT_LINES);
  return { lines: head, omitted: allLines.length - MAX_OUTPUT_LINES };
}

/** Returns true for vague/unhelpful server errors that add no value when displayed */
export function isVagueError(error: string): boolean {
  const lower = error.toLowerCase().trim();
  return (
    lower === "internal error" ||
    lower === "error" ||
    lower === "failed" ||
    lower === "tool call failed" ||
    lower === "tool failed" ||
    lower === "unknown error" ||
    lower.startsWith("internal error (tool:")
  );
}
