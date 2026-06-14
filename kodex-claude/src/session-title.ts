import type { PromptRequest, SessionNotification } from "@agentclientprotocol/sdk";
import type { SDKSessionInfo } from "@anthropic-ai/claude-agent-sdk";

const MAX_TITLE_LENGTH = 256;
export const TITLE_SUMMARY_INPUT_LENGTH = 8_000;
export const SESSION_INFO_SYNC_RETRY_DELAYS_MS = [0, 250, 1_000, 2_500, 5_000, 10_000] as const;
const KODEX_CONTEXT_COMPACTION_META_KEY = "kodex.ai/contextCompaction";
export function contextCompactionNotification(
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

export function truncateTitle(text: string, maxLength: number, suffix = "…"): string {
  const chars = Array.from(text);
  if (chars.length <= maxLength) {
    return text;
  }
  return `${chars.slice(0, Math.max(0, maxLength - Array.from(suffix).length)).join("")}${suffix}`;
}

export function titlePromptText(prompt: PromptRequest): string | null {
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

export function cleanGeneratedTitle(text: string | null | undefined): string | null {
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

export function sleep(ms: number): Promise<void> {
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

export function titleFromSessionInfo(info: SDKSessionInfo | undefined): string | null {
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

export function titleInputFromSessionInfo(info: SDKSessionInfo | undefined): string | null {
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
