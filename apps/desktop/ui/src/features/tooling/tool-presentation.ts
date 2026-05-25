import type { ToolInvocation, ToolStatus } from "../../types";

export type ToolPresentationKind = "command" | "permission" | "diff" | "generic";
export type ToolFooterTone = "running" | "success" | "danger" | "warning";

export interface RawToolDetail {
  title: string;
  body: string;
}

export interface ToolFooterStatus {
  label: string;
  tone: ToolFooterTone;
}

export interface ToolPresentation {
  presentationKind: ToolPresentationKind;
  headerLabel: string;
  toolLabel: string;
  command: string | null;
  primaryOutput: string | null;
  rawDetails: RawToolDetail[];
  footerStatus: ToolFooterStatus;
}

export function deriveToolPresentation(tool: ToolInvocation): ToolPresentation {
  const commandLike = isCommandLikeTool(tool);
  const command = commandLike ? extractCommand(tool) : null;
  const primaryOutput = extractPrimaryOutput(tool, commandLike);
  const rawDetails = extractRawDetails(tool);

  return {
    presentationKind: tool.kind === "permission" ? "permission" : commandLike ? "command" : "generic",
    headerLabel: commandLike ? commandHeaderLabel(tool.status) : genericHeaderLabel(tool.status),
    toolLabel: commandLike ? "Shell" : tool.kind || tool.name || "Tool",
    command,
    primaryOutput,
    rawDetails,
    footerStatus: footerStatus(tool),
  };
}

export function isCommandLikeTool(tool: ToolInvocation): boolean {
  const kind = tool.kind.trim().toLowerCase();
  const name = tool.name.trim().toLowerCase();
  return (
    tool.name.trim().startsWith("`") ||
    kind === "bash" ||
    name === "bash" ||
    kind === "execute" ||
    kind === "command" ||
    kind === "terminal" ||
    name === "command" ||
    name === "terminal" ||
    hasCommandInRawInput(tool.raw_input)
  );
}

function commandHeaderLabel(status: ToolStatus): string {
  switch (status) {
    case "Pending":
    case "Running":
      return "运行中";
    case "Succeeded":
      return "已运行";
    case "Failed":
      return "失败";
    case "Interrupted":
      return "已中断";
  }
}

function genericHeaderLabel(status: ToolStatus): string {
  switch (status) {
    case "Pending":
    case "Running":
      return "运行中";
    case "Succeeded":
      return "已运行";
    case "Failed":
      return "失败";
    case "Interrupted":
      return "已中断";
  }
}

function footerStatus(tool: ToolInvocation): ToolFooterStatus {
  const exitCode = tool.terminal_output?.exit_code;
  const exitSuffix = exitCode != null && exitCode !== 0 ? ` (${exitCode})` : "";
  switch (tool.status) {
    case "Pending":
    case "Running":
      return { label: "运行中", tone: "running" };
    case "Succeeded":
      return { label: "成功", tone: "success" };
    case "Failed":
      return { label: `失败${exitSuffix}`, tone: "danger" };
    case "Interrupted":
      return { label: "已中断", tone: "warning" };
  }
}

function extractCommand(tool: ToolInvocation): string | null {
  const fromInput = commandFromRawInput(tool.raw_input);
  if (fromInput) return fromInput;

  if (tool.name.startsWith("`") && tool.name.endsWith("`")) {
    return tool.name.slice(1, -1).trim() || null;
  }

  const fromRawOutput = commandFromRawOutput(tool.raw_output);
  if (fromRawOutput) return fromRawOutput;

  return null;
}

function extractPrimaryOutput(tool: ToolInvocation, commandLike: boolean): string | null {
  const terminalOutput = normalizeOutput(tool.terminal_output?.output);
  if (terminalOutput) return terminalOutput;

  const error = normalizeOutput(tool.error);
  if (error) return error;

  const detail = normalizeOutput(tool.detail_text);
  if (detail) return detail;

  const logText = logsToOutput(tool.logs);
  if (logText) return logText;

  if (!commandLike) {
    const raw = readableRawOutput(tool.raw_output);
    return raw && !isLowValueOutput(raw, tool.summary) ? raw : null;
  }

  const raw = readableRawOutput(tool.raw_output);
  return raw && !isLowValueOutput(raw, tool.summary) ? raw : null;
}

