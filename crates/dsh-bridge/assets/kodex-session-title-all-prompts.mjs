import { randomUUID } from "node:crypto";

/**
 * Kodex-owned, self-contained DSH session-title provider.
 *
 * The installed DSH package is intentionally not modified. Kodex writes this
 * module next to its patch overlay, disables DSH's first-prompt row, and mounts
 * this provider from the generated file:// entry instead. Completed-turn checks
 * are explicitly refreshed from `turn/end` so a new user message cannot cancel
 * the previous auxiliary title request.
 */
const name = "kodex-session-title-all-prompts";
const inject = ["sessionTitle", "llm", "sessions"];

const DEFAULT_CONFIG = {
  targetWords: 5,
  targetCjkCharacters: 10,
  maxInputBytes: 65536,
  maxOutputTokens: 8192,
  timeoutMs: 60000,
};

function positiveInteger(value, fallback, field) {
  const resolved = value ?? fallback;
  if (!Number.isInteger(resolved) || resolved <= 0) {
    throw new Error(`kodex-session-title: ${field} must be a positive integer`);
  }
  return resolved;
}

function normalizeConfig(raw) {
  const config = raw ?? {};
  const hasProvider = config.provider !== undefined;
  const hasModel = config.model !== undefined;
  if (hasProvider !== hasModel) {
    throw new Error("kodex-session-title: provider and model must be supplied together");
  }
  if (hasProvider && (typeof config.provider !== "string" || config.provider.length === 0 || typeof config.model !== "string" || config.model.length === 0)) {
    throw new Error("kodex-session-title: provider and model must be non-empty strings");
  }
  return {
    targetWords: positiveInteger(config.targetWords, DEFAULT_CONFIG.targetWords, "targetWords"),
    targetCjkCharacters: positiveInteger(
      config.targetCjkCharacters,
      DEFAULT_CONFIG.targetCjkCharacters,
      "targetCjkCharacters",
    ),
    maxInputBytes: positiveInteger(config.maxInputBytes, DEFAULT_CONFIG.maxInputBytes, "maxInputBytes"),
    maxOutputTokens: positiveInteger(
      config.maxOutputTokens,
      DEFAULT_CONFIG.maxOutputTokens,
      "maxOutputTokens",
    ),
    timeoutMs: positiveInteger(config.timeoutMs, DEFAULT_CONFIG.timeoutMs, "timeoutMs"),
    ...(hasProvider ? { provider: config.provider, model: config.model } : {}),
  };
}

function normalizeTitle(value) {
  return String(value ?? "")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F]/gu, "")
    .replace(/[\u200B\u200E\u200F\u202A-\u202E\u2060-\u2064\u2066-\u206F\uFEFF]/gu, "")
    .replace(/\s+/gu, " ")
    .trim();
}

function frameMessages(messages, currentTitle) {
  return `Generate or evaluate a session title from this JSON payload:\n${JSON.stringify({
    currentTitle: currentTitle ?? null,
    messages,
  })}`;
}

function frameSize(messages, currentTitle) {
  return Buffer.byteLength(frameMessages(messages, currentTitle), "utf8");
}

/**
 * Keep the first and newest user messages, then fill the remaining budget with
 * the newest middle messages. The first/newest pair preserves the original goal
 * and the current direction without allowing a long session to grow without
 * bound.
 */
function selectMessages(messages, maxInputBytes, currentTitle) {
  const normalized = messages.map((message, index) => ({
    seq: message.seq,
    text: String(message.text ?? "").slice(0, 4000),
    index,
  }));
  if (normalized.length === 0) return [];

  const selected = [];
  const selectedIndexes = new Set();
  const add = (message) => {
    if (!message || selectedIndexes.has(message.index)) return;
    const candidate = [...selected, message].sort((a, b) => a.index - b.index);
    if (frameSize(candidate, currentTitle) <= maxInputBytes) {
      selected.push(message);
      selectedIndexes.add(message.index);
    }
  };

  add(normalized[0]);
  add(normalized[normalized.length - 1]);
  for (let index = normalized.length - 2; index >= 1; index -= 1) {
    add(normalized[index]);
  }
  return selected;
}

function routeFor(config, request) {
  if (config.provider !== undefined && config.model !== undefined) {
    return { provider: config.provider, model: config.model };
  }
  if (request.route === undefined) {
    throw new Error("kodex-session-title: no route is available for the title request");
  }
  return request.route;
}

function terminalError(finish) {
  switch (finish?.kind) {
    case undefined:
    case "stop":
      return undefined;
    case "error":
    case "aborted": {
      const error = new Error(finish.failure?.message ?? "title request was aborted");
      error.code = finish.failure?.code;
      return error;
    }
    case "max-tokens":
      return new Error("kodex-session-title: title output reached maxOutputTokens");
    case "tool-calls":
      return new Error("kodex-session-title: title model unexpectedly requested a tool");
    default:
      return new Error(`kodex-session-title: unsupported finish reason ${String(finish.kind)}`);
  }
}

