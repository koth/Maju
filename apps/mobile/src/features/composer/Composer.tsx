import { useState } from "react";
import { View, Text, TextInput, Pressable, ActivityIndicator, Vibration } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { styles, colors, spacing, radius } from "../theme";

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
        borderTopWidth: 1,
        borderTopColor: colors.border,
        paddingBottom: insets.bottom > 0 ? 0 : 8,
      }}
    >
      {error ? (
        <View style={{ paddingHorizontal: spacing.md, paddingTop: spacing.xs + 2 }}>
          <View style={[styles.chip, { backgroundColor: colors.dangerTint, borderColor: colors.danger, alignSelf: "flex-start" }]}>
            <Text style={{ color: colors.danger, fontSize: 12 }}>{error}</Text>
          </View>
        </View>
      ) : null}
      <View style={{ flexDirection: "row", alignItems: "flex-end", padding: spacing.sm, gap: spacing.sm }}>
        <TextInput
          style={[
            styles.input,
            {
              flex: 1,
              height: Math.max(46, Math.min(inputHeight + 8, 140)),
              borderRadius: inputHeight <= 50 ? 23 : radius.lg,
              paddingHorizontal: spacing.lg,
              fontSize: 15,
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
            style={({ pressed }) => [
              styles.pillButton,
              {
                width: 46,
                height: 46,
                alignSelf: "flex-end",
                backgroundColor: colors.dangerTint,
                borderWidth: 1,
                borderColor: colors.danger,
                shadowOpacity: 0,
                elevation: 0,
                opacity: pressed ? 0.8 : 1,
              },
            ]}
            disabled={canceling}
            onPress={handleCancel}
            accessibilityRole="button"
            accessibilityLabel="停止智能体"
          >
            {canceling ? (
              <ActivityIndicator color={colors.danger} size="small" />
            ) : (
              <View style={{ width: 14, height: 14, borderRadius: 3, backgroundColor: colors.danger }} />
            )}
          </Pressable>
        ) : null}
        <Pressable
          style={({ pressed }) => [
            styles.pillButton,
            {
              width: 46,
              height: 46,
              alignSelf: "flex-end",
              opacity: canSend ? (pressed ? 0.85 : 1) : 0.4,
            },
          ]}
          disabled={!canSend}
          onPress={handleSend}
          accessibilityRole="button"
          accessibilityLabel="发送消息"
        >
          {sending ? (
            <ActivityIndicator color="#fff" size="small" />
          ) : (
            <Text style={{ color: "#fff", fontSize: 18, fontWeight: "800", lineHeight: 20 }}>{"\u2191"}</Text>
          )}
        </Pressable>
      </View>
    </View>
  );
}
// end of file