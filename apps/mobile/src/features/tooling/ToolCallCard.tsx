import { memo, useEffect, useRef, useState } from "react";
import { View, Text, Pressable, StyleSheet, Animated, Easing, Vibration } from "react-native";
import type { ToolInvocation } from "../../types";
import { styles, colors, spacing, radius } from "../theme";
import { deriveToolPresentation, type ToolTone } from "./tool-presentation";
import { compactPreviewHunks } from "./compact-diff";

interface Props {
  tool: ToolInvocation;
  onStop?: (toolCallId: string) => void;
}

const TONE_COLOR: Record<ToolTone, string> = {
  running: colors.accent,
  ok: colors.textFaint,
  danger: colors.danger,
  warning: colors.warn,
};

// Status bullet with the desktop `tc-bullet-active` blink cadence while the
// tool is running; static otherwise.
function StatusBullet({ running, color }: { running: boolean; color: string }) {
  const opacity = useRef(new Animated.Value(1)).current;
  useEffect(() => {
    if (!running) {
      opacity.stopAnimation();
      opacity.setValue(0.9);
      return;
    }
    const animation = Animated.loop(
      Animated.sequence([
        Animated.timing(opacity, { toValue: 0.25, duration: 550, easing: Easing.inOut(Easing.quad), useNativeDriver: true }),
        Animated.timing(opacity, { toValue: 1, duration: 550, easing: Easing.inOut(Easing.quad), useNativeDriver: true }),
      ]),
    );
    animation.start();
    return () => animation.stop();
  }, [running, opacity]);
  return (
    <Animated.Text style={[cardStyles.bullet, { color, opacity }]}>{"\u25CF"}</Animated.Text>
  );
}

// Collapsed tool row matching the desktop ToolCallCard: a flat
// `● verb  title (+N -N) ›` line instead of a boxed card. Expanding reveals
// the `└`-prefixed output block, diff list, and raw request/result — the
// same sections the desktop shows, without tabs.
function ToolCallCardImpl({ tool, onStop }: Props) {
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
        style={({ pressed }) => [cardStyles.header, pressed && presentation.hasDetail ? cardStyles.headerPressed : null]}
      >
        <StatusBullet running={running} color={TONE_COLOR[presentation.tone]} />
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
    paddingHorizontal: spacing.xs + 2,
    borderRadius: radius.sm,
    minWidth: 0,
  },
  headerPressed: {
    backgroundColor: colors.surface,
  },
  bullet: {
    fontSize: 7,
    marginRight: spacing.sm,
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
    paddingLeft: 18,
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