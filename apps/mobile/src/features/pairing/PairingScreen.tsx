import { useState } from "react";
import { View, Text, TextInput, Pressable, ActivityIndicator, ScrollView, Platform, StyleSheet } from "react-native";
import { CameraView, useCameraPermissions, scanFromURLAsync } from "expo-camera";
import type { BarcodeScanningResult } from "expo-camera";
import * as ImagePicker from "expo-image-picker";
import { useAppController, useConnectionState } from "../../app/AppServicesContext";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { WsTransport } from "../../relay/transport";
import { parsePairingQr } from "../../pairing/qr-parse";
import { styles, colors, spacing, radius } from "../theme";

type Phase = "idle" | "dialing" | "authenticating" | "pairing" | "connected" | "error";

// Pairing: scan the PC's QR (relay_endpoint + pairing_code + pc_device_pubkey),
// dial the relay over TLS WebSocket, run DeviceAuth + the E2E handshake, and
// resync state. Manual entry accepts the raw QR JSON as a fallback. Free-tier
// pairing needs no account; a successful pair lands on the session list.
// Reached from the machines list via "Pair a new PC"; `onCancel` (when given)
// returns there without pairing — each successful scan binds an ADDITIONAL
// machine, it never overwrites the existing bindings.
export function PairingScreen({
  onOpenDiagnostics,
  onCancel,
}: {
  onOpenDiagnostics?: () => void;
  onCancel?: () => void;
}) {
  const controller = useAppController();
  const connState = useConnectionState();
  const insets = useSafeAreaInsets();
  const [permission, requestPermission] = useCameraPermissions();
  const [manual, setManual] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [error, setError] = useState<string | null>(null);

  async function pairFromJson(qrJson: string) {
    setError(null);
    try {
      const allowInsecure =
        process.env.EXPO_PUBLIC_RELAY_ALLOW_INSECURE_WS === "1";
      const qr = parsePairingQr(qrJson, allowInsecure);
      setPhase("dialing");
      const transport = new WsTransport(qr.relay_endpoint);
      await transport.ready;
      setPhase("authenticating");
      await controller.pairFromTransport(transport, qrJson, allowInsecure);
      setPhase("connected");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPhase("error");
    }
  }

  function onBarcodeScanned(result: BarcodeScanningResult) {
    if (phase === "dialing" || phase === "authenticating" || phase === "pairing") return;
    if (!result.data) return;
    void pairFromJson(result.data);
  }

  async function pickQrImage() {
    if (busy) return;
    setError(null);
    try {
      const perm = await ImagePicker.requestMediaLibraryPermissionsAsync();
      if (!perm.granted) {
        setError("photo permission denied");
        setPhase("error");
        return;
      }
      const result = await ImagePicker.launchImageLibraryAsync({
        mediaTypes: ["images"],
        allowsEditing: false,
        quality: 1,
      });
      if (result.canceled || !result.assets?.length) return;
      const uri = result.assets[0].uri;
      const scanned = await scanFromURLAsync(uri, ["qr"]);
      if (!scanned.length) {
        setError("no QR code found in image");
        setPhase("error");
        return;
      }
      void pairFromJson(scanned[0].data);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPhase("error");
    }
  }
  const busy = phase === "dialing" || phase === "authenticating" || phase === "pairing" || connState === "connecting" || connState === "authenticating" || connState === "paired/e2e";

  return (
    // Rendered from the headerless machines screen: pad the status bar inset
    // here since the root SafeAreaView no longer applies a top edge.
    <ScrollView
      style={styles.screen}
      contentContainerStyle={{ padding: spacing.lg, paddingTop: insets.top + spacing.lg, paddingBottom: spacing.xxl }}
    >
      {onCancel ? (
        <Pressable
          onPress={onCancel}
          hitSlop={10}
          style={({ pressed }) => ({
            alignSelf: "flex-start",
            paddingVertical: spacing.xs,
            opacity: pressed ? 0.6 : 1,
            marginBottom: spacing.xs,
          })}
          accessibilityRole="button"
          accessibilityLabel="返回设备列表"
        >
          <Text style={{ color: colors.accent, fontSize: 15, fontWeight: "500" }}>{"\u2039"} 设备</Text>
        </Pressable>
      ) : null}
      <View style={{ alignItems: "center", marginTop: spacing.xl, marginBottom: spacing.xl }}>
        <View style={pairStyles.mark}>
          <Text style={pairStyles.markText}>M</Text>
        </View>
        <Text style={[styles.title, { textAlign: "center", marginTop: spacing.lg, marginBottom: spacing.xs }]}>
          配对 Maju 电脑
        </Text>
        <Text style={[styles.subtitle, { textAlign: "center", marginBottom: 0, maxWidth: 320 }]}>
          扫描电脑上显示的二维码即可绑定。配对过程端到端加密（X25519 + ChaCha20-Poly1305）。
        </Text>
      </View>

      {!permission || permission.status !== "granted" ? (
        <View style={[styles.card, { alignItems: "center" }]}>
          <Text style={[styles.text, { textAlign: "center", marginBottom: spacing.md }]}>
            需要相机权限才能扫描二维码。
          </Text>
          <Pressable
            style={({ pressed }) => [styles.button, { opacity: pressed ? 0.9 : 1, minWidth: 180 }]}
            onPress={() => requestPermission()}
          >
            <Text style={styles.buttonText}>授权相机</Text>
          </Pressable>
        </View>
      ) : (
        <View style={pairStyles.camera}>
          <CameraView
            facing="back"
            barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
            onBarcodeScanned={onBarcodeScanned}
            style={{ flex: 1 }}
          />
          <View pointerEvents="none" style={pairStyles.cameraFrame} />
        </View>
      )}

      <Text style={styles.sectionHeader}>或上传二维码图片</Text>
      <Pressable
        style={({ pressed }) => [styles.buttonGhost, { marginTop: spacing.sm, opacity: pressed ? 0.85 : 1, borderColor: colors.borderStrong }, busy && { opacity: 0.5 }]}
        disabled={busy}
        onPress={pickQrImage}
      >
        <Text style={[styles.text, { fontWeight: "600" }]}>从相册选取二维码</Text>
      </Pressable>

      <Text style={styles.sectionHeader}>或粘贴配对内容</Text>
      <TextInput
        style={[styles.input, { minHeight: 84, fontFamily: "monospace", fontSize: 13 }]}
        placeholder='{"relay_endpoint":"wss://…","pairing_code":"…","pc_device_pubkey":"…"}'
        placeholderTextColor={colors.textDim}
        value={manual}
        onChangeText={setManual}
        multiline
        autoCapitalize="none"
        autoCorrect={false}
      />
      <Pressable
        style={({ pressed }) => [styles.button, { marginTop: spacing.sm, opacity: pressed ? 0.9 : 1 }, manual.trim().length === 0 && { opacity: 0.5 }]}
        disabled={busy || manual.trim().length === 0}
        onPress={() => pairFromJson(manual.trim())}
      >
        <Text style={styles.buttonText}>开始配对</Text>
      </Pressable>

      <View style={{ marginTop: spacing.xl, alignItems: "center" }}>
        {busy && <ActivityIndicator color={colors.textDim} />}
        {error ? (
          <Text style={[styles.textFaint, { color: colors.danger, textAlign: "center", lineHeight: 18 }]}>
            {error}
          </Text>
        ) : null}
        {phase === "connected" && (
          <Text style={[styles.text, { color: colors.success, marginTop: spacing.sm }]}>
            配对成功，已建立端到端加密连接。
          </Text>
        )}
        {onOpenDiagnostics ? (
          <Pressable
            style={({ pressed }) => ({ marginTop: spacing.md, padding: spacing.sm, opacity: pressed ? 0.7 : 1 })}
            onPress={onOpenDiagnostics}
          >
            <Text style={[styles.text, { color: colors.textFaint, fontSize: 13 }]}>查看诊断日志</Text>
          </Pressable>
        ) : null}
      </View>
    </ScrollView>
  );
}

const pairStyles = StyleSheet.create({
  // A plain glyph mark. The bordered accent square with a glow read as a
  // widget rather than a brand mark.
  mark: {
    width: 60,
    height: 60,
    borderRadius: 18,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: colors.surfaceAlt,
  },
  markText: { color: colors.text, fontSize: 26, fontWeight: "700" },
  camera: {
    overflow: "hidden",
    height: 280,
    borderRadius: radius.lg,
  },
  cameraFrame: {
    position: "absolute",
    top: spacing.lg,
    left: spacing.lg,
    right: spacing.lg,
    bottom: spacing.lg,
    borderRadius: radius.md,
    borderWidth: 2,
    borderColor: "rgba(255,255,255,0.35)",
  },
});
// end of file
