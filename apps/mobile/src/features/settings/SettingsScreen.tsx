import { View, Text, Pressable, Alert } from "react-native";
import { useAppController, useConnectionState, useSubscriptionState } from "../../app/AppServicesContext";
import { styles, colors, spacing } from "../theme";

// Settings: connection state, device id, subscription status, unbind/re-pair,
// and a kill switch (disconnect). All state comes from the controller.
export function SettingsScreen({ onRescan }: { onRescan: () => void }) {
  const controller = useAppController();
  const connState = useConnectionState();
  const subscription = useSubscriptionState();

  const unbind = () => {
    Alert.alert(
      "Unbind device?",
      "This clears the bound account. You will need to re-scan to pair again.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Unbind",
          style: "destructive",
          onPress: async () => {
            await controller.unbindAndClear();
            await controller.disconnect();
            onRescan();
          },
        },
      ],
    );
  };

  const kill = async () => {
    await controller.disconnect();
    onRescan();
  };

  const subStatus = subscription.active
    ? `active · ${subscription.plan ?? "—"}`
    : "free / inactive";

  return (
    <View style={styles.screen}>
      <View style={{ padding: spacing.md }}>
        <Text style={styles.title}>Settings</Text>

        <View style={styles.card}>
          <Text style={styles.textDim}>Connection</Text>
          <Text style={styles.text}>{connState}</Text>
        </View>

        <View style={styles.card}>
          <Text style={styles.textDim}>Device id</Text>
          <Text style={styles.mono}>{controller.deviceIdValue ?? "(generating)"}</Text>
        </View>

        <View style={styles.card}>
          <Text style={styles.textDim}>Subscription</Text>
          <Text style={styles.text}>{subStatus}</Text>
          {subscription.expiresAt ? (
            <Text style={styles.textDim}>expires {new Date(subscription.expiresAt).toLocaleDateString()}</Text>
          ) : null}
        </View>

        <Pressable style={[styles.buttonGhost, { marginTop: spacing.md }]} onPress={unbind}>
          <Text style={[styles.text, { color: colors.danger }]}>Unbind & re-pair</Text>
        </Pressable>
        <Pressable style={[styles.buttonDanger, { marginTop: spacing.sm }]} onPress={kill}>
          <Text style={styles.buttonText}>Disconnect (kill switch)</Text>
        </Pressable>
      </View>
    </View>
  );
}
// end of file
