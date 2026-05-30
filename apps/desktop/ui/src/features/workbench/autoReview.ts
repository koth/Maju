import type { TimelineTurnChangeSet } from "../conversation/ConversationTimeline";

export function latestReviewableTurnChangeSet(
  turnChangeSetsByMessageId: Record<string, TimelineTurnChangeSet>,
  liveTurnChangeSet: TimelineTurnChangeSet | null,
) {
  if (liveTurnChangeSet && liveTurnChangeSet.files.length > 0) {
    return liveTurnChangeSet;
  }

  return Object.values(turnChangeSetsByMessageId)
    .filter((changeSet) => changeSet.files.length > 0)
    .sort((a, b) => timestampValue(b.updatedAt) - timestampValue(a.updatedAt))[0] ?? null;
}

export function reviewableTurnChangeSetSignature(
  sessionId: string,
  changeSet: TimelineTurnChangeSet,
) {
  return [
    sessionId,
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
  ].join("|");
}

function timestampValue(value: string | null | undefined) {
  if (!value) return 0;
  const parsed = Date.parse(value);
  if (Number.isFinite(parsed)) return parsed;
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}
