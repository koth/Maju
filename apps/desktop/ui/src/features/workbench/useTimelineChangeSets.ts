import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import type { ChangeSetSummary, FileChangeSummary, UiSnapshot } from "../../types";
import { sessionListChangeSetFiles, sessionListChangeSets } from "../../lib/tauri";
import type { TimelineTurnChangeSet } from "../conversation/ConversationTimeline";

function timestampValue(value: string | null | undefined) {
  if (!value) return 0;
  const parsed = Date.parse(value);
  if (Number.isFinite(parsed)) return parsed;
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}

function buildTimelineTurnChangeSets(
  summaries: ChangeSetSummary[],
  filesByChangeSetId: Record<string, FileChangeSummary[]>,
): Record<string, TimelineTurnChangeSet> {
  const byMessageId: Record<string, TimelineTurnChangeSet> = {};
  for (const summary of summaries) {
    if (summary.source !== "AgentTurn" || !summary.message_id || summary.file_count === 0) {
      continue;
    }
    const files = filesByChangeSetId[summary.id] ?? [];
    if (files.length === 0) continue;
    const existing = byMessageId[summary.message_id];
    if (
      existing &&
      timestampValue(existing.updatedAt) >= timestampValue(summary.updated_at)
    ) {
      continue;
    }
    byMessageId[summary.message_id] = {
      changeSetId: summary.id,
      files,
      updatedAt: summary.updated_at,
    };
  }
  return byMessageId;
}

function buildTimelineTurnChangeSet(
  summary: ChangeSetSummary | null | undefined,
  filesByChangeSetId: Record<string, FileChangeSummary[]>,
): TimelineTurnChangeSet | null {
  if (!summary || summary.source !== "AgentTurn" || summary.file_count === 0) {
    return null;
  }
  const files = filesByChangeSetId[summary.id] ?? [];
  if (files.length === 0) return null;
  return {
    changeSetId: summary.id,
    files,
    updatedAt: summary.updated_at,
  };
}

function timelineTurnChangeSetSignature(
  messageId: string,
  changeSet: TimelineTurnChangeSet,
) {
  return [
    messageId,
    changeSet.changeSetId,
    changeSet.updatedAt,
    ...changeSet.files.map((file) =>
      [
        file.path,
        file.change_type,
        file.added_lines,
        file.removed_lines,
        file.quality,
        file.updated_at,
      ].join(":"),
    ),
  ].join(":");
}

function timelineTurnChangeSetsSignature(
  changeSetsByMessageId: Record<string, TimelineTurnChangeSet>,
  liveTurnChangeSet: TimelineTurnChangeSet | null,
) {
  const entries = Object.entries(changeSetsByMessageId).map(([messageId, changeSet]) =>
    timelineTurnChangeSetSignature(messageId, changeSet),
  );
  if (liveTurnChangeSet) {
    entries.push(timelineTurnChangeSetSignature("__live__", liveTurnChangeSet));
  }
  return entries.sort().join("|");
}

function latestPendingAgentTurnSummary(
  summaries: ChangeSetSummary[],
  activeTurnOwnerKey: string | null,
) {
  if (!activeTurnOwnerKey) return undefined;
  return summaries
    .filter(
      (summary) =>
        summary.source === "AgentTurn" &&
        summary.message_id == null &&
        summary.owner_key === activeTurnOwnerKey &&
        summary.file_count > 0,
    )
    .sort((a, b) => {
      if (a.status !== b.status) {
        if (a.status === "Pending") return -1;
        if (b.status === "Pending") return 1;
      }
      return timestampValue(b.updated_at) - timestampValue(a.updated_at);
    })[0];
}

function activeTurnOwnerKey(snapshot: UiSnapshot | null) {
  if (
    snapshot?.session.status !== "Streaming" &&
    snapshot?.session.status !== "WaitingForTool"
  ) {
    return null;
  }

  const messagesById = new Map(snapshot.messages.map((message) => [message.id, message]));
  for (let index = snapshot.timeline.length - 1; index >= 0; index -= 1) {
    const item = snapshot.timeline[index];
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (message?.role === "User") {
      return `user-message:${message.id}`;
    }
  }
  return null;
}

interface UseTimelineChangeSetsArgs {
  snapshot: UiSnapshot | null;
  snapshotRef: MutableRefObject<UiSnapshot | null>;
  workspaceReady: boolean;
  onGitRefresh: () => void | Promise<void>;
}

