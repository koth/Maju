import { useEffect, useMemo } from "react";
import { Animated, Easing, View } from "react-native";
import { colors, spacing } from "../theme";

// Three breathing dots for the "agent is thinking" state. Each dot fades and
// lifts on a staggered loop so the waiting state feels alive instead of a
// bare spinner.
export function ThinkingIndicator({ text = "思考中\u2026" }: { text?: string }) {
  const dots = useMemo(
    () => [new Animated.Value(0), new Animated.Value(0), new Animated.Value(0)],
    [],
  );

  useEffect(() => {
    const animations = dots.map((dot, index) =>
      Animated.loop(
        Animated.sequence([
          Animated.delay(index * 180),
          Animated.timing(dot, { toValue: 1, duration: 320, easing: Easing.out(Easing.quad), useNativeDriver: true }),
          Animated.timing(dot, { toValue: 0, duration: 320, easing: Easing.in(Easing.quad), useNativeDriver: true }),
          Animated.delay((2 - index) * 180),
        ]),
      ),
    );
    animations.forEach((animation) => animation.start());
    return () => animations.forEach((animation) => animation.stop());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <View style={thinkingStyles.pill}>
      <View style={thinkingStyles.dotRow}>
        {dots.map((dot, index) => (
          <Animated.View
            key={index}
            style={[
              thinkingStyles.dot,
              {
                opacity: dot.interpolate({ inputRange: [0, 1], outputRange: [0.35, 1] }),
                transform: [{ translateY: dot.interpolate({ inputRange: [0, 1], outputRange: [0, -3] }) }],
              },
            ]}
          />
        ))}
      </View>
      <Animated.Text style={[thinkingStyles.text, { opacity: dots[1].interpolate({ inputRange: [0, 1], outputRange: [0.6, 1] }) }]}>
        {text}
      </Animated.Text>
    </View>
  );
}

const thinkingStyles = {
  // No pill and no border: three quiet dots next to faint text. A boxed
  // container around a transient state added a competing surface to every
  // streaming turn.
  pill: {
    flexDirection: "row" as const,
    alignItems: "center" as const,
    gap: spacing.sm,
    paddingVertical: spacing.xs,
    marginTop: spacing.xs,
    alignSelf: "flex-start" as const,
  },
  dotRow: { flexDirection: "row" as const, gap: 4 },
  dot: { width: 5, height: 5, borderRadius: 2.5, backgroundColor: colors.textDim },
  text: { color: colors.textFaint, fontSize: 13 },
};
// end of file