async function generateTitle(ctx, config, request) {
  request.signal.throwIfAborted();
  const current = ctx.sessionTitle.get(request.session);
  const currentTitle = current?.source?.kind === "provider" ? current.title : undefined;
  const selectedMessages = selectMessages(request.messages, config.maxInputBytes, currentTitle);
  if (selectedMessages.length === 0) {
    throw new Error("kodex-session-title: at least one source message is required");
  }

  const route = routeFor(config, request);
  const prompt = frameMessages(
    selectedMessages.map(({ seq, text }) => ({ seq, text })),
    currentTitle,
  );
  const message = {
    id: randomUUID(),
    role: "user",
    content: [{ type: "text", text: prompt }],
    source: { kind: "dsh-session-title-llm" },
  };
  const system = [
    "Create or evaluate a concise title for an AI coding-assistant session from the supplied human messages.",
    "Return only the title on one line, in plain natural language, with no quotes, prefix, explanation, Markdown, XML, or terminal control codes.",
    "Use the language of the messages.",
    `Aim for about ${config.targetWords} words in non-CJK languages or ${config.targetCjkCharacters} CJK characters.`,
    "If a current automatically generated title is supplied, keep it exactly when it still describes the session's primary task.",
    "Change it only when the newest messages materially change that primary task or make the title inaccurate. Do not rename for incremental details, clarifications, progress reports, test results, or follow-up work within the same task.",
  ].join("\n");

  request.session.append("session/title-llm-request", {
    titleProvider: name,
    messageSeqs: selectedMessages.map(({ seq }) => seq),
    route,
    system,
    messages: [message],
    maxTokens: config.maxOutputTokens,
  });

  const signal = AbortSignal.any([request.signal, AbortSignal.timeout(config.timeoutMs)]);
  const options = {
    provider: route.provider,
    model: route.model,
    messages: [message],
    system,
    maxTokens: config.maxOutputTokens,
    // Keep the auxiliary title stream out of the main conversation's
    // provider/session lane; it is a separate one-shot request.
    sessionId: `kodex-session-title:${request.session.id}`,
    purpose: "session-title",
    signal,
  };
  const textByIndex = new Map();
  const completedTextByIndex = new Map();
  let sawToolCall = false;
  let finish;
  for await (const chunk of ctx.llm.stream(options)) {
    signal.throwIfAborted();
    if (chunk.type === "text-delta") {
      textByIndex.set(chunk.index, `${textByIndex.get(chunk.index) ?? ""}${chunk.text}`);
    } else if (chunk.type === "block-end" && chunk.block?.type === "text") {
      // block-end is authoritative. Some adapters emit an initial empty
      // text-delta before the completed block; treating that delta as the
      // final value would discard the actual title.
      completedTextByIndex.set(chunk.index, chunk.block.text);
    } else if (chunk.type === "tool-call-delta" || chunk.type === "block-end" && chunk.block?.type === "tool-call") {
      sawToolCall = true;
    } else if (chunk.type === "finish") {
      finish = chunk.reason;
    }
  }
  signal.throwIfAborted();
  const failure = terminalError(finish);
  if (failure !== undefined) throw failure;
  if (sawToolCall) throw new Error("kodex-session-title: title output must contain text only");

  const textIndexes = [...new Set([...textByIndex.keys(), ...completedTextByIndex.keys()])]
    .sort((left, right) => left - right);
  const title = normalizeTitle(textIndexes.map((index) => {
    const completed = completedTextByIndex.get(index);
    return typeof completed === "string" && completed.length > 0
      ? completed
      : textByIndex.get(index) ?? "";
  }).join(" "));
  if (title.length === 0) throw new Error("kodex-session-title: title model produced no text");
  return {
    title,
    messageSeqs: selectedMessages.map(({ seq }) => seq),
    model: route,
  };
}

function apply(ctx, rawConfig) {
  const config = normalizeConfig(rawConfig);
  ctx.sessionTitle.register({
    id: name,
    // The first prompt is seeded by DSH's native cadence. Every completed
    // turn is also explicitly refreshed below; using all-prompts here would
    // let the next user message abort the previous title request.
    automatic: "first-prompt",
    generate(request) {
      return generateTitle(ctx, config, request);
    },
  });

  if (typeof ctx.on !== "function" || typeof ctx.sessionTitle.refresh !== "function") {
    throw new Error("kodex-session-title: DSH turn-end refresh API is unavailable");
  }

  const latestHumanSeqBySession = new WeakMap();
  ctx.on("session/event", (session, event) => {
    if (event.type === "user/message") {
      if (event.data?.source?.kind === "user") latestHumanSeqBySession.set(session, event.seq);
      return;
    }
    if (event.type !== "turn/end" || event.data.reason?.kind !== "completed") return;
    const latestHumanSeq = latestHumanSeqBySession.get(session);
    if (latestHumanSeq === undefined) return;
    latestHumanSeqBySession.delete(session);

    const current = ctx.sessionTitle.get(session);
    // A user rename pins the title. Never let an automatic refresh overwrite
    // that explicit choice.
    if (current?.source?.kind === "user") return;
    // If DSH's first-prompt request already covered the latest human message
    // (including its selected message seqs), do not issue a duplicate refresh.
    if (
      current?.source?.kind === "provider" &&
      Array.isArray(current.messageSeqs) &&
      current.messageSeqs.some((seq) => Number(seq) === latestHumanSeq)
    ) {
      return;
    }

    const reportRefreshError = (error) => {
      if (error?.name === "AbortError" || error?.code === "ABORTED") return;
      const message = `kodex-session-title: turn-end refresh failed for session ${session.id}: ${String(error)}`;
      if (typeof ctx.logger?.warn === "function") ctx.logger.warn(message);
    };
    try {
      void Promise.resolve(ctx.sessionTitle.refresh(session)).catch(reportRefreshError);
    } catch (error) {
      reportRefreshError(error);
    }
  });
}

export { apply, inject, name };
