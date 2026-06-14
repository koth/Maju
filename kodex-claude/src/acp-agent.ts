import {
  Agent,
  AgentSideConnection,
  AuthenticateRequest,
  AuthMethod,
  AvailableCommand,
  CancelNotification,
  ClientCapabilities,
  ForkSessionRequest,
  ForkSessionResponse,
  InitializeRequest,
  InitializeResponse,
  ListSessionsRequest,
  ListSessionsResponse,
  LoadSessionRequest,
  LoadSessionResponse,
  ndJsonStream,
  NewSessionRequest,
  NewSessionResponse,
  PermissionOption,
  PromptRequest,
  PromptResponse,
  ReadTextFileRequest,
  ReadTextFileResponse,
  RequestError,
  ResumeSessionRequest,
  ResumeSessionResponse,
  SessionConfigOption,
  SessionModelState,
  SessionModeState,
  SessionNotification,
  SetSessionConfigOptionRequest,
  SetSessionConfigOptionResponse,
  SetSessionModelRequest,
  SetSessionModelResponse,
  SetSessionModeRequest,
  SetSessionModeResponse,
  CloseSessionRequest,
  CloseSessionResponse,
  DeleteSessionRequest,
  DeleteSessionResponse,
  TerminalHandle,
  TerminalOutputResponse,
  WriteTextFileRequest,
  WriteTextFileResponse,
  StopReason,
} from "@agentclientprotocol/sdk";
import {
  CanUseTool,
  deleteSession,
  getSessionMessages,
  getSessionInfo,
  listSessions,
  McpServerConfig,
  ModelInfo,
  ModelUsage,
  Options,
  PermissionMode,
  PermissionUpdate,
  Query,
  query,
  renameSession,
  Settings,
  SDKAssistantMessageError,
  SDKMessageOrigin,
  SDKPartialAssistantMessage,
  SDKSessionInfo,
  SDKUserMessage,
  SlashCommand,
} from "@anthropic-ai/claude-agent-sdk";
import { ContentBlockParam } from "@anthropic-ai/sdk/resources";
import { BetaContentBlock, BetaRawContentBlockDelta } from "@anthropic-ai/sdk/resources/beta.mjs";
import { randomUUID } from "node:crypto";
import * as os from "node:os";
import * as path from "node:path";
import packageJson from "../package.json" with { type: "json" };
import { SettingsManager } from "./settings.js";
import {
  applyTaskCreate,
  applyTaskUpdate,
  ClaudePlanEntry,
  createPostToolUseHook,
  createTaskHook,
  parseTaskCreateOutput,
  planEntries,
  registerHookCallback,
  TaskState,
  taskStateToPlanEntries,
  toolInfoFromToolUse,
  toolUpdateFromDiffToolResponse,
  toolUpdateFromToolResult,
} from "./tools.js";
import { nodeToWebReadable, nodeToWebWritable, Pushable, unreachable } from "./utils.js";
import { buildWoaEnv, ensureWoaToken, WoaConfig } from "./woa/index.js";

export const CLAUDE_CONFIG_DIR =
  process.env.CLAUDE_CONFIG_DIR ?? path.join(os.homedir(), ".claude");

const MAX_TITLE_LENGTH = 256;
const TITLE_SUMMARY_INPUT_LENGTH = 8_000;
const SESSION_INFO_SYNC_RETRY_DELAYS_MS = [0, 250, 1_000, 2_500, 5_000, 10_000] as const;
const KODEX_CONTEXT_COMPACTION_META_KEY = "kodex.ai/contextCompaction";
const KODEX_PERMISSION_GUIDANCE_META_KEY = "kodex.ai/permissionGuidance";
const KODEX_USER_INPUT_ANSWERS_META_KEY = "kodex.ai/userInputAnswers";
const ASK_USER_QUESTION_OPTION_PREFIX = "ask_user_question";

interface NormalizedAskUserQuestionOption {
  label: string;
  description: string;
  preview?: string;
}

interface NormalizedAskUserQuestion {
  question: string;
  header: string;
  options: NormalizedAskUserQuestionOption[];
  multiSelect: boolean;
}

interface AskUserQuestionSelection {
  question: NormalizedAskUserQuestion;
  option: NormalizedAskUserQuestionOption;
}

function contextCompactionNotification(
  sessionId: string,
  phase: "started" | "completed",
  fallbackText: string,
): SessionNotification {
  return {
    sessionId,
    update: {
      sessionUpdate: "agent_message_chunk",
      content: {
        type: "text",
        text: fallbackText,
      },
      _meta: {
        [KODEX_CONTEXT_COMPACTION_META_KEY]: {
          phase,
          message: phase === "started" ? "正在压缩上下文" : "上下文已自动压缩",
        },
      },
    },
  } as SessionNotification;
}

