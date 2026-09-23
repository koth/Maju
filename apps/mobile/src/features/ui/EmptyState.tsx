import { View, Text, type StyleProp, type ViewStyle } from "react-native";
import { colors, spacing } from "../theme";

// Shared empty/error state: a soft glyph medallion + title + hint so blank
// screens read intentional instead of broken.
//
// `style` MUST be forwarded to the root view: VirtualizedList counter-flips
// an inverted list's empty component by cloning a `style` prop onto it
// (`_renderEmptyComponent`). A custom component that swallows the prop
// renders upside-down inside the flipped ScrollView — the "欢迎消息倒着
// 渲染" bug on a brand-new session, whose only row is this empty state.
export function EmptyState({
  glyph = "\u25CB",
  title,
  hint,
  style,
}: {
  glyph?: string;
  title: string;
  hint?: string;
  style?: StyleProp<ViewStyle>;
}) {
  return (
    <View style={[emptyStyles.wrap, style]}>
      <View style={emptyStyles.medallion}>
        <Text style={emptyStyles.glyph}>{glyph}</Text>
      </View>
      <Text style={emptyStyles.title}>{title}</Text>
      {hint ? <Text style={emptyStyles.hint}>{hint}</Text> : null}
    </View>
  );
}

const emptyStyles = {
  // No medallion box: a quiet glyph, a line of body text, and a hint. Empty
  // screens should feel calm, and a bordered badge in the middle of an empty
  // screen is a lot of chrome for zero information.
  wrap: { alignItems: "center" as const, paddingVertical: spacing.xxl, paddingHorizontal: spacing.xl },
  medallion: {
    alignItems: "center" as const,
    justifyContent: "center" as const,
    marginBottom: spacing.md,
  },
  glyph: { color: colors.textFaint, fontSize: 26 },
  title: { color: colors.textDim, fontSize: 14, fontWeight: "500" as const, textAlign: "center" as const },
  hint: { color: colors.textFaint, fontSize: 12, marginTop: spacing.xs, textAlign: "center" as const, lineHeight: 18 },
};
// end of file