export function useTimelineChangeSets({
  snapshot,
  snapshotRef,
  workspaceReady,
  onGitRefresh,
}: UseTimelineChangeSetsArgs) {
  const [timelineTurnChangeSets, setTimelineTurnChangeSets] = useState<
    Record<string, TimelineTurnChangeSet>
  >({});
  const [liveTurnChangeSet, setLiveTurnChangeSet] =
    useState<TimelineTurnChangeSet | null>(null);
  const [agentConversationChangeCount, setAgentConversationChangeCount] = useState(0);
  const changeSetRefreshRef = useRef<{
    workspaceRoot: string;
    signature: string;
  } | null>(null);

  const clearChangeSets = useCallback(() => {
    setTimelineTurnChangeSets({});
    setLiveTurnChangeSet(null);
    setAgentConversationChangeCount(0);
    changeSetRefreshRef.current = null;
  }, []);

  const currentAgentTurnChangesSignature = useMemo(
    () => timelineTurnChangeSetsSignature(timelineTurnChangeSets, liveTurnChangeSet),
    [liveTurnChangeSet, timelineTurnChangeSets],
  );
  const activeAgentTurn =
    snapshot?.session.status === "Streaming" ||
    snapshot?.session.status === "WaitingForTool";
  const activeAgentTurnOwnerKey = useMemo(() => activeTurnOwnerKey(snapshot), [snapshot]);

  useEffect(() => {
    const sessionId = snapshot?.session.id;
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceReady || !sessionId || !workspaceRoot) {
      setTimelineTurnChangeSets({});
      setLiveTurnChangeSet(null);
      setAgentConversationChangeCount(0);
      return;
    }

    let cancelled = false;
    Promise.all([
      sessionListChangeSets({
        source: "AgentTurn",
        session_id: sessionId,
        workspace_root: workspaceRoot,
      }),
      sessionListChangeSets({
        source: "AgentConversation",
        session_id: sessionId,
        workspace_root: workspaceRoot,
      }),
    ])
      .then(async ([summaries, conversationSummaries]) => {
        const turnSummaries = summaries.filter(
          (summary) =>
            summary.source === "AgentTurn" &&
            summary.message_id != null &&
            summary.file_count > 0,
        );
        const pendingTurnSummary = activeAgentTurn
          ? latestPendingAgentTurnSummary(summaries, activeAgentTurnOwnerKey)
          : undefined;
        const summariesToLoad = pendingTurnSummary
          ? [...turnSummaries, pendingTurnSummary]
          : turnSummaries;
        const fileEntries = await Promise.all(
          summariesToLoad.map(async (summary) => {
            try {
              const response = await sessionListChangeSetFiles({
                change_set_id: summary.id,
              });
              return [summary.id, response.files] as const;
            } catch {
              return [summary.id, []] as const;
            }
          }),
        );
        if (cancelled) return;
        const filesByChangeSetId = Object.fromEntries(fileEntries);
        setTimelineTurnChangeSets(
          buildTimelineTurnChangeSets(turnSummaries, filesByChangeSetId),
        );
        setLiveTurnChangeSet(
          buildTimelineTurnChangeSet(pendingTurnSummary, filesByChangeSetId),
        );
        const conversationSummary = conversationSummaries.find(
          (summary) => summary.source === "AgentConversation" && summary.file_count > 0,
        );
        setAgentConversationChangeCount(conversationSummary?.file_count ?? 0);
      })
      .catch(() => {
        if (!cancelled) {
          setTimelineTurnChangeSets({});
          setLiveTurnChangeSet(null);
          setAgentConversationChangeCount(0);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [
    activeAgentTurn,
    activeAgentTurnOwnerKey,
    snapshot?.revision,
    snapshot?.session.id,
    snapshot?.workspace.root,
    workspaceReady,
  ]);

  useEffect(() => {
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceReady || !workspaceRoot) return;

    const previous = changeSetRefreshRef.current;
    if (!previous || previous.workspaceRoot !== workspaceRoot) {
      changeSetRefreshRef.current = {
        workspaceRoot,
        signature: currentAgentTurnChangesSignature,
      };
      return;
    }

    if (previous.signature === currentAgentTurnChangesSignature) return;
    changeSetRefreshRef.current = {
      workspaceRoot,
      signature: currentAgentTurnChangesSignature,
    };

    const timer = window.setTimeout(() => {
      if (snapshotRef.current?.workspace.root === workspaceRoot) {
        void onGitRefresh();
      }
    }, 120);

    return () => window.clearTimeout(timer);
  }, [
    currentAgentTurnChangesSignature,
    onGitRefresh,
    snapshot?.workspace.root,
    snapshotRef,
    workspaceReady,
  ]);

  return {
    timelineTurnChangeSets,
    liveTurnChangeSet,
    agentConversationChangeCount,
    clearChangeSets,
  };
}
