import type { ToolInvocation } from "../../types";

/** Workspace-relative paths mentioned by the current turn — extracted from
 *  the turn's file changes and from the inputs/outputs of shell tool calls.
 *  `MarkdownBody` uses this pool to resolve file references without probing
 *  the whole repository. */
export interface FilePathCandidatePool {
  byMessageId: ReadonlyMap<string, readonly string[]>;
  all: readonly string[];
}

/** Extract path-like tokens from shell command inputs and command output.
 *  Both relative (`crates/app-core/src/state.rs`) and absolute paths are
 *  collected; `MarkdownBody` normalises them against the workspace root. */
export function collectPathsFromText(text: string): string[] {
  const found = new Set<string>();
  const pattern = /(?:[A-Za-z]:[\\/]|\/|\.{1,2}[\\/])?[\w.$~@-]+(?:[\\/][\w.$~@-]+)+(?::\d+(?::\d+)?)?/g;
  for (const match of text.matchAll(pattern)) {
    let token = match[0];
    token = token.replace(/^[ab]\//, "");
    const lastSegment = token.replace(/\\/g, "/").split("/").pop() ?? "";
    const name = lastSegment.replace(/:\d+(?::\d+)?$/, "");
    if (!/\.[A-Za-z0-9]{1,10}$/.test(name)) continue;
    found.add(token);
  }
  return [...found];
}

function collectToolPaths(tool: ToolInvocation): string[] {
  const texts = [tool.raw_input, tool.raw_output, tool.terminal_output?.output];
  const found = new Set<string>();
  for (const text of texts) {
    if (!text) continue;
    for (const path of collectPathsFromText(text)) {
      found.add(path);
    }
  }
  return [...found];
}

/** Build the per-turn candidate pool. A turn spans from a user message
 *  (exclusive) to the next user message; steers do not break a turn. Every
 *  assistant message in the turn shares the accumulated pool. */
export function buildFilePathCandidatePool(
  timeline: readonly unknown[],
  messagesById: ReadonlyMap<string, { id: string; role: string; is_steer?: boolean | null }>,
  toolsById: ReadonlyMap<string, ToolInvocation>,
  turnChangeSetsByMessageId: Record<string, { files: { path: string }[] }>,
): FilePathCandidatePool {
  const byMessageId = new Map<string, readonly string[]>();
  const pool = new Set<string>();
  let turnAssistantIds: string[] = [];

  const flushTurn = () => {
    if (turnAssistantIds.length === 0) return;
    const snapshot = [...pool];
    for (const id of turnAssistantIds) {
      byMessageId.set(id, snapshot);
    }
    turnAssistantIds = [];
  };

  for (const item of timeline) {
    if (typeof item !== "object" || item === null) continue;

    if ("Message" in item) {
      const message = messagesById.get((item as { Message: string }).Message);
      if (!message) continue;
      if (message.role === "User" && !message.is_steer) {
        flushTurn();
        pool.clear();
        continue;
      }
      if (message.role === "Assistant") {
        turnAssistantIds.push(message.id);
      }
      continue;
    }

    if ("Tool" in item) {
      const tool = toolsById.get((item as { Tool: string }).Tool);
      if (!tool) continue;
      for (const path of collectToolPaths(tool)) {
        pool.add(path);
      }
      for (const path of tool.diff_paths ?? []) {
        pool.add(path);
      }
    }
  }
  flushTurn();

  const all = new Set(pool);
  for (const paths of byMessageId.values()) {
    for (const path of paths) {
      all.add(path);
    }
  }
  for (const changeSet of Object.values(turnChangeSetsByMessageId)) {
    for (const file of changeSet.files) {
      all.add(file.path);
    }
  }

  return { byMessageId, all: [...all] };
}
