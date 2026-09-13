import { memo, useState } from "react";
import { View, Text, Pressable, ScrollView, StyleSheet } from "react-native";
import type { TurnFileChanges, UiSnapshot } from "../../types";
import { useAppController } from "../../app/AppServicesContext";
import { FileDiffSheet, type FileDiffTurn } from "./FileDiffSheet";
import { colors, spacing } from "../theme";

// Pinned above the composer: the files changed by the most recent turn that
// modified any file — the mobile counterpart of the desktop per-turn
// ChangesBar. Collapsed shows one summary line (label + file count + line
// totals); expanding lists every file with its +/- counts. Tapping a file
// opens a full-screen modal with the file's real diff (fetched on demand via
// the GetFileDiff control op), aligned with the desktop review panel.

function latestTurnWithChanges(turns: TurnFileChanges[]): TurnFileChanges | null {
  for (let i = turns.length - 1; i >= 0; i--) {
    if (turns[i].changes.length > 0) return turns[i];
  }
  return null;
}

function lastTimelineMessageId(snapshot: UiSnapshot): string | null {
  for (let i = snapshot.timeline.length - 1; i >= 0; i--) {
    const item = snapshot.timeline[i];
    if (typeof item === "object" && "Message" in item) return item.Message;
  }
  return null;
}

export const TurnChangesBar = memo(function TurnChangesBar({ snapshot }: { snapshot: UiSnapshot }) {
  const controller = useAppController();
  const [expanded, setExpanded] = useState(false);
  const [target, setTarget] = useState<FileDiffTurn | null>(null);
  const turn = latestTurnWithChanges(snapshot.turn_changes ?? []);
  if (!turn) return null;

  const files = turn.changes;
  const added = files.reduce((sum, file) => sum + file.added_lines, 0);
  const removed = files.reduce((sum, file) => sum + file.removed_lines, 0);
  // The turn is "current" while its closing assistant reply is still the last
  // timeline entry; once a newer turn starts (even without file changes yet)
  // the bar relabels so it never claims stale work is 本轮.
  const isCurrentTurn = turn.message_id === lastTimelineMessageId(snapshot);

  return (
    <View style={barStyles.wrap}>
      <Pressable
        style={({ pressed }) => [barStyles.header, pressed ? barStyles.headerPressed : null]}
        onPress={() => setExpanded((value) => !value)}
        accessibilityRole="button"
        accessibilityState={{ expanded }}
        accessibilityLabel={expanded ? "收起本轮改动" : "展开本轮改动"}
      >
        <Text style={barStyles.label}>{isCurrentTurn ? "本轮改动" : "上轮改动"}</Text>
        <Text style={barStyles.meta}>{`${files.length} 个文件`}</Text>
        <Text style={[barStyles.count, { color: colors.success }]}>{`+${added}`}</Text>
        <Text style={[barStyles.count, { color: colors.danger }]}>{`\u2212${removed}`}</Text>
        <Text style={[barStyles.chevron, expanded ? barStyles.chevronOpen : null]}>{"\u203A"}</Text>
      </Pressable>
      {expanded ? (
        <ScrollView style={barStyles.list} nestedScrollEnabled>
          {files.map((file, index) => (
            <Pressable
              key={`${file.path}:${index}`}
              style={({ pressed }) => [barStyles.fileRow, pressed ? barStyles.fileRowPressed : null]}
              onPress={() =>
                setTarget({
                  messageId: turn.message_id,
                  initialPath: file.path,
                  sections: files.map((entry) => ({
                    path: entry.path,
                    changeType: entry.change_type,
                    addedLines: entry.added_lines,
                    removedLines: entry.removed_lines,
                  })),
                })
              }
              accessibilityRole="button"
              accessibilityLabel={`查看 ${file.path} 的 diff`}
            >
              <Text
                style={[barStyles.path, file.change_type === "Deleted" ? barStyles.pathDeleted : null]}
                numberOfLines={1}
              >
                {file.path}
              </Text>
              <Text style={[barStyles.count, { color: colors.success }]}>{`+${file.added_lines}`}</Text>
              <Text style={[barStyles.count, { color: colors.danger }]}>{`\u2212${file.removed_lines}`}</Text>
            </Pressable>
          ))}
        </ScrollView>
      ) : null}
      <FileDiffSheet turn={target} controller={controller} onClose={() => setTarget(null)} />
    </View>
  );
});

const barStyles = StyleSheet.create({
  // A thin strip above the composer: hairline separation + plain text. It used
  // to be a filled panel, which stacked another surface between the timeline
  // and the input.
  wrap: {
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: colors.border,
    backgroundColor: colors.bg,
    maxHeight: 240,
  },
  header: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.sm,
    paddingHorizontal: spacing.lg,
  },
  headerPressed: {
    backgroundColor: colors.surface,
  },
  label: {
    color: colors.textDim,
    fontSize: 12,
    fontWeight: "600",
  },
  meta: {
    color: colors.textFaint,
    fontSize: 12,
    marginLeft: spacing.sm,
    flexShrink: 1,
  },
  count: {
    fontSize: 12,
    fontWeight: "600",
    fontVariant: ["tabular-nums"],
    marginLeft: spacing.sm,
  },
  chevron: {
    color: colors.textFaint,
    fontSize: 13,
    marginLeft: "auto",
    paddingLeft: spacing.sm,
    transform: [{ rotate: "90deg" }],
  },
  chevronOpen: {
    transform: [{ rotate: "-90deg" }],
  },
  list: {
    maxHeight: 200,
    paddingHorizontal: spacing.lg,
    paddingBottom: spacing.sm,
  },
  fileRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.xs + 1,
    borderRadius: 6,
    gap: spacing.sm,
  },
  fileRowPressed: {
    backgroundColor: colors.surface,
  },
  path: {
    color: colors.textDim,
    fontSize: 12,
    flex: 1,
    fontFamily: "monospace",
  },
  pathDeleted: {
    color: colors.textFaint,
    textDecorationLine: "line-through",
  },
});
// end of file
