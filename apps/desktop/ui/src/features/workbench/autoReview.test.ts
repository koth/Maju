import { describe, expect, it } from "vitest";
import type { FileChangeSummary } from "../../types";
import type { TimelineTurnChangeSet } from "../conversation/ConversationTimeline";
import {
  latestReviewableTurnChangeSet,
  reviewableTurnChangeSetSignature,
} from "./autoReview";

function file(path: string, updatedAt = "2026-05-30T01:00:00Z"): FileChangeSummary {
  return {
    change_set_id: "cs",
    path,
    change_type: "Modified",
    added_lines: 2,
    removed_lines: 1,
    quality: "Exact",
    updated_at: updatedAt,
  };
}

function turn(
  changeSetId: string,
  updatedAt: string,
  files: FileChangeSummary[] = [file("src/main.ts", updatedAt)],
): TimelineTurnChangeSet {
  return { changeSetId, updatedAt, files };
}

describe("latestReviewableTurnChangeSet", () => {
  it("prefers the live turn when it has diff files", () => {
    const older = turn("completed", "2026-05-30T01:00:00Z");
    const live = turn("live", "2026-05-30T00:30:00Z");

    expect(latestReviewableTurnChangeSet({ msg: older }, live)).toBe(live);
  });

  it("falls back to the newest completed turn with files", () => {
    const older = turn("older", "2026-05-30T01:00:00Z");
    const newer = turn("newer", "2026-05-30T02:00:00Z");

    expect(
      latestReviewableTurnChangeSet(
        {
          a: older,
          b: turn("empty", "2026-05-30T03:00:00Z", []),
          c: newer,
        },
        null,
      ),
    ).toBe(newer);
  });
});

describe("reviewableTurnChangeSetSignature", () => {
  it("changes when the diff changes", () => {
    const before = turn("cs-1", "2026-05-30T01:00:00Z", [file("src/main.ts")]);
    const after = turn("cs-1", "2026-05-30T01:00:00Z", [
      { ...file("src/main.ts"), added_lines: 3 },
    ]);

    expect(reviewableTurnChangeSetSignature("s-1", before)).not.toEqual(
      reviewableTurnChangeSetSignature("s-1", after),
    );
  });
});
