import { useState } from "react";
import { View, Text, TextInput, Pressable, ActivityIndicator } from "react-native";
import { styles, colors, spacing } from "../theme";

interface Props {
  onSend: (text: string) => void | Promise<void>;
  disabled?: boolean;
}

// Prompt input + send. Image/file attach is behind a feature flag for the
// MVP (the prompt content type supports them, but the picker UI is deferred).
export function Composer({ onSend, disabled }: Props) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  const canSend = text.trim().length > 0 && !disabled && !sending;

  const handleSend = async () => {
    if (!canSend) return;
    const value = text.trim();
    setText("");
    setSending(true);
    try {
      await onSend(value);
    } finally {
      setSending(false);
    }
  };

  return (
    <View style={{ flexDirection: "row", alignItems: "flex-end", padding: spacing.sm, backgroundColor: colors.bg, borderTopWidth: 1, borderTopColor: colors.border }}>
      <TextInput
        style={[styles.input, { flex: 1, minHeight: 44, maxHeight: 140 }]}
        placeholder="Message the agent…"
        placeholderTextColor={colors.textDim}
        value={text}
        onChangeText={setText}
        multiline
        editable={!disabled}
      />
      <Pressable
        style={[styles.button, { marginLeft: spacing.sm, alignSelf: "stretch", justifyContent: "center" }, !canSend && { opacity: 0.5 }]}
        disabled={!canSend}
        onPress={handleSend}
      >
        {sending ? <ActivityIndicator color="#fff" /> : <Text style={styles.buttonText}>Send</Text>}
      </Pressable>
    </View>
  );
}
// end of file