function sanitizeTitle(text: string): string {
  // Replace newlines and collapse whitespace
  const sanitized = text
    .replace(/[\r\n]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (sanitized.length <= MAX_TITLE_LENGTH) {
    return sanitized;
  }
  return sanitized.slice(0, MAX_TITLE_LENGTH - 1) + "…";
}

function truncateTitle(text: string, maxLength: number, suffix = "…"): string {
  const chars = Array.from(text);
  if (chars.length <= maxLength) {
    return text;
  }
  return `${chars.slice(0, Math.max(0, maxLength - Array.from(suffix).length)).join("")}${suffix}`;
}

function titlePromptText(prompt: PromptRequest): string | null {
  for (const chunk of prompt.prompt) {
    if (chunk.type !== "text") {
      continue;
    }
    const text = chunk.text.trim();
    if (text.length > 0 && !text.startsWith("/")) {
      return truncateTitle(text, TITLE_SUMMARY_INPUT_LENGTH, "");
    }
  }
  return null;
}

function cleanGeneratedTitle(text: string | null | undefined): string | null {
  if (!text) {
    return null;
  }
  let title = sanitizeTitle(text)
    .replace(/^```(?:\w+)?\s*/i, "")
    .replace(/\s*```$/i, "")
    .replace(/^["'“”‘’`]+|["'“”‘’`]+$/g, "")
    .trim();
  if (title.includes("\n")) {
    title = title.split(/\r?\n/)[0].trim();
  }
  if (!title) {
    return null;
  }
  return truncateTitle(title, MAX_TITLE_LENGTH);
}

function sleep(ms: number): Promise<void> {
  if (ms <= 0) {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    if (typeof timer === "object" && "unref" in timer) {
      timer.unref();
    }
  });
}

function titleFromSessionInfo(info: SDKSessionInfo | undefined): string | null {
  if (!info) {
    return null;
  }

  // Claude Code's SDK `summary` can be the user's first prompt, or a lightly
  // normalized variant of it. Kodex wants LLM-authored titles for this channel,
  // so only trust an explicit custom title (written by renameSession after our
  // title-generation query). If there is no custom title, the caller should
  // generate one instead of falling back to `summary`.
  const customTitle = cleanGeneratedTitle(info.customTitle);
  if (customTitle) {
    return customTitle;
  }
  return null;
}

function titleInputFromSessionInfo(info: SDKSessionInfo | undefined): string | null {
  if (!info) {
    return null;
  }
  const input = info.firstPrompt ?? info.summary;
  if (!input) {
    return null;
  }
  const normalized = sanitizeTitle(input);
  if (!normalized) {
    return null;
  }
  return truncateTitle(normalized, TITLE_SUMMARY_INPUT_LENGTH, "");
}

/**
 * Logger interface for customizing logging output
 */
export interface Logger {
  log: (...args: any[]) => void;
  error: (...args: any[]) => void;
}

export type ClaudeAcpAgentOptions = {
  logger?: Logger;
  woa?: WoaConfig;
};

export type RunAcpOptions = {
  woa?: WoaConfig;
  input?: WritableStream<Uint8Array>;
  output?: ReadableStream<Uint8Array>;
};

function isLogger(value: Logger | ClaudeAcpAgentOptions | undefined): value is Logger {
  return (
    !!value &&
    typeof (value as Logger).log === "function" &&
    typeof (value as Logger).error === "function"
  );
}

function mergeEnv(...sources: Array<Record<string, string | undefined> | undefined>) {
  const env: Record<string, string> = {};
  for (const source of sources) {
    if (!source) {
      continue;
    }
    for (const [key, value] of Object.entries(source)) {
      if (value === undefined) {
        delete env[key];
      } else {
        env[key] = value;
      }
    }
  }
  return env;
}

type AccumulatedUsage = {
  inputTokens: number;
  outputTokens: number;
  cachedReadTokens: number;
  cachedWriteTokens: number;
};

type UsageSnapshot = {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
};

const ZERO_USAGE = Object.freeze({
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
});

const DEFAULT_CONTEXT_WINDOW = 200000;
const KODEX_MODEL_PROVIDER_MAP_ENV = "KODEX_MODEL_PROVIDER_MAP";
const KODEX_PROVIDER_VALUE_PREFIX = "kodex-provider:";

type KodexModelProviderEntry = {
  model: string;
  displayName: string;
  provider: string;
};

type KodexModelInfo = ModelInfo & {
  kodexProvider?: string;
  kodexRouteModel?: string;
};

type Session = {
  query: Query;
  input: Pushable<SDKUserMessage>;
  cancelled: boolean;
  cwd: string;
  /** Serialized snapshot of session-defining params (cwd, mcpServers) used to
   *  detect when loadSession/resumeSession is called with changed values. */
  sessionFingerprint: string;
  settingsManager: SettingsManager;
  accumulatedUsage: AccumulatedUsage;
  modes: SessionModeState;
  models: SessionModelState;
  modelInfos: ModelInfo[];
  configOptions: SessionConfigOption[];
  promptRunning: boolean;
  pendingMessages: Map<string, { resolve: (cancelled: boolean) => void; order: number }>;
  nextPendingOrder: number;
  abortController: AbortController;
  emitRawSDKMessages: boolean | SDKMessageFilter[];
  /** Context window size of the last top-level assistant model, carried across
   *  prompts so mid-stream usage_update notifications report a correct `size`
   *  before the turn's first result message arrives. Defaults to
   *  DEFAULT_CONTEXT_WINDOW, refreshed from each result's modelUsage, and
   *  invalidated when the user switches the session's model. */
  contextWindowSize: number;
  /** Accumulated task list for the session, keyed by task ID. Task IDs are
   *  per-session, so this state must not be shared across sessions. */
  taskState: TaskState;
  /** First user prompt used by the title summarizer if the SDK has no title. */
  titlePrompt?: string;
  /** First top-level assistant result used by the title summarizer. */
  titleAssistantResult?: string;
  /** Model-generated ACP title used until it is persisted as a Claude custom title. */
  generatedTitle?: string;
  /** Whether a model title summary job has already been started for this session. */
  titleSummaryStarted?: boolean;
  /** Last title emitted to the client, used to avoid duplicate title updates. */
  lastEmittedTitle?: string;
};

type QueryWithTitleHelpers = Query & {
  generateSessionTitle?: (
    description: string,
    options?: { persist?: boolean },
  ) => Promise<string | null | undefined>;
  askSideQuestion?: (
    question: string,
  ) => Promise<{ response: string; synthetic?: boolean } | null | undefined>;
};

/** Compute a stable fingerprint of the session-defining params so we can
 *  detect when a loadSession/resumeSession call requires tearing down and
 *  recreating the underlying Query process.  MCP servers are sorted by name
 *  so that ordering differences don't trigger unnecessary recreations. */
function computeSessionFingerprint(params: {
  cwd: string;
  mcpServers?: NewSessionRequest["mcpServers"];
}): string {
  const servers = [...(params.mcpServers ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  return JSON.stringify({ cwd: params.cwd, mcpServers: servers });
}

type BackgroundTerminal =
  | {
      handle: TerminalHandle;
      status: "started";
      lastOutput: TerminalOutputResponse | null;
    }
  | {
      status: "aborted" | "exited" | "killed" | "timedOut";
      pendingOutput: TerminalOutputResponse;
    };

export type SDKMessageFilter = {
  type: string;
  subtype?: string;
  origin?: SDKMessageOrigin["kind"];
};

/**
 * Extra metadata that can be given when creating a new session.
 */
export type NewSessionMeta = {
  claudeCode?: {
    /**
     * Options forwarded to Claude Code when starting a new session.
     * Those parameters will be ignored and managed by ACP:
     *   - cwd
     *   - includePartialMessages
     *   - allowDangerouslySkipPermissions
     *   - permissionMode
     *   - canUseTool
     *   - executable
     * Those parameters will be used and updated to work with ACP:
     *   - hooks (merged with ACP's hooks)
     *   - mcpServers (merged with ACP's mcpServers)
     *   - disallowedTools (merged with ACP's disallowedTools)
     *   - tools (passed through; defaults to claude_code preset if not provided)
     */
    options?: Options;
    /**
     * When set, raw SDK messages are emitted as extNotification("_claude/sdkMessage", message)
     * in addition to normal processing.
     * - true: emit all messages
     * - false/undefined: emit nothing (default)
     * - SDKMessageFilter[]: emit only messages matching at least one filter
     */
    emitRawSDKMessages?: boolean | SDKMessageFilter[];
  };
  additionalRoots?: string[];
};

/**
 * Extra metadata for 'gateway' authentication requests.
 */
type GatewayAuthMeta = {
  /**
   * These parameters are mapped to environment variables to:
   * - Redirect API calls via baseUrl
   * - Inject custom headers
   * - Bypass the default Claude login requirement
   */
  gateway: {
    baseUrl: string;
    headers: Record<string, string>;
  };
};

type GatewayAuthRequest = AuthenticateRequest & { _meta?: GatewayAuthMeta };

/**
 * Extra metadata that the agent provides for each tool_call / tool_update update.
 */
export type ToolUpdateMeta = {
  claudeCode?: {
    /* The name of the tool that was used in Claude Code. */
    toolName: string;
    /* The structured output provided by Claude Code. */
    toolResponse?: unknown;
  };
  /* Terminal metadata for Bash tool execution, matching codex-acp's _meta protocol. */
  terminal_info?: {
    terminal_id: string;
  };
  terminal_output?: {
    terminal_id: string;
    data: string;
  };
  terminal_exit?: {
    terminal_id: string;
    exit_code: number;
    signal: string | null;
  };
};

export type ToolUseCache = {
  [key: string]: {
    type: "tool_use" | "server_tool_use" | "mcp_tool_use";
    id: string;
    name: string;
    input: unknown;
  };
};

export async function claudeCliPath(): Promise<string> {
  if (process.env.CLAUDE_CODE_EXECUTABLE) {
    return process.env.CLAUDE_CODE_EXECUTABLE;
  }
  // The SDK's CLI is a native binary shipped as a platform-specific optional
  // dependency of @anthropic-ai/claude-agent-sdk. Resolve via a require bound
  // to the SDK so nested installs are found even when npm doesn't hoist.
  const { createRequire } = await import("node:module");
  const req = createRequire(import.meta.resolve("@anthropic-ai/claude-agent-sdk"));
  const ext = process.platform === "win32" ? ".exe" : "";
  // On linux, both glibc and musl variants may be installed side-by-side
  // (e.g. bunx hydrates every optional dep), so picking one by trial is
  // unreliable: the wrong binary segfaults at runtime instead of failing to
  // spawn. Detect the runtime libc and prefer the matching variant, falling
  // back to the other only if the preferred one isn't installed.
  const candidates =
    process.platform === "linux"
      ? isMuslLibc()
        ? [
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}-musl/claude${ext}`,
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}/claude${ext}`,
          ]
        : [
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}/claude${ext}`,
            `@anthropic-ai/claude-agent-sdk-linux-${process.arch}-musl/claude${ext}`,
          ]
      : [`@anthropic-ai/claude-agent-sdk-${process.platform}-${process.arch}/claude${ext}`];
  for (const candidate of candidates) {
    try {
      return req.resolve(candidate);
    } catch {
      // try next candidate
    }
  }
  throw new Error(
    `Claude native binary not found for ${process.platform}-${process.arch}. ` +
      `Reinstall @anthropic-ai/claude-agent-sdk without --omit=optional, or set CLAUDE_CODE_EXECUTABLE.`,
  );
}

function isMuslLibc(): boolean {
  // process.report.getReport().header.glibcVersionRuntime is populated when
  // Node is dynamically linked against glibc, and absent on musl.
  const report = process.report?.getReport() as
    | { header?: { glibcVersionRuntime?: string } }
    | undefined;
  return !report?.header?.glibcVersionRuntime;
}

function shouldHideClaudeAuth(): boolean {
  return process.argv.includes("--hide-claude-auth");
}

// Bypass Permissions doesn't work if we are a root/sudo user
const IS_ROOT = (process.geteuid?.() ?? process.getuid?.()) === 0;
const ALLOW_BYPASS = !IS_ROOT || !!process.env.IS_SANDBOX;

// Slash commands that the SDK handles locally without replaying the user
// message and without invoking the model.
const LOCAL_ONLY_COMMANDS = new Set(["/context", "/heapdump", "/extra-usage"]);

// The Claude SDK persists local slash command invocations (e.g. `/model`) and
// their output as user messages in the session transcript, wrapping the
// payload in these XML-like markers that the CLI uses for its own display.
// The live prompt loop drops them; replay must strip them too or they leak
// into the UI on session/load.
const LOCAL_COMMAND_TAG_PATTERN =
  /<(command-name|command-message|command-args|local-command-stdout|local-command-stderr)>[\s\S]*?<\/\1>/g;

function stripMarkerTags(text: string): string {
  return text.replace(LOCAL_COMMAND_TAG_PATTERN, "");
}

/**
 * Return user-message content with local-command marker tags removed, or
 * `null` if nothing meaningful remains (caller should skip the message).
 * Preserves real prose that's mixed in alongside the markers — e.g. a
 * message like `<command-name>…</command-name>hi` becomes `hi`.
 */
export function stripLocalCommandMetadata(content: unknown): unknown | null {
  if (typeof content === "string") {
    const stripped = stripMarkerTags(content);
    return stripped.trim() === "" ? null : stripped;
  }
  if (!Array.isArray(content)) return content;

  const kept: unknown[] = [];
  for (const block of content) {
    if (
      block &&
      typeof block === "object" &&
      "type" in block &&
      (block as { type: unknown }).type === "text" &&
      "text" in block &&
      typeof (block as { text: unknown }).text === "string"
    ) {
      const stripped = stripMarkerTags((block as { text: string }).text);
      if (stripped.trim() === "") continue;
      kept.push({ ...(block as object), text: stripped });
    } else {
      kept.push(block);
    }
  }
  if (kept.length === 0) return null;
  return kept;
}

export function isLocalCommandMetadata(content: unknown): boolean {
  return stripLocalCommandMetadata(content) === null;
}

const PERMISSION_MODE_ALIASES: Record<string, PermissionMode> = {
  auto: "auto",
  default: "default",
  acceptedits: "acceptEdits",
  dontask: "dontAsk",
  plan: "plan",
  bypasspermissions: "bypassPermissions",
  bypass: "bypassPermissions",
};

export function resolvePermissionMode(
  defaultMode?: unknown,
  logger: Logger = console,
): PermissionMode {
  if (defaultMode === undefined) {
    return "default";
  }

  if (typeof defaultMode !== "string") {
    logger.error("Ignoring permissions.defaultMode from settings: expected a string.");
    return "default";
  }

  const normalized = defaultMode.trim().toLowerCase();
  if (normalized === "") {
    logger.error("Ignoring permissions.defaultMode from settings: expected a non-empty string.");
    return "default";
  }

  const mapped = PERMISSION_MODE_ALIASES[normalized];
  if (!mapped) {
    logger.error(`Ignoring permissions.defaultMode from settings: unknown value '${defaultMode}'.`);
    return "default";
  }

  if (mapped === "bypassPermissions" && !ALLOW_BYPASS) {
    logger.error(
      "Ignoring permissions.defaultMode from settings: bypassPermissions is not available when running as root.",
    );
    return "default";
  }

  return mapped;
}

/**
 * Builds the label for the "Always Allow" permission option so the user can see
 * the exact scope they are committing to. Uses the SDK-provided suggestions
 * when available (e.g. `Bash(npm test:*)`) and falls back to naming the whole
 * tool so "Always Allow" is never a blank check without disclosure.
 */
export function describeAlwaysAllow(
  suggestions: PermissionUpdate[] | undefined,
  toolName: string,
): string {
  if (!suggestions || suggestions.length === 0) {
    return `Always Allow all ${toolName}`;
  }

  const ruleLabels: string[] = [];
  const directories: string[] = [];

  for (const update of suggestions) {
    if (update.type === "addRules" && update.behavior === "allow") {
      for (const rule of update.rules) {
        ruleLabels.push(
          rule.ruleContent ? `${rule.toolName}(${rule.ruleContent})` : `all ${rule.toolName}`,
        );
      }
    } else if (update.type === "addDirectories") {
      directories.push(...update.directories);
    }
  }

  const parts: string[] = [];
  if (ruleLabels.length > 0) {
    parts.push(ruleLabels.join(", "));
  }
  if (directories.length > 0) {
    parts.push(`access to ${directories.join(", ")}`);
  }

  if (parts.length === 0) {
    return `Always Allow all ${toolName}`;
  }

  return `Always Allow ${parts.join(" and ")}`;
}

function normalizeAskUserQuestions(input: unknown): NormalizedAskUserQuestion[] | null {
  if (!isRecord(input) || !Array.isArray(input.questions)) return null;

  const questions: NormalizedAskUserQuestion[] = [];
  for (const rawQuestion of input.questions) {
    if (!isRecord(rawQuestion)) continue;
    const question = stringValue(rawQuestion.question);
    const header = stringValue(rawQuestion.header) || question.slice(0, 12) || "Question";
    if (!question || !Array.isArray(rawQuestion.options)) continue;

    const options: NormalizedAskUserQuestionOption[] = [];
    for (const rawOption of rawQuestion.options) {
      if (!isRecord(rawOption)) continue;
      const label = stringValue(rawOption.label);
      const description = stringValue(rawOption.description);
      if (!label) continue;
      const preview = stringValue(rawOption.preview);
      options.push({
        label,
        description,
        ...(preview ? { preview } : {}),
      });
    }

    if (options.length === 0) continue;
    questions.push({
      question,
      header,
      options,
      multiSelect: rawQuestion.multiSelect === true,
    });
  }

  return questions.length > 0 ? questions : null;
}

function askUserQuestionPermissionOptions(questions: NormalizedAskUserQuestion[]): PermissionOption[] {
  const includeQuestionPrefix = questions.length > 1;
  const options: PermissionOption[] = [];

  questions.forEach((question, questionIndex) => {
    question.options.forEach((option, optionIndex) => {
      options.push({
        kind: "allow_once",
        name: includeQuestionPrefix ? `${question.header}: ${option.label}` : option.label,
        optionId: `${ASK_USER_QUESTION_OPTION_PREFIX}:${questionIndex}:${optionIndex}`,
      });
    });
  });

  return options;
}

function askUserQuestionSelection(
  optionId: string | undefined,
  questions: NormalizedAskUserQuestion[],
): AskUserQuestionSelection | null {
  if (!optionId) return null;
  const [prefix, questionIndexText, optionIndexText] = optionId.split(":");
  if (prefix !== ASK_USER_QUESTION_OPTION_PREFIX) return null;
  const questionIndex = Number(questionIndexText);
  const optionIndex = Number(optionIndexText);
  if (!Number.isInteger(questionIndex) || !Number.isInteger(optionIndex)) return null;
  const question = questions[questionIndex];
  const option = question?.options[optionIndex];
  return question && option ? { question, option } : null;
}

function withAskUserQuestionAnswer(
  input: unknown,
  selection: AskUserQuestionSelection,
  guidance?: string,
): Record<string, unknown> {
  const base = isRecord(input) ? { ...input } : {};
  const answers = stringRecord(base.answers);
  answers[selection.question.question] = selection.option.label;
  base.answers = answers;

  const notes = guidance?.trim();
  const preview = selection.option.preview?.trim();
  if (preview || notes) {
    const annotations = annotationRecord(base.annotations);
    annotations[selection.question.question] = {
      ...(preview ? { preview } : {}),
      ...(notes ? { notes } : {}),
    };
    base.annotations = annotations;
  }

  return base;
}

function withAskUserQuestionAnswers(
  input: unknown,
  questions: NormalizedAskUserQuestion[],
  answerMap: Record<string, string[]>,
): Record<string, unknown> {
  const base = isRecord(input) ? { ...input } : {};
  const answers = stringRecord(base.answers);
  const annotations = annotationRecord(base.annotations);
  let hasAnnotations = Object.keys(annotations).length > 0;

  for (const question of questions) {
    const values = answerMap[question.question] ?? [];
    if (values.length === 0) continue;
    answers[question.question] = question.multiSelect ? values.join(", ") : values[0] ?? "";

    const previews = values
      .map((value) => question.options.find((option) => option.label === value)?.preview?.trim())
      .filter((preview): preview is string => !!preview);
    if (previews.length > 0) {
      annotations[question.question] = {
        ...(annotations[question.question] ?? {}),
        preview: previews.join("\n\n"),
      };
      hasAnnotations = true;
    }
  }

  base.answers = answers;
  if (hasAnnotations) {
    base.annotations = annotations;
  }

  return base;
}

function permissionGuidance(response: unknown): string | undefined {
  if (!isRecord(response)) return undefined;
  const topLevelMeta = isRecord(response._meta) ? response._meta : undefined;
  const outcome = isRecord(response.outcome) ? response.outcome : undefined;
  const outcomeMeta = outcome && isRecord(outcome._meta) ? outcome._meta : undefined;
  return (
    stringValue(topLevelMeta?.[KODEX_PERMISSION_GUIDANCE_META_KEY]) ||
    stringValue(outcomeMeta?.[KODEX_PERMISSION_GUIDANCE_META_KEY]) ||
    undefined
  );
}

function permissionInputAnswers(response: unknown): Record<string, string[]> | undefined {
  if (!isRecord(response)) return undefined;
  const topLevelMeta = isRecord(response._meta) ? response._meta : undefined;
  const outcome = isRecord(response.outcome) ? response.outcome : undefined;
  const outcomeMeta = outcome && isRecord(outcome._meta) ? outcome._meta : undefined;
  return (
    inputAnswersFromMeta(topLevelMeta?.[KODEX_USER_INPUT_ANSWERS_META_KEY]) ??
    inputAnswersFromMeta(outcomeMeta?.[KODEX_USER_INPUT_ANSWERS_META_KEY]) ??
    undefined
  );
}

function inputAnswersFromMeta(value: unknown): Record<string, string[]> | undefined {
  if (!isRecord(value)) return undefined;
  const rawAnswers = isRecord(value.answers) ? value.answers : value;
  const answers: Record<string, string[]> = {};
  for (const [key, entry] of Object.entries(rawAnswers)) {
    if (!Array.isArray(entry)) continue;
    const values = entry
      .filter((item): item is string => typeof item === "string")
      .map((item) => item.trim())
      .filter(Boolean);
    if (values.length > 0) answers[key] = values;
  }
  return Object.keys(answers).length > 0 ? answers : undefined;
}

function stringRecord(value: unknown): Record<string, string> {
  if (!isRecord(value)) return {};
  const result: Record<string, string> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry === "string") result[key] = entry;
  }
  return result;
}

function annotationRecord(value: unknown): Record<string, Record<string, string>> {
  if (!isRecord(value)) return {};
  const result: Record<string, Record<string, string>> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (!isRecord(entry)) continue;
    const annotation: Record<string, string> = {};
    const preview = stringValue(entry.preview);
    const notes = stringValue(entry.notes);
    if (preview) annotation.preview = preview;
    if (notes) annotation.notes = notes;
    if (Object.keys(annotation).length > 0) result[key] = annotation;
  }
  return result;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

// Implement the ACP Agent interface
export class ClaudeAcpAgent implements Agent {
  sessions: {
    [key: string]: Session;
  };
  client: AgentSideConnection;
  toolUseCache: ToolUseCache;
  backgroundTerminals: { [key: string]: BackgroundTerminal } = {};
  clientCapabilities?: ClientCapabilities;
  logger: Logger;
  gatewayAuthRequest?: GatewayAuthRequest;
  woaConfig?: WoaConfig;

  constructor(client: AgentSideConnection, loggerOrOptions?: Logger | ClaudeAcpAgentOptions) {
    this.sessions = {};
    this.client = client;
    this.toolUseCache = {};
    this.logger = isLogger(loggerOrOptions)
      ? loggerOrOptions
      : (loggerOrOptions?.logger ?? console);
    this.woaConfig = isLogger(loggerOrOptions) ? undefined : loggerOrOptions?.woa;
  }

  async initialize(request: InitializeRequest): Promise<InitializeResponse> {
    this.clientCapabilities = request.clientCapabilities;

    // Bypasses standard auth by routing requests through a custom Anthropic-protocol gateway.
    // Only offered when the client advertises `auth._meta.gateway` capability.
    const supportsGatewayAuth = request.clientCapabilities?.auth?._meta?.gateway === true;

    const gatewayAuthMethod: AuthMethod = {
      id: "gateway",
      name: "Custom model gateway",
      description: "Use a custom gateway to authenticate and access models",
      _meta: {
        gateway: {
          protocol: "anthropic",
        },
      },
    };

    const gatewayBedrockAuthMethod: AuthMethod = {
      id: "gateway-bedrock",
      name: "Custom model gateway",
      description: "Use a custom gateway to authenticate and access models",
      _meta: {
        gateway: {
          protocol: "bedrock",
        },
      },
    };

    const supportsTerminalAuth = request.clientCapabilities?.auth?.terminal === true;
    const supportsMetaTerminalAuth = request.clientCapabilities?._meta?.["terminal-auth"] === true;
    const supportsAnyTerminalAuth = supportsTerminalAuth || supportsMetaTerminalAuth;
    const baseTerminalAuthArgs = process.argv.slice(1);

    // Detect remote environments where the OAuth browser redirect to localhost
    // won't work. This matches the SDK's internal isRemote check. In these cases,
    // the `auth login` subcommand would fall back to a device-code-like manual
    // flow, which doesn't work well over ACP, so we offer the TUI login instead.
    const isRemote = !!(
      process.env.NO_BROWSER ||
      process.env.SSH_CONNECTION ||
      process.env.SSH_CLIENT ||
      process.env.SSH_TTY ||
      process.env.CLAUDE_CODE_REMOTE
    );
    const terminalAuthMethods: AuthMethod[] = [];

    if (isRemote) {
      const remoteLoginMethod: AuthMethod = {
        description: "Run `claude /login` in the terminal",
        name: "Log in with Claude",
        id: "claude-login",
        type: "terminal",
        args: ["--cli"],
      };

      if (supportsMetaTerminalAuth) {
        remoteLoginMethod._meta = {
          "terminal-auth": {
            command: process.execPath,
            args: [...process.argv.slice(1), "--cli"],
            label: "Claude Login",
          },
        };
      }

      if (!shouldHideClaudeAuth() && (supportsTerminalAuth || supportsMetaTerminalAuth)) {
        terminalAuthMethods.push(remoteLoginMethod);
      }
    } else {
      const claudeLoginMethod: AuthMethod = {
        description: "Use Claude subscription ",
        name: "Claude Subscription",
        id: "claude-ai-login",
        type: "terminal",
        args: ["--cli", "auth", "login", "--claudeai"],
      };

      const consoleLoginMethod: AuthMethod = {
        description: "Use Anthropic Console (API usage billing)",
        name: "Anthropic Console",
        id: "console-login",
        type: "terminal",
        args: ["--cli", "auth", "login", "--console"],
      };

      if (supportsMetaTerminalAuth) {
        const baseArgs = process.argv.slice(1);
        claudeLoginMethod._meta = {
          "terminal-auth": {
            command: process.execPath,
            args: [...baseArgs, "--cli", "auth", "login", "--claudeai"],
            label: "Claude Login",
          },
        };
        consoleLoginMethod._meta = {
          "terminal-auth": {
            command: process.execPath,
            args: [...baseArgs, "--cli", "auth", "login", "--console"],
            label: "Anthropic Console Login",
          },
        };
      }

      if (!shouldHideClaudeAuth() && (supportsTerminalAuth || supportsMetaTerminalAuth)) {
        terminalAuthMethods.push(claudeLoginMethod);
      }
      if (supportsTerminalAuth || supportsMetaTerminalAuth) {
        terminalAuthMethods.push(consoleLoginMethod);
      }
    }

    if (this.woaConfig?.enabled && supportsAnyTerminalAuth) {
      const woaLoginMethod: AuthMethod = {
        description: "Run WOA Device Code login",
        name: "Tencent WOA",
        id: "woa-login",
        type: "terminal",
        args: ["--woa-login"],
      };

      if (supportsMetaTerminalAuth) {
        woaLoginMethod._meta = {
          "terminal-auth": {
            command: process.execPath,
            args: [...baseTerminalAuthArgs, "--woa-login"],
            label: "Tencent WOA Login",
          },
        };
      }

      terminalAuthMethods.push(woaLoginMethod);
    }

    return {
      protocolVersion: 1,
      agentCapabilities: {
        _meta: {
          claudeCode: {
            promptQueueing: true,
          },
        },
        promptCapabilities: {
          image: true,
          embeddedContext: true,
        },
        mcpCapabilities: {
          http: true,
          sse: true,
        },
        loadSession: true,
        sessionCapabilities: {
          additionalDirectories: {},
          close: {},
          delete: {},
          fork: {},
          list: {},
          resume: {},
        },
      },
      agentInfo: {
        name: packageJson.name,
        title: "Claude Agent",
        version: packageJson.version,
      },
      authMethods: [
        ...terminalAuthMethods,
        ...(supportsGatewayAuth ? [gatewayAuthMethod, gatewayBedrockAuthMethod] : []),
      ],
    };
  }

  async newSession(params: NewSessionRequest): Promise<NewSessionResponse> {
    const response = await this.createSession(params, {
      // Revisit these meta values once we support resume
      resume: (params._meta as NewSessionMeta | undefined)?.claudeCode?.options?.resume,
    });
    // Needs to happen after we return the session
    setTimeout(() => {
      this.sendAvailableCommandsUpdate(response.sessionId);
    }, 0);
    return response;
  }

  async unstable_forkSession(params: ForkSessionRequest): Promise<ForkSessionResponse> {
    const response = await this.createSession(
      {
        cwd: params.cwd,
        mcpServers: params.mcpServers ?? [],
        additionalDirectories: params.additionalDirectories,
        _meta: params._meta,
      },
      {
        resume: params.sessionId,
        forkSession: true,
      },
    );
    // Needs to happen after we return the session
    setTimeout(() => {
      this.sendAvailableCommandsUpdate(response.sessionId);
    }, 0);
    return response;
  }

  async resumeSession(params: ResumeSessionRequest): Promise<ResumeSessionResponse> {
    const result = await this.getOrCreateSession(params);

    // Needs to happen after we return the session
    setTimeout(() => {
      this.sendAvailableCommandsUpdate(params.sessionId);
    }, 0);
    return result;
  }

  async loadSession(params: LoadSessionRequest): Promise<LoadSessionResponse> {
    const result = await this.getOrCreateSession(params);

    await this.replaySessionHistory(params.sessionId);

    // Send available commands after replay so it doesn't interleave with history
    setTimeout(() => {
      this.sendAvailableCommandsUpdate(params.sessionId);
    }, 0);

    return result;
  }

  async listSessions(params: ListSessionsRequest): Promise<ListSessionsResponse> {
    const sdk_sessions = await listSessions({ dir: params.cwd ?? undefined });
    const sessions = [];

    for (const session of sdk_sessions) {
      if (!session.cwd) continue;
      const localSession = this.sessions[session.sessionId];
      const generatedTitle = localSession?.generatedTitle;
      const title = titleFromSessionInfo(session) || generatedTitle || "";
      sessions.push({
        sessionId: session.sessionId,
        cwd: session.cwd,
        title,
        updatedAt: new Date(session.lastModified).toISOString(),
      });
    }
    return {
      sessions,
    };
  }

  async authenticate(_params: AuthenticateRequest): Promise<void> {
    if (_params.methodId === "gateway" || _params.methodId === "gateway-bedrock") {
      this.gatewayAuthRequest = _params as GatewayAuthRequest;
      return;
    }
    throw new Error("Method not implemented.");
  }

  async prompt(params: PromptRequest): Promise<PromptResponse> {
    const session = this.sessions[params.sessionId];
    if (!session) {
      throw new Error("Session not found");
    }

    session.cancelled = false;
    session.accumulatedUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cachedReadTokens: 0,
      cachedWriteTokens: 0,
    };

    let lastAssistantTotalUsage: number | null = null;
    let lastAssistantUsage: UsageSnapshot | null = null;
    let lastAssistantModel: string | null = null;
    // When the Claude SDK classifies a turn as failed (e.g. rate limit, auth
    // problem, billing), it sets a categorical `error` field on the
    // `SDKAssistantMessage` that precedes the final `result` message. We
    // capture it here so the subsequent `RequestError.internalError` can
    // forward it to clients as structured `data`, sparing them from
    // pattern-matching on the human-readable message text.
    let lastAssistantError: SDKAssistantMessageError | undefined;

    const userMessage = promptToClaude(params);

    const promptUuid = randomUUID();
    userMessage.uuid = promptUuid;

    // These local-only commands return a result without replaying the user
    // message. Mark promptReplayed=true so their result isn't consumed as a
    // background task result.
    const firstText = params.prompt[0]?.type === "text" ? params.prompt[0].text : "";
    const isLocalOnlyCommand =
      firstText.startsWith("/") && LOCAL_ONLY_COMMANDS.has(firstText.split(" ", 1)[0]);
    if (!isLocalOnlyCommand && !session.titlePrompt) {
      session.titlePrompt = titlePromptText(params) ?? undefined;
    }

    if (session.promptRunning) {
      session.input.push(userMessage);
      const order = session.nextPendingOrder++;
      const cancelled = await new Promise<boolean>((resolve) => {
        session.pendingMessages.set(promptUuid, { resolve, order });
      });
      if (cancelled) {
        return { stopReason: "cancelled" };
      }
    } else {
      session.input.push(userMessage);
    }

    session.promptRunning = true;
    let handedOff = false;
    let stopReason: StopReason = "end_turn";
    let shouldSyncSessionInfo = false;

    try {
      while (true) {
        const { value: message, done } = await session.query.next();

        if (done || !message) {
          if (session.cancelled) {
            return { stopReason: "cancelled" };
          }
          break;
        }

        if (
          session.emitRawSDKMessages &&
          shouldEmitRawMessage(session.emitRawSDKMessages, message)
        ) {
          await this.client.extNotification("_claude/sdkMessage", {
            sessionId: params.sessionId,
            message: message as Record<string, unknown>,
          });
        }

        switch (message.type) {
          case "system":
            switch (message.subtype) {
              case "init":
                break;
              case "status": {
                if (message.status === "compacting") {
                  await this.client.sessionUpdate(
                    contextCompactionNotification(message.session_id, "started", "Compacting..."),
                  );
                }
                break;
              }
              case "compact_boundary": {
                // Send used:0 immediately so the client doesn't keep showing
                // the stale pre-compaction context size until the next turn.
                //
                // This is a deliberate approximation: we don't know the exact
                // post-compaction token count (only the SDK's next API call
                // reveals that). But used:0 is directionally correct — context
                // just dropped dramatically — and the real value replaces it
                // within seconds when the next result message arrives.
                // The alternative (no update) leaves the client showing e.g.
                // "944k/1m" right after the user sees "Compacting completed",
                // which is confusing and wrong.
                lastAssistantTotalUsage = 0;
                lastAssistantUsage = null;
                await this.client.sessionUpdate({
                  sessionId: message.session_id,
                  update: {
                    sessionUpdate: "usage_update",
                    used: 0,
                    size: session.contextWindowSize,
                  },
                });
                await this.client.sessionUpdate(
                  contextCompactionNotification(
                    message.session_id,
                    "completed",
                    "\n\nCompacting completed.",
                  ),
                );
                break;
              }
              case "local_command_output": {
                await this.client.sessionUpdate({
                  sessionId: message.session_id,
                  update: {
                    sessionUpdate: "agent_message_chunk",
                    content: { type: "text", text: message.content },
                  },
                });
                break;
              }
              case "session_state_changed": {
                if (message.state === "idle") {
                  shouldSyncSessionInfo = true;
                  return { stopReason, usage: sessionUsage(session) };
                }
                break;
              }
              case "hook_started":
              case "hook_progress":
              case "hook_response":
              case "files_persisted":
              case "task_started":
              case "task_notification":
              case "task_progress":
              case "task_updated":
              case "elicitation_complete":
              case "plugin_install":
              case "memory_recall":
              case "notification":
              case "api_retry":
              case "mirror_error":
              case "permission_denied":
                // Todo: process via status api: https://docs.claude.com/en/docs/claude-code/hooks#hook-output
                break;
              default:
                unreachable(message, this.logger);
                break;
            }
            break;
          case "result": {
            // Accumulate usage from this result
            session.accumulatedUsage.inputTokens += message.usage.input_tokens;
            session.accumulatedUsage.outputTokens += message.usage.output_tokens;
            session.accumulatedUsage.cachedReadTokens += message.usage.cache_read_input_tokens;
            session.accumulatedUsage.cachedWriteTokens += message.usage.cache_creation_input_tokens;

            const matchingModelUsage = lastAssistantModel
              ? getMatchingModelUsage(message.modelUsage, lastAssistantModel)
              : null;
            // Only overwrite when we have an authoritative value — a miss
            // (e.g. a turn with no top-level assistant message) would
            // otherwise discard the window learned on a prior turn and
            // leave the next prompt's mid-stream updates reporting 200k.
            if (matchingModelUsage) {
              session.contextWindowSize = matchingModelUsage.contextWindow;
            }

            // Task-notification followups are autonomous work triggered by a
            // task-notification system message, not by the user's prompt.
            // They should not influence the user-turn lifecycle (stop reason,
            // slash-command output forwarding) but their cost is real.
            const isTaskNotification = message.origin?.kind === "task-notification";

            // Send usage_update notification
            if (lastAssistantTotalUsage !== null) {
              await this.client.sessionUpdate({
                sessionId: params.sessionId,
                update: {
                  sessionUpdate: "usage_update",
                  used: lastAssistantTotalUsage,
                  size: session.contextWindowSize,
                  cost: {
                    amount: message.total_cost_usd,
                    currency: "USD",
                  },
                  ...(message.origin && {
                    _meta: { "_claude/origin": message.origin },
                  }),
                },
              });
            }

            if (session.cancelled) {
              if (!isTaskNotification) {
                stopReason = "cancelled";
              }
              break;
            }

            switch (message.subtype) {
              case "success": {
                if (message.result.includes("Please run /login")) {
                  throw RequestError.authRequired();
                }
                if (message.stop_reason === "max_tokens") {
                  if (!isTaskNotification) {
                    stopReason = "max_tokens";
                  }
                  break;
                }
                if (message.is_error) {
                  throw RequestError.internalError(
                    errorKindData(lastAssistantError),
                    message.result,
                  );
                }
                if (!isTaskNotification && !session.titleAssistantResult) {
                  session.titleAssistantResult = truncateTitle(
                    message.result,
                    TITLE_SUMMARY_INPUT_LENGTH,
                    "",
                  );
                }
                if (!isTaskNotification) {
                  shouldSyncSessionInfo = true;
                }
                // For local-only commands (no model invocation), the result
                // text is the command output — forward it to the client.
                // Task-notification followups never originate from a user
                // slash command, so skip the forwarding for them.
                if (isLocalOnlyCommand && !isTaskNotification) {
                  for (const notification of toAcpNotifications(
                    message.result,
                    "assistant",
                    params.sessionId,
                    this.toolUseCache,
                    this.client,
                    this.logger,
                  )) {
                    await this.client.sessionUpdate(notification);
                  }
                }
                break;
              }
              case "error_during_execution": {
                if (message.stop_reason === "max_tokens") {
                  if (!isTaskNotification) {
                    stopReason = "max_tokens";
                  }
                  break;
                }
                if (message.is_error) {
                  throw RequestError.internalError(
                    errorKindData(lastAssistantError),
                    message.errors.join(", ") || message.subtype,
                  );
                }
                if (!isTaskNotification) {
                  stopReason = "end_turn";
                }
                break;
              }
              case "error_max_budget_usd":
              case "error_max_turns":
              case "error_max_structured_output_retries":
                if (message.is_error) {
                  throw RequestError.internalError(
                    errorKindData(lastAssistantError),
                    message.errors.join(", ") || message.subtype,
                  );
                }
                if (!isTaskNotification) {
                  stopReason = "max_turn_requests";
                }
                break;
              default:
                unreachable(message, this.logger);
                break;
            }
            break;
          }
          case "stream_event": {
            if (
              message.parent_tool_use_id === null &&
              (message.event.type === "message_start" || message.event.type === "message_delta")
            ) {
              if (message.event.type === "message_start") {
                lastAssistantUsage = snapshotFromUsage(message.event.message.usage);
                const model = message.event.message.model;
                if (model && model !== "<synthetic>") {
                  lastAssistantModel = model;
                  // Only upgrade from the default — once a `result` has given
                  // us an authoritative window, trust it over the heuristic.
                  // Model switches invalidate the cached window via
                  // `syncSessionConfigState`, which resets us back to the
                  // default so this branch runs again for the new model.
                  if (session.contextWindowSize === DEFAULT_CONTEXT_WINDOW) {
                    const inferred = inferContextWindowFromModel(model);
                    if (inferred !== null) {
                      session.contextWindowSize = inferred;
                    }
                  }
                }
              } else {
                const usage = message.event.usage;
                const prev: Readonly<UsageSnapshot> = lastAssistantUsage ?? ZERO_USAGE;
                // Per Anthropic API, message_delta usage fields are *cumulative*;
                // nullable fields (input_tokens and the cache fields) fall back
                // to the prior snapshot when the server omits them from this
                // delta. Only output_tokens is guaranteed non-null.
                lastAssistantUsage = {
                  input_tokens: usage.input_tokens ?? prev.input_tokens,
                  output_tokens: usage.output_tokens,
                  cache_read_input_tokens:
                    usage.cache_read_input_tokens ?? prev.cache_read_input_tokens,
                  cache_creation_input_tokens:
                    usage.cache_creation_input_tokens ?? prev.cache_creation_input_tokens,
                };
              }

              const nextUsage = totalTokens(lastAssistantUsage);
              if (nextUsage !== lastAssistantTotalUsage) {
                lastAssistantTotalUsage = nextUsage;
                await this.client.sessionUpdate({
                  sessionId: params.sessionId,
                  update: {
                    sessionUpdate: "usage_update",
                    used: nextUsage,
                    size: session.contextWindowSize,
                  },
                });
              }
            }
            for (const notification of streamEventToAcpNotifications(
              message,
              params.sessionId,
              this.toolUseCache,
              this.client,
              this.logger,
              {
                clientCapabilities: this.clientCapabilities,
                cwd: session.cwd,
                taskState: session.taskState,
              },
            )) {
              await this.client.sessionUpdate(notification);
            }
            break;
          }
          case "user":
          case "assistant": {
            if (session.cancelled) {
              break;
            }

            // Check for prompt replay
            if (message.type === "user" && "uuid" in message && message.uuid) {
              if (message.uuid === promptUuid) {
                break;
              }

              const pending = session.pendingMessages.get(message.uuid as string);
              if (pending) {
                pending.resolve(false);
                session.pendingMessages.delete(message.uuid as string);
                handedOff = true;
                // the current loop stops with end_turn,
                // the loop of the next prompt continues running
                return { stopReason: "end_turn", usage: sessionUsage(session) };
              }
              if ("isReplay" in message && message.isReplay) {
                // not pending or unrelated replay message
                break;
              }
            }

            // Snapshot the latest top-level assistant usage and model so the
            // next `result` can emit a usage_update tied to the right context
            // window. Subagent messages are excluded to keep the snapshot
            // aligned with what the user's current selection is producing.
            if (message.type === "assistant" && message.parent_tool_use_id === null) {
              lastAssistantUsage = snapshotFromUsage(message.message.usage);
              lastAssistantTotalUsage = totalTokens(lastAssistantUsage);
              if (message.message.model && message.message.model !== "<synthetic>") {
                lastAssistantModel = message.message.model;
              }
              if (message.error) {
                lastAssistantError = message.error;
              }
            }

            // Strip <command-*>/<local-command-stdout> markers and render any
            // remaining prose. Skill bodies and built-in slash commands (e.g.
            // /usage, /status, /model) arrive wrapped in these tags; pure-marker
            // payloads (e.g. /compact's malformed output) strip to null and are
            // skipped. Mirrors the replay path at replaySessionHistory.
            if (
              typeof message.message.content === "string" &&
              message.message.content.includes("<local-command-stdout>")
            ) {
              const stripped = stripLocalCommandMetadata(message.message.content);
              if (typeof stripped === "string") {
                for (const notification of toAcpNotifications(
                  stripped,
                  message.message.role,
                  params.sessionId,
                  this.toolUseCache,
                  this.client,
                  this.logger,
                  {
                    clientCapabilities: this.clientCapabilities,
                    parentToolUseId: message.parent_tool_use_id,
                    cwd: session.cwd,
                    taskState: session.taskState,
                  },
                )) {
                  await this.client.sessionUpdate(notification);
                }
              } else {
                this.logger.log(message.message.content);
              }
              break;
            }

            if (
              typeof message.message.content === "string" &&
              message.message.content.includes("<local-command-stderr>")
            ) {
              this.logger.error(message.message.content);
              break;
            }
            // Skip these user messages for now, since they seem to just be messages we don't want in the feed
            if (
              message.type === "user" &&
              (typeof message.message.content === "string" ||
                (Array.isArray(message.message.content) &&
                  message.message.content.length === 1 &&
                  message.message.content[0].type === "text"))
            ) {
              break;
            }

            if (
              message.type === "assistant" &&
              message.message.model === "<synthetic>" &&
              Array.isArray(message.message.content) &&
              message.message.content.length === 1 &&
              message.message.content[0].type === "text" &&
              message.message.content[0].text.includes("Please run /login")
            ) {
              throw RequestError.authRequired();
            }

            const content =
              message.type === "assistant"
                ? // Handled by stream events above
                  message.message.content.filter(
                    (item) => !["text", "thinking"].includes(item.type),
                  )
                : message.message.content;

            for (const notification of toAcpNotifications(
              content,
              message.message.role,
              params.sessionId,
              this.toolUseCache,
              this.client,
              this.logger,
              {
                clientCapabilities: this.clientCapabilities,
                parentToolUseId: message.parent_tool_use_id,
                cwd: session.cwd,
                taskState: session.taskState,
              },
            )) {
              await this.client.sessionUpdate(notification);
            }
            break;
          }
          case "tool_progress":
          case "tool_use_summary":
          case "auth_status":
          case "prompt_suggestion":
          case "rate_limit_event":
            break;
          default:
            unreachable(message);
            break;
        }
      }
      throw new Error("Session did not end in result");
    } catch (error) {
      if (error instanceof RequestError || !(error instanceof Error)) {
        throw error;
      }
      const message = error.message;
      if (
        message.includes("ProcessTransport") ||
        message.includes("terminated process") ||
        message.includes("process exited with") ||
        message.includes("process terminated by signal") ||
        message.includes("Failed to write to process stdin")
      ) {
        this.logger.error(`Session ${params.sessionId}: Claude Agent process died: ${message}`);
        session.settingsManager.dispose();
        session.input.end();
        delete this.sessions[params.sessionId];
        throw RequestError.internalError(
          undefined,
          "The Claude Agent process exited unexpectedly. Please start a new session.",
        );
      }
      throw error;
    } finally {
      if (!handedOff) {
        session.promptRunning = false;
        // This usually should not happen, but in case the loop finishes
        // without claude sending all message replays, we resolve the
        // next pending prompt call to ensure no prompts get stuck.
        if (session.pendingMessages.size > 0) {
          const next = [...session.pendingMessages.entries()].sort(
            (a, b) => a[1].order - b[1].order,
          )[0];
          if (next) {
            next[1].resolve(false);
            session.pendingMessages.delete(next[0]);
          }
        }
      }
      if (shouldSyncSessionInfo && !session.cancelled) {
        try {
          await this.syncSessionInfoUpdate(params.sessionId, session.cwd, { retry: false });
        } catch (error) {
          this.logger.error(
            `Session ${params.sessionId}: failed to sync Claude session title: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      }
    }
  }

  async cancel(params: CancelNotification): Promise<void> {
    const session = this.sessions[params.sessionId];
    if (!session) {
      return;
    }
    session.cancelled = true;
    for (const [, pending] of session.pendingMessages) {
      pending.resolve(true);
    }
    session.pendingMessages.clear();
    await session.query.interrupt();
  }

  /** Cleanly tear down a session: cancel in-flight work, dispose resources,
   *  and remove it from the session map. */
  private async teardownSession(sessionId: string): Promise<void> {
    const session = this.sessions[sessionId];
    if (!session) {
      return;
    }
    await this.cancel({ sessionId });
    session.settingsManager.dispose();
    session.abortController.abort();
    session.query.close();
    delete this.sessions[sessionId];
  }

  /** Tear down all active sessions. Called when the ACP connection closes. */
  async dispose(): Promise<void> {
    await Promise.all(Object.keys(this.sessions).map((id) => this.teardownSession(id)));
  }

  async closeSession(params: CloseSessionRequest): Promise<CloseSessionResponse> {
    if (!this.sessions[params.sessionId]) {
      throw new Error("Session not found");
    }
    await this.teardownSession(params.sessionId);
    return {};
  }

  async unstable_deleteSession(params: DeleteSessionRequest): Promise<DeleteSessionResponse> {
    // Tear down any active in-memory state first so the on-disk file isn't
    // recreated by an outstanding query writing to it.
    if (this.sessions[params.sessionId]) {
      await this.teardownSession(params.sessionId);
    }
    await deleteSession(params.sessionId);
    return {};
  }

  async unstable_setSessionModel(
    params: SetSessionModelRequest,
  ): Promise<SetSessionModelResponse | void> {
    const session = this.sessions[params.sessionId];
    if (!session) {
      throw new Error("Session not found");
    }
    const requestedModel = decodeProviderValue(params.modelId);
    // Resolve aliases (e.g. "opus", "opus[1m]") to canonical model IDs so
    // downstream lookups in modelInfos succeed and the effort option isn't
    // silently dropped.
    const resolved = resolveModelPreferenceForProvider(
      session.modelInfos,
      requestedModel.value,
      requestedModel.provider,
    );
    const modelId = resolved?.value ?? requestedModel.value;
    await session.query.setModel(modelId);
    await this.updateConfigOption(params.sessionId, "model", modelId);
  }

  async setSessionMode(params: SetSessionModeRequest): Promise<SetSessionModeResponse> {
    if (!this.sessions[params.sessionId]) {
      throw new Error("Session not found");
    }

    await this.applySessionMode(params.sessionId, params.modeId);
    await this.updateConfigOption(params.sessionId, "mode", params.modeId);
    return {};
  }

  async setSessionConfigOption(
    params: SetSessionConfigOptionRequest,
  ): Promise<SetSessionConfigOptionResponse> {
    const session = this.sessions[params.sessionId];
    if (!session) {
      throw new Error("Session not found");
    }
    if (typeof params.value !== "string") {
      throw new Error(`Invalid value for config option ${params.configId}: ${params.value}`);
    }

    const option = session.configOptions.find((o) => o.id === params.configId);
    if (!option) {
      throw new Error(`Unknown config option: ${params.configId}`);
    }

    const allValues =
      "options" in option && Array.isArray(option.options)
        ? option.options.flatMap((o) => ("options" in o ? o.options : [o]))
        : [];
    const requestedValue =
      params.configId === "model" ? decodeProviderValue(params.value) : { value: params.value };
    let validValue = allValues.find(
      (o) =>
        o.value === requestedValue.value &&
        (!requestedValue.provider || optionProvider(o) === requestedValue.provider),
    );
    if (!validValue && !requestedValue.provider) {
      validValue = allValues.find((o) => o.value === requestedValue.value);
    }

    // For model options, fall back to resolveModelPreference when the exact
    // value doesn't match.  This lets callers use human-friendly aliases like
    // "opus" or "sonnet" instead of full model IDs like "claude-opus-4-6".
    if (!validValue && params.configId === "model") {
      const modelInfos: KodexModelInfo[] = allValues.map((o) => ({
        value: o.value,
        displayName: o.name,
        description: o.description ?? "",
        ...(optionProvider(o) && { kodexProvider: optionProvider(o) }),
        ...(optionRouteModel(o) && { kodexRouteModel: optionRouteModel(o) }),
      }));
      const resolved = resolveModelPreferenceForProvider(
        modelInfos,
        requestedValue.value,
        requestedValue.provider,
      );
      if (resolved) {
        validValue = allValues.find(
          (o) =>
            o.value === resolved.value &&
            (!requestedValue.provider || optionProvider(o) === requestedValue.provider),
        );
      }
    }

    if (!validValue) {
      throw new Error(`Invalid value for config option ${params.configId}: ${requestedValue.value}`);
    }

    // Use the canonical option value so downstream code always receives the
    // model ID rather than the caller-supplied alias.
    const resolvedValue = validValue.value;

    if (params.configId === "mode") {
      await this.applySessionMode(params.sessionId, resolvedValue);
      await this.client.sessionUpdate({
        sessionId: params.sessionId,
        update: {
          sessionUpdate: "current_mode_update",
          currentModeId: resolvedValue,
        },
      });
    } else if (params.configId === "model") {
      await this.sessions[params.sessionId].query.setModel(resolvedValue);
    }
    // Effort SDK sync is handled inside applyConfigOptionValue so that direct
    // effort changes and effort changes induced by a model switch go through
    // the same path.

    await this.applyConfigOptionValue(params.sessionId, session, params.configId, resolvedValue);

    return { configOptions: session.configOptions };
  }

  private async applySessionMode(sessionId: string, modeId: string): Promise<void> {
    switch (modeId) {
      case "auto":
      case "default":
      case "acceptEdits":
      case "bypassPermissions":
      case "dontAsk":
      case "plan":
        break;
      default:
        throw new Error("Invalid Mode");
    }

    const session = this.sessions[sessionId];
    if (!session) {
      throw new Error("Session not found");
    }
    if (!session.modes.availableModes.some((mode) => mode.id === modeId)) {
      throw new Error(`Mode ${modeId} is not available in this session`);
    }

    try {
      await session.query.setPermissionMode(modeId);
    } catch (error) {
      if (error instanceof Error) {
        if (!error.message) {
          error.message = "Invalid Mode";
        }
        throw error;
      } else {
        // eslint-disable-next-line preserve-caught-error
        throw new Error("Invalid Mode");
      }
    }
  }

  private async replaySessionHistory(sessionId: string): Promise<void> {
    const toolUseCache: ToolUseCache = {};
    const messages = await getSessionMessages(sessionId);

    for (const message of messages) {
      // @ts-expect-error - untyped in SDK but we handle all of these
      let content: unknown = message.message.content;
      // @ts-expect-error - untyped in SDK but we handle all of these
      if (message.message.role === "user") {
        content = stripLocalCommandMetadata(content);
        if (content === null) continue;
      }

      for (const notification of toAcpNotifications(
        // @ts-expect-error - untyped in SDK but we handle all of these
        content,
        // @ts-expect-error - untyped in SDK but we handle all of these
        message.message.role,
        sessionId,
        toolUseCache,
        this.client,
        this.logger,
        {
          registerHooks: false,
          clientCapabilities: this.clientCapabilities,
          cwd: this.sessions[sessionId]?.cwd,
          taskState: this.sessions[sessionId]?.taskState,
        },
      )) {
        await this.client.sessionUpdate(notification);
      }
    }
  }

  async readTextFile(params: ReadTextFileRequest): Promise<ReadTextFileResponse> {
    const response = await this.client.readTextFile(params);
    return response;
  }

  async writeTextFile(params: WriteTextFileRequest): Promise<WriteTextFileResponse> {
    const response = await this.client.writeTextFile(params);
    return response;
  }

  canUseTool(sessionId: string): CanUseTool {
    return async (toolName, toolInput, { signal, suggestions, toolUseID }) => {
      const alwaysAllowLabel = describeAlwaysAllow(suggestions, toolName);
      const supportsTerminalOutput = this.clientCapabilities?._meta?.["terminal_output"] === true;
      const session = this.sessions[sessionId];
      if (!session) {
        return {
          behavior: "deny",
          message: "Session not found",
        };
      }

      if (toolName === "ExitPlanMode") {
        const optionsAll: PermissionOption[] = [
          { kind: "allow_always", name: 'Yes, and use "auto" mode', optionId: "auto" },
          {
            kind: "allow_always",
            name: "Yes, and auto-accept edits",
            optionId: "acceptEdits",
          },
          { kind: "allow_once", name: "Yes, and manually approve edits", optionId: "default" },
          { kind: "reject_once", name: "No, keep planning", optionId: "plan" },
        ];
        if (ALLOW_BYPASS) {
          optionsAll.unshift({
            kind: "allow_always",
            name: "Yes, and bypass permissions",
            optionId: "bypassPermissions",
          });
        }
        // Filter against the session's currently-advertised modes so we never
        // present options the active model can't honor (e.g. `auto` on Haiku).
        // `bypassPermissions` is already covered by `availableModes` via
        // `buildAvailableModes`/`ALLOW_BYPASS`. The `plan` option is a
        // "keep planning" reject path; it's always present in `availableModes`.
        const options = optionsAll.filter((o) =>
          session.modes.availableModes.some((m) => m.id === o.optionId),
        );

        const response = await this.client.requestPermission({
          options,
          sessionId,
          toolCall: {
            toolCallId: toolUseID,
            rawInput: toolInput,
            ...toolInfoFromToolUse(
              { name: toolName, input: toolInput, id: toolUseID },
              supportsTerminalOutput,
              session?.cwd,
            ),
          },
        });

        if (signal.aborted || response.outcome?.outcome === "cancelled") {
          throw new Error("Tool use aborted");
        }
        const selectedMode =
          response.outcome?.outcome === "selected" ? response.outcome.optionId : undefined;
        const selectedModeWasOffered = options.some((option) => option.optionId === selectedMode);
        if (
          selectedModeWasOffered &&
          (selectedMode === "default" ||
            selectedMode === "acceptEdits" ||
            selectedMode === "auto" ||
            selectedMode === "bypassPermissions")
        ) {
          await this.client.sessionUpdate({
            sessionId,
            update: {
              sessionUpdate: "current_mode_update",
              currentModeId: selectedMode,
            },
          });
          await this.updateConfigOption(sessionId, "mode", selectedMode);

          return {
            behavior: "allow",
            updatedInput: toolInput,
            updatedPermissions: suggestions ?? [
              { type: "setMode", mode: selectedMode, destination: "session" },
            ],
          };
        } else {
          return {
            behavior: "deny",
            message: "User rejected request to exit plan mode.",
          };
        }
      }

      if (toolName === "AskUserQuestion") {
        const questions = normalizeAskUserQuestions(toolInput);
        if (!questions) {
          return {
            behavior: "deny",
            message: "AskUserQuestion input was malformed.",
          };
        }

        const options = askUserQuestionPermissionOptions(questions);
        const response = await this.client.requestPermission({
          options,
          sessionId,
          toolCall: {
            toolCallId: toolUseID,
            rawInput: toolInput,
            ...toolInfoFromToolUse(
              { name: toolName, input: toolInput, id: toolUseID },
              supportsTerminalOutput,
              session?.cwd,
            ),
          },
        });

        if (signal.aborted || response.outcome?.outcome === "cancelled") {
          throw new Error("Tool use aborted");
        }

        const structuredAnswers = permissionInputAnswers(response);
        if (structuredAnswers) {
          return {
            behavior: "allow",
            updatedInput: withAskUserQuestionAnswers(toolInput, questions, structuredAnswers),
          };
        }

        const selectedOptionId =
          response.outcome?.outcome === "selected" ? response.outcome.optionId : undefined;
        const selection = askUserQuestionSelection(selectedOptionId, questions);
        if (!selection) {
          return {
            behavior: "deny",
            message: "User did not select a valid answer.",
          };
        }

        return {
          behavior: "allow",
          updatedInput: withAskUserQuestionAnswer(toolInput, selection, permissionGuidance(response)),
        };
      }

      if (session.modes.currentModeId === "bypassPermissions") {
        return {
          behavior: "allow",
          updatedInput: toolInput,
          updatedPermissions: suggestions ?? [
            { type: "addRules", rules: [{ toolName }], behavior: "allow", destination: "session" },
          ],
        };
      }

      const response = await this.client.requestPermission({
        options: [
          {
            kind: "allow_always",
            name: alwaysAllowLabel,
            optionId: "allow_always",
          },
          { kind: "allow_once", name: "Allow", optionId: "allow" },
          { kind: "reject_once", name: "Reject", optionId: "reject" },
        ],
        sessionId,
        toolCall: {
          toolCallId: toolUseID,
          rawInput: toolInput,
          ...toolInfoFromToolUse(
            { name: toolName, input: toolInput, id: toolUseID },
            supportsTerminalOutput,
            session?.cwd,
          ),
        },
      });
      if (signal.aborted || response.outcome?.outcome === "cancelled") {
        throw new Error("Tool use aborted");
      }
      if (
        response.outcome?.outcome === "selected" &&
        (response.outcome.optionId === "allow" || response.outcome.optionId === "allow_always")
      ) {
        // If Claude Code has suggestions, it will update their settings already
        if (response.outcome.optionId === "allow_always") {
          return {
            behavior: "allow",
            updatedInput: toolInput,
            updatedPermissions: suggestions ?? [
              {
                type: "addRules",
                rules: [{ toolName }],
                behavior: "allow",
                destination: "session",
              },
            ],
          };
        }
        return {
          behavior: "allow",
          updatedInput: toolInput,
        };
      } else {
        return {
          behavior: "deny",
          message: "User refused permission to run tool",
        };
      }
    };
  }

  private async sendAvailableCommandsUpdate(sessionId: string): Promise<void> {
    const session = this.sessions[sessionId];
    if (!session) return;
    const commands = await session.query.supportedCommands();
    await this.client.sessionUpdate({
      sessionId,
      update: {
        sessionUpdate: "available_commands_update",
        availableCommands: getAvailableSlashCommands(commands),
      },
    });
  }

  private async syncSessionInfoUpdate(
    sessionId: string,
    cwd: string,
    options: { retry?: boolean } = {},
  ): Promise<void> {
    let generatedEmitted = false;
    const retryDelays = options.retry === false ? [0] : SESSION_INFO_SYNC_RETRY_DELAYS_MS;
    for (const delayMs of retryDelays) {
      await sleep(delayMs);

      const session = this.sessions[sessionId];
      if (!session) {
        return;
      }

      const info = await getSessionInfo(sessionId, { dir: cwd });
      if (!info) {
        if (!generatedEmitted) {
          generatedEmitted = await this.generateAndSendSessionTitle(sessionId);
        }
        continue;
      }
      const title = titleFromSessionInfo(info);
      if (!title) {
        if (!generatedEmitted) {
          generatedEmitted = await this.generateAndSendSessionTitle(
            sessionId,
            new Date(info.lastModified).toISOString(),
            info,
          );
        }
        continue;
      }

      await this.sendSessionTitleUpdate(
        sessionId,
        title,
        new Date(info.lastModified).toISOString(),
      );
      return;
    }
  }

  private async generateAndSendSessionTitle(
    sessionId: string,
    updatedAt = new Date().toISOString(),
    sessionInfo?: SDKSessionInfo,
  ): Promise<boolean> {
    const title = await this.generateSessionTitle(sessionId, sessionInfo);
    if (!title) {
      return false;
    }
    await this.sendSessionTitleUpdate(sessionId, title, updatedAt);
    return true;
  }

  private async generateSessionTitle(
    sessionId: string,
    sessionInfo?: SDKSessionInfo,
  ): Promise<string | null> {
    const session = this.sessions[sessionId];
    if (!session || session.generatedTitle) {
      return session?.generatedTitle ?? null;
    }
    const titlePrompt = session.titlePrompt ?? titleInputFromSessionInfo(sessionInfo);
    if (session.titleSummaryStarted || !titlePrompt) {
      return null;
    }
    session.titleSummaryStarted = true;

    const description = [
      "User request:",
      titlePrompt,
      ...(session.titleAssistantResult
        ? ["", "Assistant result:", session.titleAssistantResult]
        : []),
    ].join("\n");

    const liveTitle = await this.generateLiveSessionTitle(sessionId, session, description);
    if (liveTitle) {
      return liveTitle;
    }
    if (this.woaConfig?.enabled) {
      const sideQuestionTitle = await this.generateLiveSideQuestionSessionTitle(
        sessionId,
        session,
        titlePrompt,
      );
      if (sideQuestionTitle) {
        return sideQuestionTitle;
      }
      session.titleSummaryStarted = false;
      return null;
    }

    const prompt = [
      "Generate a concise conversation title for a coding agent session.",
      "Return only the title, with no quotes, no markdown, and no extra text.",
      "Use the same language as the user's request when possible.",
      "Keep it under 12 words.",
      "",
      "<user_request>",
      titlePrompt,
      "</user_request>",
      "",
      "<assistant_result>",
      session.titleAssistantResult ?? "",
      "</assistant_result>",
    ].join("\n");

    let titleQuery: Query | null = null;
    try {
      const woaEnv = this.woaConfig?.enabled
        ? buildWoaEnv({
            token: await ensureWoaToken(this.woaConfig),
            config: this.woaConfig,
            conversationId: `${sessionId}-title`,
          })
        : {};
      const model =
        session.models.currentModelId && session.models.currentModelId !== "default"
          ? session.models.currentModelId
          : undefined;
      titleQuery = query({
        prompt,
        options: {
          cwd: session.cwd,
          env: mergeEnv(process.env, woaEnv),
          maxTurns: 1,
          model,
          permissionMode: "dontAsk",
          persistSession: false,
          pathToClaudeCodeExecutable: process.env.CLAUDE_CODE_EXECUTABLE ?? (await claudeCliPath()),
          systemPrompt:
            "You write short, specific titles for coding conversations. Reply with only the title.",
          tools: [],
        },
      });

      for await (const message of titleQuery) {
        if (message.type !== "result" || message.subtype !== "success" || message.is_error) {
          continue;
        }
        const title = cleanGeneratedTitle(message.result);
        if (!title) {
          session.titleSummaryStarted = false;
          return null;
        }
        session.generatedTitle = title;
        void renameSession(sessionId, title, { dir: session.cwd }).catch((error) => {
          this.logger.error(
            `Session ${sessionId}: failed to persist generated Claude session title: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        });
        return title;
      }
    } catch (error) {
      this.logger.error(
        `Session ${sessionId}: failed to generate Claude session title: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    } finally {
      titleQuery?.close();
    }

    session.titleSummaryStarted = false;
    return null;
  }

  private async generateLiveSessionTitle(
    sessionId: string,
    session: Session,
    description: string,
  ): Promise<string | null> {
    const generateSessionTitle = (session.query as QueryWithTitleHelpers).generateSessionTitle;
    if (typeof generateSessionTitle !== "function") {
      return null;
    }

    try {
      const title = cleanGeneratedTitle(
        await generateSessionTitle.call(session.query, description, { persist: true }),
      );
      if (!title) {
        return null;
      }
      session.generatedTitle = title;
      return title;
    } catch (error) {
      this.logger.error(
        `Session ${sessionId}: failed to generate Claude session title via live session: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      return null;
    }
  }

  private async generateLiveSideQuestionSessionTitle(
    sessionId: string,
    session: Session,
    titlePrompt: string,
  ): Promise<string | null> {
    const askSideQuestion = (session.query as QueryWithTitleHelpers).askSideQuestion;
    if (typeof askSideQuestion !== "function") {
      return null;
    }

    const question = [
      "Generate a concise conversation title for this coding agent session.",
      "Return only the title, with no quotes, no markdown, and no extra text.",
      "Use the same language as the user's request when possible.",
      "Keep it under 12 words.",
      "",
      "<user_request>",
      titlePrompt,
      "</user_request>",
      "",
      "<assistant_result>",
      session.titleAssistantResult ?? "",
      "</assistant_result>",
    ].join("\n");

    try {
      const response = await askSideQuestion.call(session.query, question);
      const title = cleanGeneratedTitle(response?.response);
      if (!title) {
        return null;
      }
      session.generatedTitle = title;
      try {
        await renameSession(sessionId, title, { dir: session.cwd });
      } catch (error) {
        this.logger.error(
          `Session ${sessionId}: failed to persist generated Claude session title: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      return title;
    } catch (error) {
      this.logger.error(
        `Session ${sessionId}: failed to generate Claude session title via side question: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      return null;
    }
  }

  private async sendSessionTitleUpdate(
    sessionId: string,
    title: string,
    updatedAt: string,
  ): Promise<void> {
    const session = this.sessions[sessionId];
    if (!session || session.lastEmittedTitle === title) {
      return;
    }
    session.lastEmittedTitle = title;
    await this.client.sessionUpdate({
      sessionId,
      update: {
        sessionUpdate: "session_info_update",
        title,
        updatedAt,
      },
    });
  }

  private async updateConfigOption(
    sessionId: string,
    configId: string,
    value: string,
  ): Promise<void> {
    const session = this.sessions[sessionId];
    if (!session) return;

    await this.applyConfigOptionValue(sessionId, session, configId, value);

    await this.client.sessionUpdate({
      sessionId,
      update: {
        sessionUpdate: "config_option_update",
        configOptions: session.configOptions,
      },
    });
  }

  private async applyConfigOptionValue(
    sessionId: string,
    session: Session,
    configId: string,
    value: string,
  ): Promise<void> {
    if (configId === "mode") {
      session.modes = { ...session.modes, currentModeId: value };
      session.configOptions = session.configOptions.map((o) =>
        o.id === configId && typeof o.currentValue === "string" ? { ...o, currentValue: value } : o,
      );
    } else if (configId === "model") {
      if (session.models.currentModelId !== value) {
        // The cached context window was learned for the previous model; reset
        // to the new model's heuristic so mid-stream updates between now and
        // the next `result` reflect the user's selection instead of the old
        // model's window.
        session.contextWindowSize = inferContextWindowFromModel(value) ?? DEFAULT_CONTEXT_WINDOW;
      }
      session.models = { ...session.models, currentModelId: value };

      // Recompute availableModes for the new model and clamp the current
      // mode if the SDK no longer offers it (today: "auto" on Haiku).
      // `ModelInfo.supportsAutoMode` is the canonical SDK signal.
      const newModelInfo = session.modelInfos.find((m) => m.value === value);
      const newAvailableModes = buildAvailableModes(newModelInfo);
      // Capture BEFORE mutating session.modes so the log message reflects
      // the invalidated mode rather than "default".
      const previousModeId = session.modes.currentModeId;
      let modeDowngraded = false;
      if (!newAvailableModes.some((m) => m.id === previousModeId)) {
        session.modes = {
          availableModes: newAvailableModes,
          currentModeId: "default",
        };
        try {
          await session.query.setPermissionMode("default");
        } catch (err) {
          // Failing the entire model switch over a bookkeeping sync error is
          // worse UX than logging and continuing; the user explicitly asked
          // to change models. The next setPermissionMode from the user will
          // either succeed or surface a fresh error.
          this.logger.error(
            `Failed to sync permissionMode to "default" after model switch invalidated "${previousModeId}":`,
            err,
          );
        }
        modeDowngraded = true;
      } else {
        session.modes = { ...session.modes, availableModes: newAvailableModes };
      }

      // Rebuild config options since effort levels depend on the selected model
      const effortOpt = session.configOptions.find((o) => o.id === "effort");
      const currentEffort =
        typeof effortOpt?.currentValue === "string" ? effortOpt.currentValue : undefined;
      session.configOptions = buildConfigOptions(
        session.modes,
        session.models,
        session.modelInfos,
        currentEffort,
      );

      // Sync effort with the SDK if it changed after the model switch
      const newEffortOpt = session.configOptions.find((o) => o.id === "effort");
      const newEffort =
        typeof newEffortOpt?.currentValue === "string" ? newEffortOpt.currentValue : undefined;
      if (newEffort !== currentEffort) {
        await session.query.applyFlagSettings({
          effortLevel: newEffort as Settings["effortLevel"],
        });
      }

      // Emit current_mode_update only after session.modes AND
      // session.configOptions have been fully reconciled. This way, a failure
      // in the configOptions/effort rebuild above can't leave the client with
      // a clamped currentModeId but stale configOptions, and the notification
      // still precedes the caller's config_option_update so order-sensitive
      // clients update currentModeId before re-rendering the option list.
      if (modeDowngraded) {
        await this.client.sessionUpdate({
          sessionId,
          update: {
            sessionUpdate: "current_mode_update",
            currentModeId: "default",
          },
        });
      }
    } else {
      session.configOptions = session.configOptions.map((o) =>
        o.id === configId && typeof o.currentValue === "string" ? { ...o, currentValue: value } : o,
      );
      if (configId === "effort") {
        await session.query.applyFlagSettings({
          effortLevel: value as Settings["effortLevel"],
        });
      }
    }
  }

  private async getOrCreateSession(params: {
    sessionId: string;
    cwd: string;
    mcpServers?: NewSessionRequest["mcpServers"];
    additionalDirectories?: NewSessionRequest["additionalDirectories"];
    _meta?: NewSessionRequest["_meta"];
  }): Promise<NewSessionResponse> {
    const existingSession = this.sessions[params.sessionId];
    if (existingSession) {
      const fingerprint = computeSessionFingerprint(params);
      if (fingerprint === existingSession.sessionFingerprint) {
        return {
          sessionId: params.sessionId,
          modes: existingSession.modes,
          models: existingSession.models,
          configOptions: existingSession.configOptions,
        };
      }

      // Session-defining params changed (e.g. cwd pointed at a git worktree,
      // or MCP servers reconfigured). Tear down the existing session and
      // recreate it so the underlying Query process picks up the new values.
      await this.teardownSession(params.sessionId);
    }

    const response = await this.createSession(
      {
        cwd: params.cwd,
        mcpServers: params.mcpServers ?? [],
        additionalDirectories: params.additionalDirectories,
        _meta: params._meta,
      },
      {
        resume: params.sessionId,
      },
    );

    return {
      sessionId: response.sessionId,
      modes: response.modes,
      models: response.models,
      configOptions: response.configOptions,
    };
  }

  private async createSession(
    params: NewSessionRequest,
    creationOpts: { resume?: string; forkSession?: boolean } = {},
  ): Promise<NewSessionResponse> {
    // We want to create a new session id unless it is resume,
    // but not resume + forkSession.
    let sessionId;
    if (creationOpts.forkSession) {
      sessionId = randomUUID();
    } else if (creationOpts.resume) {
      sessionId = creationOpts.resume;
    } else {
      sessionId = randomUUID();
    }

    const input = new Pushable<SDKUserMessage>();

    const settingsManager = new SettingsManager(params.cwd, {
      logger: this.logger,
    });
    await settingsManager.initialize();

    const mcpServers: Record<string, McpServerConfig> = {};
    if (Array.isArray(params.mcpServers)) {
      for (const server of params.mcpServers) {
        if ("type" in server && (server.type === "http" || server.type === "sse")) {
          // HTTP or SSE type MCP server
          mcpServers[server.name] = {
            type: server.type,
            url: server.url,
            headers: server.headers
              ? Object.fromEntries(server.headers.map((e) => [e.name, e.value]))
              : undefined,
          };
        } else if (!("type" in server)) {
          // Stdio type MCP server (with or without explicit type field)
          mcpServers[server.name] = {
            type: "stdio",
            command: server.command,
            args: server.args,
            env: server.env
              ? Object.fromEntries(server.env.map((e) => [e.name, e.value]))
              : undefined,
          };
        }
      }
    }

    let systemPrompt: Options["systemPrompt"] = { type: "preset", preset: "claude_code" };
    if (params._meta?.systemPrompt) {
      const customPrompt = params._meta.systemPrompt;
      if (typeof customPrompt === "string") {
        systemPrompt = customPrompt;
      } else if (
        typeof customPrompt === "object" &&
        customPrompt !== null &&
        !Array.isArray(customPrompt)
      ) {
        // Forward all preset options (append, excludeDynamicSections, and
        // anything the SDK adds later) while locking type/preset.
        systemPrompt = {
          ...(customPrompt as object),
          type: "preset",
          preset: "claude_code",
        } as Options["systemPrompt"];
      }
    }

    const permissionMode = resolvePermissionMode(
      settingsManager.getSettings().permissions?.defaultMode,
      this.logger,
    );

    // Extract options from _meta if provided
    const sessionMeta = params._meta as NewSessionMeta | undefined;
    const userProvidedOptions = sessionMeta?.claudeCode?.options;

    // Configure thinking tokens from environment variable
    const maxThinkingTokens = process.env.MAX_THINKING_TOKENS
      ? parseInt(process.env.MAX_THINKING_TOKENS, 10)
      : undefined;

    // Parse model configuration from environment (e.g. Bedrock model overrides)
    const modelConfig = parseModelConfig(process.env.CLAUDE_MODEL_CONFIG);
    const modelProviderEntries = parseModelProviderMap(process.env[KODEX_MODEL_PROVIDER_MAP_ENV]);

    // Resolve which built-in tools to expose.
    // Explicit tools array from _meta.claudeCode.options takes precedence.
    // disableBuiltInTools is a legacy shorthand for tools: [] — kept for
    // backward compatibility but callers should prefer the tools array.
    const tools: Options["tools"] =
      userProvidedOptions?.tools ??
      (params._meta?.disableBuiltInTools === true ? [] : { type: "preset", preset: "claude_code" });

    const abortController = userProvidedOptions?.abortController || new AbortController();

    // Per-session task state. Created here (rather than in the session record
    // below) so the TaskCreated/TaskCompleted hook callbacks can close over
    // the same Map that the streaming message handler will read from.
    const taskState: TaskState = new Map();
    const woaEnv = this.woaConfig?.enabled
      ? buildWoaEnv({
          token: await ensureWoaToken(this.woaConfig),
          config: this.woaConfig,
          conversationId: sessionId,
        })
      : {};

    const options: Options = {
      systemPrompt,
      settingSources: ["user", "project", "local"],
      ...(maxThinkingTokens !== undefined && { maxThinkingTokens }),
      ...userProvidedOptions,
      // CLAUDE_MODEL_CONFIG env var is a fallback for model
      // configuration (e.g. Bedrock model ID overrides). When the caller
      // provides settings via _meta, we intentionally ignore the env var —
      // the caller is assumed to have full control over model configuration.
      ...(!userProvidedOptions?.settings &&
        modelConfig && {
          settings: {
            ...(modelConfig.modelOverrides && { modelOverrides: modelConfig.modelOverrides }),
            ...(modelConfig.availableModels && { availableModels: modelConfig.availableModels }),
            ...(modelConfig.preserveDefaultModel !== undefined && {
              preserveDefaultModel: modelConfig.preserveDefaultModel,
            }),
          },
        }),
      env: mergeEnv(
        process.env,
        userProvidedOptions?.env,
        createEnvForGateway(this.gatewayAuthRequest),
        woaEnv,
        // Opt-in to session state events like when the agent is idle
        { CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1" },
      ),
      // Override certain fields that must be controlled by ACP
      cwd: params.cwd,
      includePartialMessages: true,
      mcpServers: { ...(userProvidedOptions?.mcpServers || {}), ...mcpServers },
      // If we want bypassPermissions to be an option, we have to allow it here.
      // But it doesn't work in root mode, so we only activate it if it will work.
      allowDangerouslySkipPermissions: ALLOW_BYPASS,
      permissionMode,
      canUseTool: this.canUseTool(sessionId),
      pathToClaudeCodeExecutable: process.env.CLAUDE_CODE_EXECUTABLE ?? (await claudeCliPath()),
      extraArgs: {
        ...userProvidedOptions?.extraArgs,
        "replay-user-messages": "",
      },
      tools,
      hooks: {
        ...userProvidedOptions?.hooks,
        PostToolUse: [
          ...(userProvidedOptions?.hooks?.PostToolUse || []),
          {
            hooks: [
              createPostToolUseHook(this.logger, {
                onEnterPlanMode: async () => {
                  await this.client.sessionUpdate({
                    sessionId,
                    update: {
                      sessionUpdate: "current_mode_update",
                      currentModeId: "plan",
                    },
                  });
                  await this.updateConfigOption(sessionId, "mode", "plan");
                },
              }),
            ],
          },
        ],
        TaskCreated: [
          ...(userProvidedOptions?.hooks?.TaskCreated || []),
          {
            hooks: [
              createTaskHook({
                taskState,
                onChange: async () => {
                  await this.client.sessionUpdate({
                    sessionId,
                    update: {
                      sessionUpdate: "plan",
                      entries: taskStateToPlanEntries(taskState),
                    },
                  });
                },
              }),
            ],
          },
        ],
        TaskCompleted: [
          ...(userProvidedOptions?.hooks?.TaskCompleted || []),
          {
            hooks: [
              createTaskHook({
                taskState,
                onChange: async () => {
                  await this.client.sessionUpdate({
                    sessionId,
                    update: {
                      sessionUpdate: "plan",
                      entries: taskStateToPlanEntries(taskState),
                    },
                  });
                },
              }),
            ],
          },
        ],
      },
      ...creationOpts,
      abortController,
    };

    // Prefer the official ACP `additionalDirectories` field. Fall back to the
    // legacy `_meta.additionalRoots` extension for clients that haven't been
    // updated yet. Either source is merged with directories supplied via
    // `_meta.claudeCode.options.additionalDirectories` (SDK pass-through).
    const acpAdditionalDirectories =
      params.additionalDirectories ?? sessionMeta?.additionalRoots ?? [];
    options.additionalDirectories = [
      ...(userProvidedOptions?.additionalDirectories ?? []),
      ...acpAdditionalDirectories,
    ];

    if (creationOpts?.resume === undefined || creationOpts?.forkSession) {
      // Set our own session id if not resuming an existing session.
      options.sessionId = sessionId;
    }

    // Handle abort controller from meta options
    if (abortController?.signal.aborted) {
      throw new Error("Cancelled");
    }

    const q = query({
      prompt: input,
      options,
    });

    let initializationResult;
    try {
      initializationResult = await q.initializationResult();
    } catch (error) {
      if (
        creationOpts.resume &&
        error instanceof Error &&
        (error.message === "Query closed before response received" ||
          error.message.includes("No conversation found with session ID"))
      ) {
        throw RequestError.resourceNotFound(sessionId);
      }
      throw error;
    }

    if (
      shouldHideClaudeAuth() &&
      initializationResult.account.subscriptionType &&
      !this.gatewayAuthRequest
    ) {
      throw RequestError.authRequired(
        undefined,
        "This integration does not support using claude.ai subscriptions.",
      );
    }

    // Apply user's `availableModels` allowlist from settings.json before any
    // downstream model handling. The SDK only enforces this allowlist in its
    // own UI, not in `initializationResult.models`, so we filter here to keep
    // configOptions, the current-model resolver, and the stored modelInfos
    // consistent with what the user configured.
    const settingsAvailableModels = mergeAvailableModelLists(
      settingsManager.getSettings().availableModels,
      !userProvidedOptions?.settings ? modelConfig?.availableModels : undefined,
    );
    const allowedModels = settingsAvailableModels
      ? applyAvailableModelsAllowlist(
          initializationResult.models,
          settingsAvailableModels,
          modelConfig?.preserveDefaultModel !== false,
        )
      : initializationResult.models;
    const routedModels = applyModelProviderMap(allowedModels, modelProviderEntries);

    const models = await getAvailableModels(q, routedModels, settingsManager, this.logger);

    // Gate `auto` (and future model-specific modes) on the resolved model's
    // `ModelInfo`. See `buildAvailableModes` for the canonical SDK signal.
    const currentModelInfo = routedModels.find((m) => m.value === models.currentModelId);
    const availableModes = buildAvailableModes(currentModelInfo);

    // Clamp `permissionMode` if the resolved session does not offer it. The
    // common case is `permissions.defaultMode: "auto"` resolving to a model
    // that does not support auto mode (e.g. Haiku); without this clamp the
    // SDK would later throw `"auto mode unavailable for this model"` from
    // `setPermissionMode`. Keep `permissionMode` as the resolved user intent
    // (matches what was passed into `options.permissionMode` above) and use
    // `effectiveMode` for the post-clamp value the session actually runs in.
    let effectiveMode: PermissionMode = permissionMode;
    if (!availableModes.some((m) => m.id === effectiveMode)) {
      if (effectiveMode === "auto") {
        this.logger.error(
          `permissions.defaultMode "auto" is not available for model ` +
            `"${models.currentModelId}"; falling back to "default".`,
        );
      } else {
        this.logger.error(
          `permissions.defaultMode "${effectiveMode}" is not available in ` +
            `this session; falling back to "default".`,
        );
      }
      effectiveMode = "default";
      // Sync the SDK so it doesn't keep "auto" cached internally. Wrapped in
      // try/catch since failing here would abort session creation entirely.
      try {
        await q.setPermissionMode("default");
      } catch (err) {
        this.logger.error("Failed to sync clamped permissionMode to SDK:", err);
      }
    }

    const modes = {
      currentModeId: effectiveMode,
      availableModes,
    };

    const configOptions = buildConfigOptions(
      modes,
      models,
      routedModels,
      settingsManager.getSettings().effortLevel,
    );

    // Apply the initial effort level to the SDK so it matches the UI default
    const initialEffort = configOptions.find((o) => o.id === "effort");
    if (initialEffort && typeof initialEffort.currentValue === "string") {
      await q.applyFlagSettings({
        effortLevel: initialEffort.currentValue as Settings["effortLevel"],
      });
    }

    this.sessions[sessionId] = {
      query: q,
      input: input,
      cancelled: false,
      cwd: params.cwd,
      sessionFingerprint: computeSessionFingerprint(params),
      settingsManager,
      accumulatedUsage: {
        inputTokens: 0,
        outputTokens: 0,
        cachedReadTokens: 0,
        cachedWriteTokens: 0,
      },
      modes,
      models,
      modelInfos: routedModels,
      configOptions,
      promptRunning: false,
      pendingMessages: new Map(),
      nextPendingOrder: 0,
      abortController,
      emitRawSDKMessages: sessionMeta?.claudeCode?.emitRawSDKMessages ?? false,
      contextWindowSize:
        inferContextWindowFromModel(models.currentModelId) ?? DEFAULT_CONTEXT_WINDOW,
      taskState,
    };

    return {
      sessionId,
      models,
      modes,
      configOptions,
    };
  }
}

function shouldEmitRawMessage(
  config: boolean | SDKMessageFilter[],
  message: { type: string; subtype?: string; origin?: SDKMessageOrigin },
): boolean {
  if (config === true) return true;
  if (config === false) return false;
  return config.some(
    (f) =>
      f.type === message.type &&
      (f.subtype === undefined || f.subtype === message.subtype) &&
      (f.origin === undefined || f.origin === message.origin?.kind),
  );
}

function sessionUsage(session: Session) {
  return {
    inputTokens: session.accumulatedUsage.inputTokens,
    outputTokens: session.accumulatedUsage.outputTokens,
    cachedReadTokens: session.accumulatedUsage.cachedReadTokens,
    cachedWriteTokens: session.accumulatedUsage.cachedWriteTokens,
    totalTokens:
      session.accumulatedUsage.inputTokens +
      session.accumulatedUsage.outputTokens +
      session.accumulatedUsage.cachedReadTokens +
      session.accumulatedUsage.cachedWriteTokens,
  };
}

/** Sum all four fields as a proxy for post-turn context occupancy: the current
 *  turn's output becomes next turn's input. Per the Anthropic API, input_tokens
 *  excludes cache tokens — cache_read and cache_creation are reported
 *  separately — so summing all four is not double-counting. */
function totalTokens(usage: UsageSnapshot): number {
  return (
    usage.input_tokens +
    usage.output_tokens +
    usage.cache_read_input_tokens +
    usage.cache_creation_input_tokens
  );
}

/**
 * Build the `data` payload attached to a `RequestError.internalError` when we
 * have a categorical error from the Claude SDK. Returns `undefined` when no
 * categorical error is available, matching the previous behavior of passing
 * `undefined` to `RequestError.internalError`.
 *
 * The `errorKind` field is a convention for ACP clients to dispatch on
 * without having to pattern-match the human-readable message text. Clients
 * that don't understand it fall back to the existing message-based rendering.
 */
function errorKindData(
  errorKind: SDKAssistantMessageError | undefined,
): { errorKind: SDKAssistantMessageError } | undefined {
  return errorKind ? { errorKind } : undefined;
}

/** Project a nullable API usage object into our non-null snapshot shape.
 *  Both SDK message_start and assistant message `usage` have `number | null`
 *  cache fields; we coerce absent values to 0 so `totalTokens` never hits
 *  NaN. `input_tokens`/`output_tokens` are typed `number` by the SDK but
 *  synthetic or third-party-backend stream events have been observed emitting
 *  them as null/undefined — coerce those too so a malformed upstream event
 *  can't leak NaN into the wire `used` field. Delta events have different
 *  semantics (cumulative + prev fallback) and are handled inline. */
function snapshotFromUsage(usage: {
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_read_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
}): UsageSnapshot {
  return {
    input_tokens: usage.input_tokens ?? 0,
    output_tokens: usage.output_tokens ?? 0,
    cache_read_input_tokens: usage.cache_read_input_tokens ?? 0,
    cache_creation_input_tokens: usage.cache_creation_input_tokens ?? 0,
  };
}

function createEnvForGateway(request?: GatewayAuthRequest) {
  if (!request?._meta) {
    return {};
  }
  const customHeaders = Object.entries(request._meta.gateway.headers)
    .map(([key, value]) => `${key}: ${value}`)
    .join("\n");

  if (request.methodId === "gateway-bedrock") {
    return {
      CLAUDE_CODE_USE_BEDROCK: "1",
      AWS_BEARER_TOKEN_BEDROCK: " ", // Must be non-empty to bypass pass configuration check
      ANTHROPIC_BEDROCK_BASE_URL: request._meta.gateway.baseUrl,
      ANTHROPIC_CUSTOM_HEADERS: customHeaders,
    };
  }
  return {
    ANTHROPIC_BASE_URL: request._meta.gateway.baseUrl,
    ANTHROPIC_CUSTOM_HEADERS: customHeaders,
    ANTHROPIC_AUTH_TOKEN: " ", // Must be specified to bypass claude login requirement
  };
}

/**
 * Build the list of permission modes the agent will advertise for the given
 * model. `auto` is gated by `ModelInfo.supportsAutoMode === true`, which is
 * the SDK's model-level availability signal. `undefined`/`false` both exclude
 * `auto`. `bypassPermissions` is still gated by `ALLOW_BYPASS`.
 */
function buildAvailableModes(modelInfo: ModelInfo | undefined): SessionModeState["availableModes"] {
  const modes: SessionModeState["availableModes"] = [];

  // Only advertise "auto" when the SDK reports the model supports it.
  if (modelInfo?.supportsAutoMode === true) {
    modes.push({
      id: "auto",
      name: "Auto",
      description: "Use a model classifier to approve/deny permission prompts",
    });
  }

  modes.push(
    {
      id: "default",
      name: "Default",
      description: "Standard behavior, prompts for dangerous operations",
    },
    {
      id: "acceptEdits",
      name: "Accept Edits",
      description: "Auto-accept file edit operations",
    },
    {
      id: "plan",
      name: "Plan Mode",
      description: "Planning mode, no actual tool execution",
    },
    {
      id: "dontAsk",
      name: "Don't Ask",
      description: "Don't prompt for permissions, deny if not pre-approved",
    },
  );

  if (ALLOW_BYPASS) {
    modes.push({
      id: "bypassPermissions",
      name: "Bypass Permissions",
      description: "Bypass all permission checks",
    });
  }

  return modes;
}

function buildConfigOptions(
  modes: SessionModeState,
  models: SessionModelState,
  modelInfos: ModelInfo[],
  currentEffortLevel?: string,
): SessionConfigOption[] {
  const options: SessionConfigOption[] = [
    {
      id: "mode",
      name: "Mode",
      description: "Session permission mode",
      category: "mode",
      type: "select",
      currentValue: modes.currentModeId,
      options: modes.availableModes.map((m) => ({
        value: m.id,
        name: m.name,
        description: m.description,
      })),
    },
    {
      id: "model",
      name: "Model",
      description: "AI model to use",
      category: "model",
      type: "select",
      currentValue: models.currentModelId,
      options: models.availableModels.map((m) => modelConfigOption(m)),
    },
  ];

  // Add effort level option based on the currently selected model
  const currentModelInfo = modelInfos.find((m) => m.value === models.currentModelId);
  const supportedLevels = currentModelInfo?.supportsEffort
    ? (currentModelInfo.supportedEffortLevels ?? [])
    : [];

  if (supportedLevels.length > 0) {
    const effortOptions = supportedLevels.map((level) => ({
      value: level,
      name: level
        .split(/[_-]/)
        .map((part) => (part ? part.charAt(0).toUpperCase() + part.slice(1) : part))
        .join(" "),
    }));

    // Keep the current level if valid, otherwise prefer xhigh (Claude Code's
    // recommended default for capable models), then high (the API default).
    const includes = (l: string) => (supportedLevels as string[]).includes(l);
    const validEffort =
      currentEffortLevel && includes(currentEffortLevel)
        ? currentEffortLevel
        : includes("xhigh")
          ? "xhigh"
          : includes("high")
            ? "high"
            : supportedLevels[0];

    options.push({
      id: "effort",
      name: "Effort",
      description: "Available effort levels for this model",
      category: "thought_level",
      type: "select",
      currentValue: validEffort,
      options: effortOptions,
    });
  }

  return options;
}

// Claude Code CLI persists display strings like "opus[1m]" in settings,
// but the SDK model list uses IDs like "claude-opus-4-6-1m".
const MODEL_CONTEXT_HINT_PATTERN = /\[(\d+m)\]$/i;

function tokenizeModelPreference(model: string): { tokens: string[]; contextHint?: string } {
  const lower = model.trim().toLowerCase();
  const contextHint = lower.match(MODEL_CONTEXT_HINT_PATTERN)?.[1]?.toLowerCase();

  const normalized = lower.replace(MODEL_CONTEXT_HINT_PATTERN, " $1 ");
  const rawTokens = normalized.split(/[^a-z0-9]+/).filter(Boolean);
  const tokens = rawTokens
    .map((token) => {
      if (token === "opusplan") return "opus";
      if (token === "best" || token === "default") return "";
      return token;
    })
    .filter((token) => token && token !== "claude")
    .filter((token) => /[a-z]/.test(token) || token.endsWith("m"));

  return { tokens, contextHint };
}

function scoreModelMatch(model: ModelInfo, tokens: string[], contextHint?: string): number {
  const haystack = `${model.value} ${model.displayName}`.toLowerCase();
  let score = 0;
  for (const token of tokens) {
    if (haystack.includes(token)) {
      score += token === contextHint ? 3 : 1;
    }
  }
  return score;
}

function resolveModelPreference(models: ModelInfo[], preference: string): ModelInfo | null {
  const trimmed = preference.trim();
  if (!trimmed) return null;

  const lower = trimmed.toLowerCase();

  // Exact match on value or display name
  const directMatch = models.find(
    (model) =>
      model.value === trimmed ||
      model.value.toLowerCase() === lower ||
      model.displayName.toLowerCase() === lower,
  );
  if (directMatch) return directMatch;

  // Substring match
  const includesMatch = models.find((model) => {
    const value = model.value.toLowerCase();
    const display = model.displayName.toLowerCase();
    return value.includes(lower) || display.includes(lower) || lower.includes(value);
  });
  if (includesMatch) return includesMatch;

  // Tokenized matching for aliases like "opus[1m]"
  const { tokens, contextHint } = tokenizeModelPreference(trimmed);
  if (tokens.length === 0) return null;

  let bestMatch: ModelInfo | null = null;
  let bestScore = 0;
  for (const model of models) {
    const score = scoreModelMatch(model, tokens, contextHint);
    if (0 < score && (!bestMatch || bestScore < score)) {
      bestMatch = model;
      bestScore = score;
    }
  }

  return bestMatch;
}

function resolveSettingsModel(
  models: ModelInfo[],
  settingsModel: unknown,
  logger: Logger,
): ModelInfo | null {
  if (settingsModel === undefined) {
    return null;
  }
  if (typeof settingsModel !== "string") {
    const typeLabel = settingsModel === null ? "null" : typeof settingsModel;
    logger.error(`Ignoring model from settings: expected a string, got ${typeLabel}.`);
    return null;
  }
  return resolveModelPreference(models, settingsModel);
}

/**
 * Restrict the SDK's model list to the user's `availableModels` allowlist
 * (already merged-and-deduped across settings sources by `SettingsManager`).
 * The user's exact entries become the model IDs surfaced via configOptions
 * and passed to `setModel`, which prevents Claude Code from silently
 * substituting a date-pinned variant (e.g. `haiku` →
 * `claude-haiku-4-5-20251001`) that the user may not have access to.
 *
 * For regular Claude allowlists, display info and capability flags are copied
 * from the closest SDK match so the UI still renders sensible names and effort
 * levels. External model pools opt out of the Default model; in that mode the
 * allowlist's exact entries are also used as display names so SDK alias
 * matching cannot make different BYOK models appear under the same label.
 *
 * Semantics from https://code.claude.com/docs/en/model-config#restrict-model-selection:
 * - `undefined` is handled by the caller (no allowlist applied).
 * - The Default option is unaffected by `availableModels` — it always remains
 *   available, even when the allowlist is `[]`.
 */
function applyAvailableModelsAllowlist(
  sdkModels: ModelInfo[],
  allowlist: string[],
  preserveDefaultModel = true,
): ModelInfo[] {
  // Default is always preserved per the docs. Synthesize one if the SDK
  // didn't surface it so downstream code (e.g. `getAvailableModels` picking
  // `models[0]` as a fallback) still has something to work with.
  const defaultModel = sdkModels.find((m) => m.value === "default") ?? {
    value: "default",
    displayName: "Default",
    description: "",
  };
  const result: ModelInfo[] = preserveDefaultModel ? [defaultModel] : [];
  const seen = new Set<string>(preserveDefaultModel ? [defaultModel.value] : []);

  const sdkModelsWithoutDefault = sdkModels.filter((m) => m.value !== "default");

  for (const entry of allowlist) {
    const trimmed = entry.trim();
    if (!trimmed || seen.has(trimmed)) continue;

    const sdkMatch = resolveModelPreference(sdkModelsWithoutDefault, trimmed);
    if (sdkMatch && preserveDefaultModel) {
      result.push({ ...sdkMatch, value: trimmed });
    } else {
      result.push({ value: trimmed, displayName: trimmed, description: "" });
    }
    seen.add(trimmed);
  }

  return result.length > 0 ? result : [defaultModel];
}

function mergeAvailableModelLists(...sources: unknown[]): string[] | undefined {
  let hasList = false;
  const result: string[] = [];

  for (const source of sources) {
    if (!Array.isArray(source)) continue;
    hasList = true;
    for (const entry of source) {
      if (typeof entry !== "string") continue;
      const trimmed = entry.trim();
      if (trimmed && !result.includes(trimmed)) {
        result.push(trimmed);
      }
    }
  }

  return hasList ? result : undefined;
}

function parseModelProviderMap(raw: string | undefined): KodexModelProviderEntry[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];

  const entries: KodexModelProviderEntry[] = [];
  for (const entry of parsed) {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const model = stringField(record.model);
    const displayName = stringField(record.display_name) ?? stringField(record.displayName);
    const provider = stringField(record.provider);
    if (!model || !displayName || !provider) continue;
    entries.push({ model, displayName, provider });
  }
  return entries;
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function applyModelProviderMap(
  models: ModelInfo[],
  providerEntries: KodexModelProviderEntry[],
): ModelInfo[] {
  if (providerEntries.length === 0) return models;

  const allowed = new Set<string>();
  for (const model of models) {
    allowed.add(model.value);
    allowed.add(model.displayName);
  }

  const routedModels: KodexModelInfo[] = [];
  const seen = new Set<string>();
  for (const entry of providerEntries) {
    if (!allowed.has(entry.model) && !allowed.has(entry.displayName)) {
      continue;
    }
    const key = `${entry.provider}:${entry.model}`;
    if (seen.has(key)) continue;
    seen.add(key);

    const sdkMatch =
      resolveModelPreference(models, entry.displayName) ?? resolveModelPreference(models, entry.model);
    routedModels.push({
      ...(sdkMatch ?? {
        value: entry.model,
        displayName: entry.displayName,
        description: "",
      }),
      value: entry.model,
      displayName: entry.displayName,
      description: sdkMatch?.description ?? "",
      kodexProvider: entry.provider,
      kodexRouteModel: entry.model,
    });
  }

  return routedModels.length > 0 ? routedModels : models;
}

function sessionModelOption(model: ModelInfo) {
  const option: Record<string, unknown> = {
    modelId: model.value,
    name: model.displayName,
    description: model.description,
  };
  const meta = modelProviderMeta(model);
  if (meta) option._meta = meta;
  return option as SessionModelState["availableModels"][number];
}

function modelConfigOption(model: SessionModelState["availableModels"][number]): any {
  const option: Record<string, unknown> = {
    value: model.modelId,
    name: model.name,
    description: model.description ?? undefined,
  };
  const meta = modelMeta(model);
  if (meta) option._meta = meta;
  return option;
}

function modelProviderMeta(model: ModelInfo): Record<string, string> | undefined {
  const provider = (model as KodexModelInfo).kodexProvider;
  if (!provider) return undefined;
  const routeModel = (model as KodexModelInfo).kodexRouteModel ?? model.value;
  return {
    provider,
    route_model: routeModel,
  };
}

function modelMeta(model: unknown): Record<string, unknown> | undefined {
  if (typeof model !== "object" || model === null) return undefined;
  const meta = (model as { _meta?: unknown; meta?: unknown })._meta ?? (model as { meta?: unknown }).meta;
  if (typeof meta !== "object" || meta === null || Array.isArray(meta)) return undefined;
  return meta as Record<string, unknown>;
}

function optionProvider(option: unknown): string | undefined {
  const provider = modelMeta(option)?.provider;
  return typeof provider === "string" && provider.trim() ? provider.trim() : undefined;
}

function optionRouteModel(option: unknown): string | undefined {
  const routeModel = modelMeta(option)?.route_model;
  return typeof routeModel === "string" && routeModel.trim() ? routeModel.trim() : undefined;
}

function decodeProviderValue(value: string): { value: string; provider?: string } {
  if (!value.startsWith(KODEX_PROVIDER_VALUE_PREFIX)) {
    return { value };
  }
  const rest = value.slice(KODEX_PROVIDER_VALUE_PREFIX.length);
  const separator = rest.indexOf(":");
  if (separator <= 0) {
    return { value };
  }
  const provider = rest.slice(0, separator).trim();
  const decodedValue = rest.slice(separator + 1);
  if (!provider || !decodedValue) {
    return { value };
  }
  return { value: decodedValue, provider };
}

function resolveModelPreferenceForProvider(
  models: ModelInfo[],
  preference: string,
  provider?: string,
): ModelInfo | null {
  if (!provider) return resolveModelPreference(models, preference);
  const providerModels = models.filter((model) => (model as KodexModelInfo).kodexProvider === provider);
  return resolveModelPreference(providerModels.length > 0 ? providerModels : models, preference);
}

async function getAvailableModels(
  query: Query,
  models: ModelInfo[],
  settingsManager: SettingsManager,
  logger: Logger,
): Promise<SessionModelState> {
  const settings = settingsManager.getSettings();

  let currentModel = models[0];

  // Model priority (highest to lowest):
  // 1. ANTHROPIC_MODEL environment variable
  // 2. settings.model (user configuration)
  // 3. models[0] (default first model)
  if (process.env.ANTHROPIC_MODEL) {
    const match = resolveModelPreference(models, process.env.ANTHROPIC_MODEL);
    if (match) {
      currentModel = match;
    }
  } else {
    const match = resolveSettingsModel(models, settings.model, logger);
    if (match) {
      currentModel = match;
    }
  }

  await query.setModel(currentModel.value);

  return {
    availableModels: models.map((model) => sessionModelOption(model)),
    currentModelId: currentModel.value,
  };
}

function getAvailableSlashCommands(commands: SlashCommand[]): AvailableCommand[] {
  const UNSUPPORTED_COMMANDS = [
    "cost",
    "keybindings-help",
    "login",
    "logout",
    "output-style:new",
    "release-notes",
    "todos",
  ];

  return commands
    .map((command) => {
      const input = command.argumentHint
        ? {
            hint: Array.isArray(command.argumentHint)
              ? command.argumentHint.join(" ")
              : command.argumentHint,
          }
        : null;
      let name = command.name;
      if (command.name.endsWith(" (MCP)")) {
        name = `mcp:${name.replace(" (MCP)", "")}`;
      }
      return {
        name,
        description: command.description || "",
        input,
      };
    })
    .filter((command: AvailableCommand) => !UNSUPPORTED_COMMANDS.includes(command.name));
}

function formatUriAsLink(uri: string): string {
  try {
    if (uri.startsWith("file://")) {
      const path = uri.slice(7); // Remove "file://"
      const name = path.split("/").pop() || path;
      return `[@${name}](${uri})`;
    } else if (uri.startsWith("zed://")) {
      const parts = uri.split("/");
      const name = parts[parts.length - 1] || uri;
      return `[@${name}](${uri})`;
    }
    return uri;
  } catch {
    return uri;
  }
}

export function promptToClaude(prompt: PromptRequest): SDKUserMessage {
  const content: any[] = [];
  const context: any[] = [];

  for (const chunk of prompt.prompt) {
    switch (chunk.type) {
      case "text": {
        let text = chunk.text;
        // change /mcp:server:command args -> /server:command (MCP) args
        const mcpMatch = text.match(/^\/mcp:([^:\s]+):(\S+)(?:\s(.*))?$/);
        if (mcpMatch) {
          const [, server, command, args] = mcpMatch;
          text = `/${server}:${command} (MCP)${args ? ` ${args}` : ""}`;
        }
        content.push({ type: "text", text });
        break;
      }
      case "resource_link": {
        const formattedUri = formatUriAsLink(chunk.uri);
        content.push({
          type: "text",
          text: formattedUri,
        });
        break;
      }
      case "resource": {
        if ("text" in chunk.resource) {
          const formattedUri = formatUriAsLink(chunk.resource.uri);
          content.push({
            type: "text",
            text: formattedUri,
          });
          context.push({
            type: "text",
            text: `\n<context ref="${chunk.resource.uri}">\n${chunk.resource.text}\n</context>`,
          });
        }
        // Ignore blob resources (unsupported)
        break;
      }
      case "image":
        if (chunk.data) {
          content.push({
            type: "image",
            source: {
              type: "base64",
              data: chunk.data,
              media_type: chunk.mimeType,
            },
          });
        } else if (chunk.uri && chunk.uri.startsWith("http")) {
          content.push({
            type: "image",
            source: {
              type: "url",
              url: chunk.uri,
            },
          });
        }
        break;
      // Ignore audio and other unsupported types
      default:
        break;
    }
  }

  content.push(...context);

  return {
    type: "user",
    message: {
      role: "user",
      content: content,
    },
    session_id: prompt.sessionId,
    parent_tool_use_id: null,
  };
}

/**
 * Convert an SDKAssistantMessage (Claude) to a SessionNotification (ACP).
 * Only handles text, image, and thinking chunks for now.
 */
export function toAcpNotifications(
  content: string | ContentBlockParam[] | BetaContentBlock[] | BetaRawContentBlockDelta[],
  role: "assistant" | "user",
  sessionId: string,
  toolUseCache: ToolUseCache,
  client: AgentSideConnection,
  logger: Logger,
  options?: {
    registerHooks?: boolean;
    clientCapabilities?: ClientCapabilities;
    parentToolUseId?: string | null;
    cwd?: string;
    taskState?: TaskState;
  },
): SessionNotification[] {
  const taskState = options?.taskState ?? new Map();
  const registerHooks = options?.registerHooks !== false;
  const supportsTerminalOutput = options?.clientCapabilities?._meta?.["terminal_output"] === true;
  if (typeof content === "string") {
    const update: SessionNotification["update"] = {
      sessionUpdate: role === "assistant" ? "agent_message_chunk" : "user_message_chunk",
      content: {
        type: "text",
        text: content,
      },
    };

    if (options?.parentToolUseId) {
      update._meta = {
        ...update._meta,
        claudeCode: {
          ...(update._meta?.claudeCode || {}),
          parentToolUseId: options.parentToolUseId,
        },
      };
    }

    return [{ sessionId, update }];
  }

  const output = [];
  // Only handle the first chunk for streaming; extend as needed for batching
  for (const chunk of content) {
    let update: SessionNotification["update"] | null = null;
    switch (chunk.type) {
      case "text":
      case "text_delta":
        update = {
          sessionUpdate: role === "assistant" ? "agent_message_chunk" : "user_message_chunk",
          content: {
            type: "text",
            text: chunk.text,
          },
        };
        break;
      case "image":
        update = {
          sessionUpdate: role === "assistant" ? "agent_message_chunk" : "user_message_chunk",
          content: {
            type: "image",
            data: chunk.source.type === "base64" ? chunk.source.data : "",
            mimeType: chunk.source.type === "base64" ? chunk.source.media_type : "",
            uri: chunk.source.type === "url" ? chunk.source.url : undefined,
          },
        };
        break;
      case "thinking":
      case "thinking_delta":
        update = {
          sessionUpdate: "agent_thought_chunk",
          content: {
            type: "text",
            text: chunk.thinking,
          },
        };
        break;
      case "tool_use":
      case "server_tool_use":
      case "mcp_tool_use": {
        const alreadyCached = chunk.id in toolUseCache;
        toolUseCache[chunk.id] = chunk;
        if (chunk.name === "TodoWrite") {
          // @ts-expect-error - sometimes input is empty object or undefined
          if (Array.isArray(chunk.input?.todos)) {
            update = {
              sessionUpdate: "plan",
              entries: planEntries(chunk.input as { todos: ClaudePlanEntry[] }),
            };
          }
        } else if (
          chunk.name === "TaskCreate" ||
          chunk.name === "TaskUpdate" ||
          chunk.name === "TaskList" ||
          chunk.name === "TaskGet"
        ) {
          // Task* tool_use is suppressed; the plan update is emitted at
          // tool_result time once we have the task ID (for TaskCreate) and
          // confirmation that the change took effect.
        } else {
          // Only register hooks on first encounter to avoid double-firing
          if (registerHooks && !alreadyCached) {
            registerHookCallback(chunk.id, {
              onPostToolUseHook: async (toolUseId, toolInput, toolResponse) => {
                const toolUse = toolUseCache[toolUseId];
                if (toolUse) {
                  // Both `Edit` and `Write` produce a structuredPatch in their
                  // PostToolUse tool_response. For Edit the diff replaces the
                  // optimistic content built at tool_use time. For Write the
                  // optimistic content (built from `input.content` alone with
                  // `oldText: null`) shows "creation" semantics regardless of
                  // whether the file existed; the structuredPatch from the
                  // hook lets us emit the real diff for `type: "update"`. The
                  // helper returns `{}` if the response shape isn't usable.
                  const editDiff =
                    toolUse.name === "Edit" || toolUse.name === "Write"
                      ? toolUpdateFromDiffToolResponse(toolResponse)
                      : {};
                  const update: SessionNotification["update"] = {
                    _meta: {
                      claudeCode: {
                        toolResponse,
                        toolName: toolUse.name,
                      },
                    } satisfies ToolUpdateMeta,
                    toolCallId: toolUseId,
                    sessionUpdate: "tool_call_update",
                    rawInput: toolInput,
                    ...editDiff,
                  };
                  await client.sessionUpdate({
                    sessionId,
                    update,
                  });
                } else {
                  logger.error(
                    `[claude-agent-acp] Got a tool response for tool use that wasn't tracked: ${toolUseId}`,
                  );
                }
              },
            });
          }

          let rawInput;
          try {
            rawInput = JSON.parse(JSON.stringify(chunk.input));
          } catch {
            // ignore if we can't turn it to JSON
          }

          if (alreadyCached) {
            // Second encounter (full assistant message after streaming) —
            // send as tool_call_update to refine the existing tool_call
            // rather than emitting a duplicate tool_call.
            update = {
              _meta: {
                claudeCode: {
                  toolName: chunk.name,
                },
              } satisfies ToolUpdateMeta,
              toolCallId: chunk.id,
              sessionUpdate: "tool_call_update",
              rawInput,
              ...toolInfoFromToolUse(chunk, supportsTerminalOutput, options?.cwd),
            };
          } else {
            // First encounter (streaming content_block_start or replay) —
            // send as tool_call with terminal_info for Bash tools.
            update = {
              _meta: {
                claudeCode: {
                  toolName: chunk.name,
                },
                ...(chunk.name === "Bash" && supportsTerminalOutput
                  ? { terminal_info: { terminal_id: chunk.id } }
                  : {}),
              } satisfies ToolUpdateMeta,
              toolCallId: chunk.id,
              sessionUpdate: "tool_call",
              rawInput,
              status: "pending",
              ...toolInfoFromToolUse(chunk, supportsTerminalOutput, options?.cwd),
            };
          }
        }
        break;
      }

      case "tool_result":
      case "tool_search_tool_result":
      case "web_fetch_tool_result":
      case "web_search_tool_result":
      case "code_execution_tool_result":
      case "bash_code_execution_tool_result":
      case "text_editor_code_execution_tool_result":
      case "mcp_tool_result": {
        const toolUse = toolUseCache[chunk.tool_use_id];
        if (!toolUse) {
          logger.error(
            `[claude-agent-acp] Got a tool result for tool use that wasn't tracked: ${chunk.tool_use_id}`,
          );
          break;
        }

        if (
          toolUse.name === "TaskCreate" ||
          toolUse.name === "TaskUpdate" ||
          toolUse.name === "TaskList" ||
          toolUse.name === "TaskGet"
        ) {
          // Headless/SDK sessions emit Task* tools instead of TodoWrite.
          // TaskCreate / TaskUpdate mutate the accumulated task list; TaskList
          // and TaskGet are read-only so we just suppress their tool_call /
          // tool_result events. The plan update is emitted as a snapshot of
          // the accumulated state, mirroring the legacy TodoWrite behavior.
          const isError = "is_error" in chunk && chunk.is_error;
          if (!isError) {
            if (toolUse.name === "TaskCreate") {
              applyTaskCreate(
                taskState,
                toolUse.input as Parameters<typeof applyTaskCreate>[1],
                parseTaskCreateOutput(chunk.content),
              );
            } else if (toolUse.name === "TaskUpdate") {
              applyTaskUpdate(taskState, toolUse.input as Parameters<typeof applyTaskUpdate>[1]);
            }
          }
          if (!isError && (toolUse.name === "TaskCreate" || toolUse.name === "TaskUpdate")) {
            update = {
              sessionUpdate: "plan",
              entries: taskStateToPlanEntries(taskState),
            };
          }
        } else if (toolUse.name !== "TodoWrite") {
          const { _meta: toolMeta, ...toolUpdate } = toolUpdateFromToolResult(
            chunk,
            toolUseCache[chunk.tool_use_id],
            supportsTerminalOutput,
          );

          // When terminal output is supported, send terminal_output as a
          // separate notification to match codex-acp's streaming lifecycle:
          //   1. tool_call       → _meta.terminal_info  (already sent above)
          //   2. tool_call_update → _meta.terminal_output (sent here)
          //   3. tool_call_update → _meta.terminal_exit  (sent below with status)
          if (toolMeta?.terminal_output) {
            output.push({
              sessionId,
              update: {
                _meta: {
                  terminal_output: toolMeta.terminal_output,
                  ...(options?.parentToolUseId
                    ? { claudeCode: { parentToolUseId: options.parentToolUseId } }
                    : {}),
                },
                toolCallId: chunk.tool_use_id,
                sessionUpdate: "tool_call_update" as const,
              },
            });
          }

          update = {
            _meta: {
              claudeCode: {
                toolName: toolUse.name,
              },
              ...(toolMeta?.terminal_exit ? { terminal_exit: toolMeta.terminal_exit } : {}),
            } satisfies ToolUpdateMeta,
            toolCallId: chunk.tool_use_id,
            sessionUpdate: "tool_call_update",
            status: "is_error" in chunk && chunk.is_error ? "failed" : "completed",
            rawOutput: chunk.content,
            ...toolUpdate,
          };
        }
        break;
      }

      case "document":
      case "search_result":
      case "redacted_thinking":
      case "input_json_delta":
      case "citations_delta":
      case "signature_delta":
      case "container_upload":
      case "compaction":
      case "compaction_delta":
      case "advisor_tool_result":
        break;

      default:
        unreachable(chunk, logger);
        break;
    }
    if (update) {
      if (options?.parentToolUseId) {
        update._meta = {
          ...update._meta,
          claudeCode: {
            ...(update._meta?.claudeCode || {}),
            parentToolUseId: options.parentToolUseId,
          },
        };
      }
      output.push({ sessionId, update });
    }
  }

  return output;
}