function extractRawDetails(tool: ToolInvocation): RawToolDetail[] {
  const details: RawToolDetail[] = [];
  const rawInput = prettifyRawPayload(tool.raw_input);
  const rawOutput = prettifyRawPayload(tool.raw_output);
  if (rawInput) details.push({ title: "Request", body: rawInput });
  if (rawOutput) details.push({ title: "Result", body: rawOutput });
  return details;
}

function logsToOutput(logs: ToolInvocation["logs"]): string | null {
  const lines = logs
    .map((entry) => {
      const body = normalizeOutput(entry.body);
      if (!body) return null;
      const title = entry.title.trim();
      return title ? `${title} ${body}` : body;
    })
    .filter((line): line is string => !!line);
  return lines.length > 0 ? lines.join("\n") : null;
}

function hasCommandInRawInput(rawInput: string | null): boolean {
  return !!commandFromRawInput(rawInput);
}

function commandFromRawInput(rawInput: string | null): string | null {
  if (!rawInput) return null;
  const parsed = parseJson(rawInput);
  if (parsed && typeof parsed === "object") {
    const record = parsed as Record<string, unknown>;
    return (
      commandValue(record.command) ??
      commandValue(record.cmd) ??
      commandValue(record.shell_command) ??
      commandValue(record.command_line) ??
      commandValue(record.args) ??
      null
    );
  }
  const trimmed = rawInput.trim();
  if (looksLikeJsonPayload(trimmed) || trimmed.includes("\n")) {
    return null;
  }
  return looksLikeCommand(trimmed) ? trimmed : null;
}

function commandFromRawOutput(rawOutput: string | null): string | null {
  const parsed = parseJson(rawOutput);
  if (!parsed || typeof parsed !== "object") return null;
  const record = parsed as Record<string, unknown>;
  return commandValue(record.command) ?? commandFromParsedCmd(record.parsed_cmd) ?? null;
}

function commandFromParsedCmd(parsedCmd: unknown): string | null {
  if (!Array.isArray(parsedCmd)) return null;
  const first = parsedCmd[0];
  if (!first || typeof first !== "object") return null;
  return commandValue((first as Record<string, unknown>).cmd);
}

function commandValue(value: unknown): string | null {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed.length > 0 ? summarizeCommand(trimmed) : null;
  }
  if (Array.isArray(value)) {
    const rawParts = value.filter((part): part is string => typeof part === "string");
    const wrapperSummary = shellWrapperArraySummary(rawParts);
    if (wrapperSummary) return wrapperSummary;
    const parts = rawParts.map(quoteShellArgIfNeeded);
    return parts.length > 0 ? summarizeCommand(parts.join(" ")) : null;
  }
  return null;
}

function summarizeCommand(command: string): string {
  return normalizeToolCommand(command);
}

export function normalizeToolCommand(command: string): string {
  return shellWrapperStringSummary(command) ?? normalizeDisplayCommand(command);
}

function shellWrapperStringSummary(command: string): string | null {
  return shellWrapperArraySummary(tokenizeCommandLine(command));
}

function shellWrapperArraySummary(parts: string[]): string | null {
  if (parts.length < 3 || !isShellWrapperExecutable(parts[0])) return null;

  const commandIndex = parts.findIndex((part) => {
    const normalized = stripOuterQuotes(part).trim().toLowerCase();
    return normalized === "-command" || normalized === "-c" || normalized === "-lc" || normalized === "/c";
  });

  if (commandIndex < 0 || commandIndex + 1 >= parts.length) return null;

  const innerCommand = normalizeDisplayCommand(parts.slice(commandIndex + 1).join(" "));
  return innerCommand.length > 0 ? innerCommand : null;
}

