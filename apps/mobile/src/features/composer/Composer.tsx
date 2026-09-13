import { useState } from "react";
import { View, Text, TextInput, Pressable, ActivityIndicator, Vibration, StyleSheet } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { colors, spacing, radius } from "../theme";

interface Props {
  onSend: (text: string) => void | Promise<void>;
  disabled?: boolean;
  error?: string | null;
  /** True while the session is mid-turn (Streaming / WaitingForTool): shows
   * a stop button next to send. Sending stays available so the turn can be
   * steered without stopping it first. */
  streaming?: boolean;
  onCancel?: () => void | Promise<void>;
}

// Prompt input + send (+ stop while a turn is running). Image/file attach is
// behind a feature flag for the MVP (the prompt content type supports them,
// but the picker UI is deferred).
//
// No keyboard math lives here on purpose: the conversation screen owns a single
// measured lift for the whole column (see useKeyboardAvoidance), so a second,
// platform-specific pad here used to stack on top of Android's `adjustResize`
// and float the input a keyboard away from the keyboard.
export function Composer({ onSend, disabled, error, streaming, onCancel }: Props) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [canceling, setCanceling] = useState(false);
  const [inputHeight, setInputHeight] = useState(38);
  const insets = useSafeAreaInsets();

  const canSend = text.trim().length > 0 && !disabled && !sending;

  const handleSend = async () => {
    if (!canSend) return;
    const value = text.trim();
    setSending(true);
    try {
      await onSend(value);
      Vibration.vibrate(8);
      setText("");
    } catch {
      // The parent surfaces the error and keeps the input so the user can retry.
    } finally {
      setSending(false);
    }
  };

  const handleCancel = async () => {
    if (!onCancel || !streaming || canceling) return;
    setCanceling(true);
    try {
      await onCancel();
    } finally {
      setCanceling(false);
    }
  };

  return (
    <View
      style={{
        backgroundColor: colors.bg,
        paddingBottom: insets.bottom > 0 ? 0 : 8,
      }}
    >
      {error ? (
        <Text style={composerStyles.error} numberOfLines={2}>
          {error}
        </Text>
      ) : null}
      <View style={composerStyles.row}>
        <TextInput
          style={[
            composerStyles.input,
            {
              height: Math.max(44, Math.min(inputHeight + 8, 140)),
              borderRadius: inputHeight <= 46 ? 22 : radius.lg,
            },
          ]}
          placeholder={"给智能体发消息\u2026"}
          placeholderTextColor={colors.textFaint}
          value={text}
          onChangeText={setText}
          onContentSizeChange={(event) => setInputHeight(event.nativeEvent.contentSize.height)}
          multiline
          editable={!disabled}
        />
        {streaming && onCancel ? (
          <Pressable
            style={({ pressed }) => [composerStyles.circle, composerStyles.circleStop, { opacity: pressed ? 0.8 : 1 }]}
            disabled={canceling}
            onPress={handleCancel}
            accessibilityRole="button"
            accessibilityLabel="停止智能体"
          >
            {canceling ? (
              <ActivityIndicator color={colors.text} size="small" />
            ) : (
              <View style={composerStyles.stopSquare} />
            )}
          </Pressable>
        ) : null}
        <Pressable
          style={({ pressed }) => [
            composerStyles.circle,
            canSend ? composerStyles.circleSend : composerStyles.circleDisabled,
            { opacity: pressed && canSend ? 0.85 : 1 },
          ]}
          disabled={!canSend}
          onPress={handleSend}
          accessibilityRole="button"
          accessibilityLabel="发送消息"
        >
          {sending ? (
            <ActivityIndicator color={colors.bg} size="small" />
          ) : (
            <Text style={[composerStyles.arrow, { color: canSend ? colors.bg : colors.textFaint }]}>{"\u2191"}</Text>
          )}
        </Pressable>
      </View>
    </View>
  );
}

// Chat-style composer: one filled rounded field plus two circular buttons.
// No top divider and no boxed input — the field's fill already separates it
// from the timeline, and a border on top of that read as heavy chrome.
const composerStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "flex-end",
    paddingHorizontal: spacing.md,
    paddingTop: spacing.sm,
    gap: spacing.sm,
  },
  input: {
    flex: 1,
    color: colors.text,
    backgroundColor: colors.surfaceAlt,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm + 2,
    paddingBottom: spacing.sm + 2,
    fontSize: 15,
  },
  circle: {
    width: 44,
    height: 44,
    borderRadius: 22,
    alignItems: "center",
    justifyContent: "center",
  },
  circleSend: { backgroundColor: colors.text },
  circleStop: { backgroundColor: colors.surfaceRaised },
  circleDisabled: { backgroundColor: colors.surfaceAlt },
  arrow: { fontSize: 18, fontWeight: "700", lineHeight: 20 },
  stopSquare: { width: 12, height: 12, borderRadius: 3, backgroundColor: colors.text },
  error: {
    color: colors.danger,
    fontSize: 12,
    lineHeight: 17,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm,
  },
});
// end of file