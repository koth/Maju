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
  Options,
  PermissionMode,
  Query,
  query,
  renameSession,
  Settings,
  SDKAssistantMessageError,
  SDKMessageOrigin,
  SDKSessionInfo,
  SDKUserMessage,
  SlashCommand,
} from "@anthropic-ai/claude-agent-sdk";
import { randomUUID } from "node:crypto";
import * as os from "node:os";
import * as path from "node:path";
import packageJson from "../package.json" with { type: "json" };
import {
  promptToClaude,
  streamEventToAcpNotifications,
  toAcpNotifications,
  type ToolUseCache,
} from "./acp-notifications.js";
import { SettingsManager } from "./settings.js";
import {
  KODEX_MODEL_PROVIDER_MAP_ENV,
  applyAvailableModelsAllowlist,
  applyModelProviderMap,
  buildConfigOptions,
  decodeProviderValue,
  getAvailableModels,
  getMatchingModelUsage,
  inferContextWindowFromModel,
  mergeAvailableModelLists,
  optionProvider,
  optionRouteModel,
  parseModelConfig,
  parseModelProviderMap,
  resolveModelPreferenceForProvider,
  type KodexModelInfo,
} from "./model-options.js";
import type { Logger } from "./logger.js";
import {
  ALLOW_BYPASS,
  askUserQuestionPermissionOptions,
  askUserQuestionSelection,
  describeAlwaysAllow,
  normalizeAskUserQuestions,
  permissionGuidance,
  permissionInputAnswers,
  resolvePermissionMode,
  withAskUserQuestionAnswer,
  withAskUserQuestionAnswers,
} from "./permission-utils.js";
import {
  SESSION_INFO_SYNC_RETRY_DELAYS_MS,
  TITLE_SUMMARY_INPUT_LENGTH,
  cleanGeneratedTitle,
  contextCompactionNotification,
  sleep,
  titleFromSessionInfo,
  titleInputFromSessionInfo,
  titlePromptText,
  truncateTitle,
} from "./session-title.js";
import { claudeCliPath, enrichClaudeCliLaunchError, verifiedClaudeCliPath } from "./claude-cli.js";
import {
  createPostToolUseHook,
  createTaskHook,
  TaskState,
  taskStateToPlanEntries,
  toolInfoFromToolUse,
} from "./tools.js";
import { Pushable, unreachable } from "./utils.js";
import { buildWoaEnv, ensureWoaToken, WoaConfig } from "./woa/index.js";

export {
  promptToClaude,
  streamEventToAcpNotifications,
  toAcpNotifications,
} from "./acp-notifications.js";
export type { ToolUpdateMeta, ToolUseCache } from "./acp-notifications.js";
export type { Logger } from "./logger.js";
export { claudeCliPath, verifiedClaudeCliPath } from "./claude-cli.js";
export { describeAlwaysAllow, resolvePermissionMode } from "./permission-utils.js";

export const CLAUDE_CONFIG_DIR =
  process.env.CLAUDE_CONFIG_DIR ?? path.join(os.homedir(), ".claude");

export type ClaudeAcpAgentOptions = {
  logger?: Logger;
  woa?: WoaConfig;
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

function shouldHideClaudeAuth(): boolean {
  return process.argv.includes("--hide-claude-auth");
}

// Bypass Permissions doesn't work if we are a root/sudo user
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
      throw new Error(
        `Invalid value for config option ${params.configId}: ${requestedValue.value}`,
      );
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
        const stopPlanningOption: PermissionOption = {
          kind: "reject_once",
          name: "No, stop planning",
          optionId: "rejectAndExitPlan",
        };
        const options = [
          ...optionsAll.filter((o) =>
            session.modes.availableModes.some((m) => m.id === o.optionId),
          ),
          stopPlanningOption,
        ];

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
        if (selectedModeWasOffered && selectedMode === "rejectAndExitPlan") {
          throw new Error("Tool use aborted");
        }
        if (selectedModeWasOffered && selectedMode === "plan") {
          const guidance = permissionGuidance(response);
          return {
            behavior: "deny",
            message: guidance
              ? `User asked to keep planning and provided guidance:\n${guidance}`
              : "User asked to keep planning.",
          };
        }
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
          updatedInput: withAskUserQuestionAnswer(
            toolInput,
            selection,
            permissionGuidance(response),
          ),
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
      const claudeExecutable = await verifiedClaudeCliPath();
      titleQuery = query({
        prompt,
        options: {
          cwd: session.cwd,
          env: mergeEnv(process.env, woaEnv),
          maxTurns: 1,
          model,
          permissionMode: "dontAsk",
          persistSession: false,
          pathToClaudeCodeExecutable: claudeExecutable,
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
    const claudeExecutable = await verifiedClaudeCliPath();

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
      pathToClaudeCodeExecutable: claudeExecutable,
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
      throw await enrichClaudeCliLaunchError(error, claudeExecutable);
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