export function streamEventToAcpNotifications(
  message: SDKPartialAssistantMessage,
  sessionId: string,
  toolUseCache: ToolUseCache,
  client: AgentSideConnection,
  logger: Logger,
  options?: {
    clientCapabilities?: ClientCapabilities;
    cwd?: string;
    taskState?: TaskState;
  },
): SessionNotification[] {
  const event = message.event;
  switch (event.type) {
    case "content_block_start":
      return toAcpNotifications(
        [event.content_block],
        "assistant",
        sessionId,
        toolUseCache,
        client,
        logger,
        {
          clientCapabilities: options?.clientCapabilities,
          parentToolUseId: message.parent_tool_use_id,
          cwd: options?.cwd,
          taskState: options?.taskState,
        },
      );
    case "content_block_delta":
      return toAcpNotifications(
        [event.delta],
        "assistant",
        sessionId,
        toolUseCache,
        client,
        logger,
        {
          clientCapabilities: options?.clientCapabilities,
          parentToolUseId: message.parent_tool_use_id,
          cwd: options?.cwd,
          taskState: options?.taskState,
        },
      );
    // No content
    case "message_start":
    case "message_delta":
    case "message_stop":
    case "content_block_stop":
      return [];

    default:
      unreachable(event, logger);
      return [];
  }
}

