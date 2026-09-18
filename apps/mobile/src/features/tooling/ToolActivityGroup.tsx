import { memo, useRef, useState } from "react";
import { Animated, Easing, Pressable, StyleSheet, Text, View } from "react-native";
import { colors, radius, spacing } from "../theme";
import { ToolCallCard } from "./ToolCallCard";
import { BULLET_HANG, ROW_PADDING } from "./row-geometry";
import { StatusBullet, TONE_COLOR } from "./StatusBullet";
import type { ToolActivityGroup } from "./tool-activity";

// Collapsed activity run in the phone timeline — the mobile counterpart of the
// desktop `ToolActivityGroup`: one summary row per contiguous run of tool
// calls, expanding to the individual rows on tap.
//
// The summary row is deliberately the same height, indent and type scale as a
// `ToolCallCard` header so a collapsed run sits in the timeline as one row of
// the same rhythm it replaces.

interface Props {
  group: ToolActivityGroup;
  onStopTool?: (toolCallId: string) => void;
}

function ToolActivityGroupImpl({ group, onStopTool }: Props) {
  const [expanded, setExpanded] = useState(false);
  const spin = useRef(new Animated.Value(0)).current;
  const running = group.tools.some(
    (tool) => tool.status === "Running" || tool.status === "Pending",
  );

  const toggle = () => {
    const next = !expanded;
    setExpanded(next);
    Animated.timing(spin, {
      toValue: next ? 1 : 0,
      duration: 150,
      easing: Easing.out(Easing.quad),
      useNativeDriver: true,
    }).start();
  };

  return (
    <View style={groupStyles.wrap}>
      <Pressable
        onPress={toggle}
        accessibilityRole="button"
        accessibilityState={{ expanded }}
        accessibilityLabel={expanded ? `Collapse ${group.summary}` : `Expand ${group.summary}`}
        style={({ pressed }) => [groupStyles.summary, pressed ? groupStyles.summaryPressed : null]}
      >
        {/* Finished runs carry no marker (the summary text already says
            what happened); a run still in flight keeps the live bullet. */}
        {running ? <StatusBullet running color={TONE_COLOR.running} /> : null}
        <Text style={groupStyles.label} numberOfLines={1}>
          {group.summary}
        </Text>
        <Animated.View style={{ transform: [{ rotate: spin.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "90deg"] }) }] }}>
          <Text style={groupStyles.chevron}>{"\u203A"}</Text>
        </Animated.View>
      </Pressable>

      {expanded ? (
        <View style={groupStyles.content}>
          {group.tools.map((tool) => (
            <ToolCallCard key={tool.id} tool={tool} onStop={onStopTool} indented />
          ))}
        </View>
      ) : null}
    </View>
  );
}

const groupStyles = StyleSheet.create({
  wrap: { width: "100%" },
  // Mirrors `cardStyles.header` in ToolCallCard: same padding, same hanging
  // bullet geometry, so the collapsed summary and the rows it stands in for put
  // their text on the same column.
  summary: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.xs,
    paddingLeft: BULLET_HANG,
    paddingRight: ROW_PADDING,
    marginLeft: -BULLET_HANG,
    borderRadius: radius.sm,
    minWidth: 0,
  },
  summaryPressed: { backgroundColor: colors.surface },
  label: {
    color: colors.textDim,
    fontSize: 14,
    fontWeight: "500",
    flexShrink: 1,
    flexGrow: 1,
  },
  chevron: {
    color: colors.textFaint,
    fontSize: 14,
    marginLeft: spacing.xs,
  },
  // The expanded members hang off an indent rule, like the desktop
  // `.tool-activity-content` (11px offset + 13px padding + 1px border).
  content: {
    marginLeft: 11,
    paddingLeft: 13,
    borderLeftWidth: StyleSheet.hairlineWidth,
    borderLeftColor: colors.border,
  },
});

export const ToolActivityGroupRow = memo(ToolActivityGroupImpl);
// end of file
