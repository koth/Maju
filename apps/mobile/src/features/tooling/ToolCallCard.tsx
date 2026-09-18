import { memo, useRef, useState } from "react";
import { View, Text, Pressable, StyleSheet, Animated, Easing, Vibration } from "react-native";
import type { ToolInvocation } from "../../types";
import { styles, colors, spacing, radius } from "../theme";
import { deriveToolPresentation } from "./tool-presentation";
import { BULLET_HANG, ROW_PADDING } from "./row-geometry";
import { StatusBullet, TONE_COLOR } from "./StatusBullet";
import { compactPreviewHunks } from "./compact-diff";

interface Props {
  tool: ToolInvocation;
  onStop?: (toolCallId: string) => void;
  /// Set for rows that already live inside an indented container (the expanded
  /// members of an activity group): their bullet stays inline instead of
  /// hanging into a gutter they do not own.
  indented?: boolean;
}

// Collapsed tool row matching the desktop ToolCallCard: a flat
// `● verb  title (+N -N) ›` line instead of a boxed card. Expanding reveals
// the `└`-prefixed output block, diff list, and raw request/result — the
// same sections the desktop shows, without tabs.
function ToolCallCardImpl({ tool, onStop, indented = false }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [stopRequested, setStopRequested] = useState(false);
  const spin = useRef(new Animated.Value(0)).current;
  const running = tool.status === "Pending" || tool.status === "Running";
  const presentation = deriveToolPresentation(tool);
  const added = tool.diff_previews.reduce(
    (sum, preview) => sum + preview.hunks.reduce((acc, hunk) => acc + hunk.lines.filter((line) => line.kind === "Added").length, 0),
    0,
  );
  const removed = tool.diff_previews.reduce(
    (sum, preview) => sum + preview.hunks.reduce((acc, hunk) => acc + hunk.lines.filter((line) => line.kind === "Removed").length, 0),
    0,
  );
  // Editing cards with a real diff expand to the patch only — same rule as
  // the desktop (`showEditingDiffOnly`). The request/result JSON under a
  // rendered diff is pure noise (and duplicates the whole edit payload).
  const showDiffOnly = tool.diff_previews.length > 0 && (added > 0 || removed > 0);

  const toggle = () => {
    if (!presentation.hasDetail) return;
    const next = !expanded;
    setExpanded(next);
    Animated.timing(spin, {
      toValue: next ? 1 : 0,
      duration: 150,
      easing: Easing.out(Easing.quad),
      useNativeDriver: true,
    }).start();
  };

  const stop = () => {
    if (stopRequested || !onStop) return;
    setStopRequested(true);
    Vibration.vibrate(8);
    onStop(tool.call_id);
  };

  return (
    <View style={cardStyles.wrap}>
      <Pressable
        onPress={toggle}
        disabled={!presentation.hasDetail}
        accessibilityRole="button"
        accessibilityState={{ expanded }}
        style={({ pressed }) => [
          cardStyles.header,
          indented ? cardStyles.headerIndented : cardStyles.headerHanging,
          pressed && presentation.hasDetail ? cardStyles.headerPressed : null,
        ]}
      >
        {/* Desktop parity: only a row that still needs a marker draws one. A
            succeeded call's verb (已运行 / 已编辑) already carries the state, so
            a neutral dot in front of every finished row is pure noise; the
            gutter stays reserved either way (the bullet's width equals its hang
            in `row-geometry`), so the verb does not move. */}
        {presentation.tone !== "ok" ? (
          <StatusBullet running={running} color={TONE_COLOR[presentation.tone]} hang={indented ? 0 : undefined} />
        ) : null}
        <Text style={cardStyles.verb} numberOfLines={1}>
          {presentation.verb}
        </Text>
        <Text style={cardStyles.title} numberOfLines={1}>
          {presentation.title}
        </Text>
        {added > 0 || removed > 0 ? (
          <Text style={cardStyles.diffStats}>
            <Text style={{ color: colors.success }}>{`+${added}`}</Text>
            <Text style={{ color: colors.danger }}>{` -${removed}`}</Text>
          </Text>
        ) : null}
        {presentation.hasDetail ? (
          <Animated.View style={{ transform: [{ rotate: spin.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "90deg"] }) }] }}>
            <Text style={cardStyles.chevron}>{"\u203A"}</Text>
          </Animated.View>
        ) : null}
        {running && tool.can_stop && onStop ? (
          <Pressable
            hitSlop={8}
            style={({ pressed }) => [cardStyles.stopButton, { opacity: pressed ? 0.7 : 1 }]}
            onPress={(event) => {
              event.stopPropagation();
              stop();
            }}
            accessibilityRole="button"
            accessibilityLabel="停止工具"
          >
            <Text style={cardStyles.stopText}>{stopRequested ? "停止中\u2026" : "停止"}</Text>
          </Pressable>
        ) : null}
      </Pressable>

      {expanded ? (
        <View style={cardStyles.detail}>
          {!showDiffOnly && presentation.outputLines.length > 0 ? (
            <View style={cardStyles.outputBlock}>
              {presentation.outputLines.map((line, index) => (
                <Text key={index} style={cardStyles.outputLine} numberOfLines={0}>
                  <Text style={cardStyles.outputPrefix}>{index === 0 ? "\u2514 " : "  "}</Text>
                  {line}
                </Text>
              ))}
            </View>
          ) : null}

          {tool.diff_previews.map((preview) => {
            // Compact like the desktop: changed lines ± 3 context lines, long
            // unchanged runs collapse into explicit gap markers. Without this
            // a full-file hunk renders the whole file inside the card.
            const hunks = compactPreviewHunks(preview.hunks);
            return (
              <View key={preview.path} style={cardStyles.diffFile}>
                <Text style={[styles.mono, { color: colors.textDim, fontSize: 12, marginBottom: 2 }]} numberOfLines={1}>
                  {preview.path}
                </Text>
                {hunks.map((hunk, hi) => (
                  <View key={`${preview.path}:${hi}`}>
                    {hunk.heading ? (
                      <Text style={[styles.mono, { color: colors.textFaint, fontSize: 11, marginTop: 2 }]} numberOfLines={1}>
                        {hunk.heading}
                      </Text>
                    ) : null}
                    {hunk.rows.map((row, ri) =>
                      row.kind === "gap" ? (
                        <Text key={`gap:${ri}`} style={cardStyles.diffGap} numberOfLines={1}>
                          {`\u22EF ${row.count} \u884C\u672A\u66F4\u6539 \u22EF`}
                        </Text>
                      ) : (
                        <View
                          key={`line:${ri}`}
                          style={[
                            cardStyles.diffLine,
                            row.lineKind === "Added" ? cardStyles.diffAdded : row.lineKind === "Removed" ? cardStyles.diffRemoved : null,
                          ]}
                        >
                          <Text
                            style={[
                              styles.mono,
                              {
                                color: row.lineKind === "Added" ? colors.success : row.lineKind === "Removed" ? colors.danger : colors.textDim,
                                fontSize: 12,
                                lineHeight: 18,
                                flex: 1,
                              },
                            ]}
                          >
                            {row.lineKind === "Added" ? "+" : row.lineKind === "Removed" ? "\u2212" : " "}
                            {` ${row.content}`}
                          </Text>
                        </View>
                      ),
                    )}
                  </View>
                ))}
              </View>
            );
          })}

          {tool.error ? (
            <Text style={[styles.mono, { color: colors.danger, fontSize: 12, lineHeight: 18, marginTop: spacing.xs }]}>{tool.error}</Text>
          ) : null}

          {!showDiffOnly && tool.raw_input ? (
            <View style={{ marginTop: spacing.xs }}>
              <Text style={cardStyles.rawLabel}>Request</Text>
              <Text style={cardStyles.rawBody}>{tool.raw_input}</Text>
            </View>
          ) : null}
          {!showDiffOnly && tool.raw_output ? (
            <View style={{ marginTop: spacing.xs }}>
              <Text style={cardStyles.rawLabel}>Result</Text>
              <Text style={cardStyles.rawBody}>{tool.raw_output}</Text>
            </View>
          ) : null}
        </View>
      ) : null}
    </View>
  );
}