export function runAcp(options?: RunAcpOptions) {
  const input = options?.input ?? nodeToWebWritable(process.stdout);
  const output = options?.output ?? nodeToWebReadable(process.stdin);

  const stream = ndJsonStream(input, output);
  let agent!: ClaudeAcpAgent;
  const connection = new AgentSideConnection((client) => {
    agent = new ClaudeAcpAgent(client, { woa: options?.woa });
    return agent;
  }, stream);
  return { connection, agent };
}

function commonPrefixLength(a: string, b: string) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) {
    i++;
  }
  return i;
}

/** Best-effort first guess of a model's context window from its ID, used only
 *  until a `result` message arrives with the authoritative `modelUsage` value.
 *  Anthropic 1M-context variants encode "1m" as a distinct token in the SDK
 *  model ID (e.g., "claude-opus-4-6-1m"), which `\b1m\b` catches without also
 *  matching things like "10m" or embedded substrings. */
function inferContextWindowFromModel(model: string): number | null {
  if (/\b1m\b/i.test(model)) return 1_000_000;
  return null;
}

function parseModelConfig(raw: string | undefined):
  | {
      modelOverrides?: Record<string, string>;
      availableModels?: string[];
      preserveDefaultModel?: boolean;
    }
  | undefined {
  if (!raw) return undefined;
  const parsed = JSON.parse(raw);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("CLAUDE_MODEL_CONFIG must be a JSON object");
  }
  const result: {
    modelOverrides?: Record<string, string>;
    availableModels?: string[];
    preserveDefaultModel?: boolean;
  } = {};
  if (parsed.modelOverrides !== undefined) result.modelOverrides = parsed.modelOverrides;
  if (parsed.availableModels !== undefined) result.availableModels = parsed.availableModels;
  if (parsed.preserveDefaultModel !== undefined)
    result.preserveDefaultModel = Boolean(parsed.preserveDefaultModel);
  return Object.keys(result).length > 0 ? result : undefined;
}

function getMatchingModelUsage(modelUsage: Record<string, ModelUsage>, currentModel: string) {
  let bestKey: string | null = null;
  let bestLen = 0;

  for (const key of Object.keys(modelUsage)) {
    const len = commonPrefixLength(key, currentModel);
    if (len > bestLen) {
      bestLen = len;
      bestKey = key;
    }
  }

  if (bestKey) {
    return modelUsage[bestKey];
  }
}