function isShellWrapperExecutable(value: string): boolean {
  const normalized = stripOuterQuotes(value).replace(/\\/g, "/").toLowerCase();
  const baseName = normalized.split("/").pop() ?? normalized;
  return (
    baseName === "pwsh" ||
    baseName === "pwsh.exe" ||
    baseName === "powershell" ||
    baseName === "powershell.exe" ||
    baseName === "bash" ||
    baseName === "sh" ||
    baseName === "zsh" ||
    baseName === "dash" ||
    baseName === "ksh" ||
    baseName === "fish" ||
    baseName === "cmd" ||
    baseName === "cmd.exe"
  );
}

function tokenizeCommandLine(command: string): string[] {
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

function normalizeDisplayCommand(command: string): string {
  return stripOuterQuotes(command.trim())
    .replace(/\\"/g, '"')
    .replace(/\\'/g, "'");
}

function stripOuterQuotes(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length < 2) return trimmed;
  const first = trimmed[0];
  const last = trimmed[trimmed.length - 1];
  if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
    return trimmed.slice(1, -1).trim();
  }
  return trimmed;
}

function readableRawOutput(rawOutput: string | null): string | null {
  if (!rawOutput) return null;
  const parsed = parseJson(rawOutput);
  if (parsed && typeof parsed === "object") {
    const record = parsed as Record<string, unknown>;
    return (
      normalizeOutput(stringValue(record.formatted_output)) ??
      normalizeOutput(stringValue(record.aggregated_output)) ??
      normalizeOutput(stringValue(record.output)) ??
      normalizeOutput(stringValue(record.stdout)) ??
      normalizeOutput(stringValue(record.stderr)) ??
      null
    );
  }
  return normalizeOutput(rawOutput);
}

function prettifyRawPayload(raw: string | null): string | null {
  if (!raw?.trim()) return null;
  const parsed = parseJson(raw);
  if (parsed === null) return raw.trim();
  try {
    return JSON.stringify(parsed, null, 2);
  } catch {
    return raw.trim();
  }
}

function parseJson(raw: string | null): unknown | null {
  if (!raw?.trim()) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function normalizeOutput(output: string | null | undefined): string | null {
  if (!output) return null;
  const decoded = decodeEscapedLineBreaks(output);
  const normalized = stripAnsi(decoded).replace(/\r\n/g, "\n").trim();
  return normalized.length > 0 ? normalized : null;
}

function decodeEscapedLineBreaks(text: string): string {
  if (!text.includes("\\n") && !text.includes("\\r") && !text.includes("\\u001b")) return text;
  return text
    .replace(/\\u001b/g, "\u001b")
    .replace(/\\r\\n/g, "\n")
    .replace(/\\n/g, "\n")
    .replace(/\\r/g, "\n");
}

function stripAnsi(text: string): string {
  return text.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "");
}

function isLowValueOutput(output: string, summary: string): boolean {
  const lower = output.toLowerCase();
  return (
    output === summary ||
    (output.length < 10 && !output.includes("\n")) ||
    lower === "completed" ||
    lower === "ok" ||
    lower === "success"
  );
}

function quoteShellArgIfNeeded(value: string): string {
  if (!/\s/.test(value)) return value;
  if (/^["'].*["']$/.test(value)) return value;
  return `"${value.replace(/"/g, '\\"')}"`;
}

function looksLikeCommand(text: string): boolean {
  const trimmed = normalizeToolCommand(text.trim());
  if (!trimmed) return false;
  if (trimmed.startsWith("`") && trimmed.endsWith("`")) return true;
  if (/[;&|]/.test(trimmed)) return true;
  return /^(?:bash|sh|zsh|cmd|powershell|pwsh|npm|pnpm|yarn|bun|cargo|git|ls|dir|cd|mkdir|rm|cp|mv|python|node|npx)\b/i.test(
    trimmed
  );
}

function looksLikeJsonPayload(text: string): boolean {
  const trimmed = text.trim();
  return trimmed.startsWith("{") || trimmed.startsWith("[");
}
