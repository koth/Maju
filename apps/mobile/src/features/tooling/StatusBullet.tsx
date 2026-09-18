import { useEffect, useRef } from "react";
import { Animated, Easing } from "react-native";
import { colors } from "../theme";
import { BULLET_SLOT } from "./row-geometry";
import type { ToolTone } from "./tool-presentation";

/// Bullet tone → palette. Shared by the tool row and the activity summary row
/// so a collapsed run carries the tone of what it stands in for.
///
/// `ok` is never drawn: a succeeded row marks nothing (its verb already says
/// 已运行 / 已编辑), but the tone stays in the map because `Record<ToolTone, …>`
/// is deliberately total — adding a tone must not silently fall through to a
/// wrong colour.
export const TONE_COLOR: Record<ToolTone, string> = {
  running: colors.accent,
  ok: colors.textFaint,
  danger: colors.danger,
  warning: colors.warn,
};

// Status bullet with the desktop `tc-bullet-active` blink cadence while the
// tool is running; static otherwise. Rendered only for rows that need a marker
// (still running, or finished abnormally) — see the call sites.
//
// `hang` is the only layout knob: rows that own a gutter pass the default, rows
// already sitting inside an indented container (an expanded activity group)
// pass 0 so the bullet stays inline rather than crossing their indent rule.
export function StatusBullet({
  running,
  color,
  hang = BULLET_SLOT,
}: {
  running: boolean;
  color: string;
  hang?: number;
}) {
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
    <Animated.Text
      style={[bulletStyles.bullet, { color, opacity, marginLeft: -hang }]}
    >
      {"\u25CF"}
    </Animated.Text>
  );
}

const bulletStyles = {
  bullet: {
    fontSize: 7,
    // A fixed slot rather than the glyph's own advance, so the hang always
    // matches the reserved width exactly. The glyph is drawn at the slot's left
    // edge, which leaves it its own ~8px gap before the text.
    width: BULLET_SLOT,
    marginRight: 0,
  },
} as const;
// end of file
