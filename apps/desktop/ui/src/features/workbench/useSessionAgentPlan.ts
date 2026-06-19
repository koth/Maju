import { useEffect, useMemo, useState } from "react";
import type { AgentPlanEntry, UiSnapshot } from "../../types";

interface RetainedAgentPlan {
  signature: string;
  entries: AgentPlanEntry[];
}

function agentPlanSignature(entries: AgentPlanEntry[]) {
  return entries
    .map((entry) =>
      [
        entry.id ?? "",
        entry.content,
        entry.priority,
        entry.status,
      ].join("\u001f"),
    )
    .join("\u001e");
}

export function useSessionAgentPlan(
  snapshot: Pick<UiSnapshot, "agent_plan" | "session"> | null,
) {
  const [retainedPlans, setRetainedPlans] = useState<Record<string, RetainedAgentPlan>>({});
  const liveEntries = snapshot?.agent_plan ?? [];
  const liveSignature = useMemo(
    () => agentPlanSignature(liveEntries),
    [liveEntries],
  );

  useEffect(() => {
    const sessionId = snapshot?.session.id ?? null;
    if (!sessionId || liveEntries.length === 0) return;

    setRetainedPlans((current) =>
      current[sessionId]?.signature === liveSignature
        ? current
        : {
            ...current,
            [sessionId]: { signature: liveSignature, entries: liveEntries },
          },
    );
  }, [liveEntries, liveSignature, snapshot?.session.id]);

  if (!snapshot) return [];
  if (liveEntries.length > 0) return liveEntries;
  return retainedPlans[snapshot.session.id]?.entries ?? [];
}
