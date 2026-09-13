// On-device diagnostics viewer: shows the persisted log (pairing, relay
// I/O, identity persistence) so connection issues can be reported without
// adb/Xcode. Copy button puts the whole log on the clipboard for pasting.
import { useCallback, useEffect, useState } from "react";
import { View, Text, Pressable, ScrollView, Share } from "react-native";
import { diagnostics } from "../../util/diagnostics";
import { styles, colors, spacing, radius } from "../theme";

export function DiagnosticsScreen({ onClose }: { onClose?: () => void }) {
  const [log, setLog] = useState("loading\u2026");

  const refresh = useCallback(async () => {
    const text = await diagnostics.readAll();
    setLog(text || "(no log yet)");
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const share = async () => {
    const text = await diagnostics.readAll();
    await Share.share({ message: text || "(empty)" });
  };

  const clear = async () => {
    await diagnostics.clear();
    await refresh();
  };

  return (
    <View style={{ flex: 1, backgroundColor: colors.bg }}>
      <View style={{ padding: spacing.md }}>
        <View style={[styles.rowBetween, { marginBottom: spacing.md }]}>
          <View>
            <Text style={{ color: colors.text, fontSize: 17, fontWeight: "800" }}>诊断</Text>
            <Text style={styles.textFaint}>中继与配对日志</Text>
          </View>
          {onClose ? (
            <Pressable
              style={({ pressed }) => [styles.buttonGhost, { opacity: pressed ? 0.7 : 1, paddingVertical: spacing.sm, paddingHorizontal: spacing.lg }]}
              onPress={onClose}
            >
              <Text style={[styles.text, { fontWeight: "600" }]}>关闭</Text>
            </Pressable>
          ) : null}
        </View>
        <View style={[styles.row, { flexWrap: "wrap" }]}>
          <Pressable
            style={({ pressed }) => [local.btn, { opacity: pressed ? 0.7 : 1 }]}
            onPress={() => void refresh()}
          >
            <Text style={[styles.text, { fontSize: 13, fontWeight: "600" }]}>刷新</Text>
          </Pressable>
          <Pressable
            style={({ pressed }) => [local.btn, { opacity: pressed ? 0.7 : 1 }]}
            onPress={() => void share()}
          >
            <Text style={[styles.text, { fontSize: 13, fontWeight: "600" }]}>分享</Text>
          </Pressable>
          <Pressable
            style={({ pressed }) => [local.btn, { opacity: pressed ? 0.7 : 1 }]}
            onPress={() => void clear()}
          >
            <Text style={[styles.text, { fontSize: 13, fontWeight: "600", color: colors.danger }]}>清空</Text>
          </Pressable>
        </View>
      </View>
      <ScrollView
        style={{ flex: 1, paddingHorizontal: spacing.md }}
        contentContainerStyle={{ paddingBottom: spacing.xl }}
      >
        <View style={local.panel}>
          <Text style={[styles.mono, { fontSize: 11, color: colors.textDim, lineHeight: 17 }]}>{log}</Text>
        </View>
      </ScrollView>
    </View>
  );
}

const local = {
  btn: {
    paddingVertical: spacing.sm,
    paddingHorizontal: spacing.md,
    borderRadius: radius.pill,
    backgroundColor: colors.surfaceAlt,
    marginRight: spacing.sm,
    marginBottom: spacing.sm,
  } as const,
  panel: {
    backgroundColor: colors.surfaceAlt,
    borderRadius: radius.lg,
    padding: spacing.md,
  } as const,
};
// end of file