const cardStyles = StyleSheet.create({
  wrap: {
    paddingVertical: 3,
  },
  header: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.xs,
    paddingRight: ROW_PADDING,
    borderRadius: radius.sm,
    minWidth: 0,
  },
  // Hanging geometry (desktop parity): the box bleeds `BULLET_HANG` into the
  // left gutter and pads the same amount back, so the row's TEXT still starts on
  // the transcript's prose column while the bullet sits out in the gutter. Only
  // the left side bleeds — the right edge stays where it was.
  headerHanging: {
    paddingLeft: BULLET_HANG,
    marginLeft: -BULLET_HANG,
  },
  headerIndented: {
    paddingLeft: ROW_PADDING,
  },
  headerPressed: {
    backgroundColor: colors.surface,
  },
  verb: {
    color: colors.text,
    fontSize: 14,
    fontWeight: "500",
    marginRight: spacing.sm,
    flexShrink: 0,
  },
  title: {
    color: colors.textDim,
    fontSize: 14,
    flexShrink: 1,
    flexGrow: 1,
  },
  diffStats: {
    fontSize: 12,
    fontWeight: "500",
    marginLeft: spacing.sm,
    fontVariant: ["tabular-nums"],
  },
  chevron: {
    color: colors.textFaint,
    fontSize: 14,
    marginLeft: spacing.xs,
  },
  stopButton: {
    marginLeft: spacing.sm,
    paddingHorizontal: spacing.sm,
    paddingVertical: 2,
    borderRadius: radius.pill,
    backgroundColor: colors.surfaceAlt,
  },
  stopText: {
    color: colors.danger,
    fontSize: 11,
    fontWeight: "600",
  },
  detail: {
    // Indented under the row rather than flush with the prose column: the
    // header text now starts at the column edge, so the detail needs its own
    // small step to read as belonging to the row above it.
    paddingLeft: spacing.md,
    paddingTop: spacing.xs,
    paddingBottom: spacing.xs,
  },
  outputBlock: {
    marginBottom: spacing.xs,
  },
  outputLine: {
    color: colors.textDim,
    fontFamily: "monospace",
    fontSize: 12,
    lineHeight: 18,
  },
  outputPrefix: {
    color: colors.textFaint,
  },
  diffFile: {
    marginTop: spacing.xs,
    backgroundColor: colors.surface,
    borderRadius: radius.md,
    padding: spacing.sm,
  },
  diffLine: {
    borderRadius: 3,
    paddingHorizontal: spacing.xs,
    marginVertical: 1,
  },
  diffGap: {
    color: colors.textFaint,
    fontSize: 11,
    lineHeight: 18,
    textAlign: "center",
    marginVertical: 2,
  },
  diffAdded: {
    backgroundColor: "rgba(52,211,153,0.10)",
  },
  diffRemoved: {
    backgroundColor: "rgba(251,113,133,0.10)",
  },
  rawLabel: {
    color: colors.textFaint,
    fontSize: 10,
    fontWeight: "700",
    textTransform: "uppercase",
    letterSpacing: 0.5,
  },
  rawBody: {
    color: colors.textDim,
    fontFamily: "monospace",
    fontSize: 11,
    lineHeight: 16,
    marginTop: 2,
  },
});

export const ToolCallCard = memo(ToolCallCardImpl);
// end of file