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
    .sort(compareTimelineTurnChangeSets)[0] ?? null;
}

export function reviewableTurnChangeSetSignature(
  sessionId: string,
  changeSet: TimelineTurnChangeSet,
) {
  return [
    sessionId,
    changeSet.changeSetId,
    changeSet.updatedAt,
    changeSet.timelineIndex ?? -1,
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

function compareTimelineTurnChangeSets(
  a: TimelineTurnChangeSet,
  b: TimelineTurnChangeSet,
) {
  const updatedDelta = timestampValue(b.updatedAt) - timestampValue(a.updatedAt);
  if (updatedDelta !== 0) return updatedDelta;

  const timelineDelta = (b.timelineIndex ?? -1) - (a.timelineIndex ?? -1);
  if (timelineDelta !== 0) return timelineDelta;

  return b.changeSetId.localeCompare(a.changeSetId);
}

function timestampValue(value: string | null | undefined) {
  if (!value) return 0;
  const parsed = Date.parse(value);
  if (Number.isFinite(parsed)) return parsed;
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : 0;
}
