import { useCallback, useEffect, useState } from "react";
import {
  ActivityIndicator,
  Modal,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from "react-native";
import { useAppController, useConnectionState } from "../../app/AppServicesContext";
import type { BoundDevice } from "../../account/binding";
import { colors, radius, shadows, spacing, styles } from "../theme";
import {
  machineLabel,
  machinePhase,
  machinePhaseLabel,
  machineTint,
  relayHost,
  type MachinePhase,
} from "./machine-view";

// Quick "which PC am I talking to?" switcher for the project page. The
// machines landing screen is only reachable before the first connect (or after
// the Settings kill switch), so without this a user bound to several PCs had
// to leave the session list to change machines. The chip shows the active
// machine with a green dot while the link is live; tapping it lists every
// bound machine and switches in place (fresh E2E handshake, session list
// reloads for the newly selected machine).
//
// Stateless data-wise: the bound list is re-read from the controller whenever
// the sheet opens, and the active machine is read from the controller on each
// render (the connection-state subscription re-renders across every
// transition), so there is no cached copy to go stale.

const PHASE_DOT: Record<MachinePhase, string> = {
  connected: colors.success,
  connecting: colors.warn,
  idle: colors.textFaint,
};

export function MachineSwitcher({ onWillSwitch }: { onWillSwitch?: () => void }) {
  const controller = useAppController();
  const connState = useConnectionState();
  const [open, setOpen] = useState(false);
  const [devices, setDevices] = useState<BoundDevice[]>([]);
  const [loading, setLoading] = useState(false);
  const [switchingTo, setSwitchingTo] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const activePeer = controller.activePeerDeviceId;

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setDevices(await controller.listMachines());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [controller]);

  // Load once for the chip label, then again on every open in case the bound
  // list changed (pairing a new machine happens outside this screen).
  useEffect(() => {
    void load();
  }, [load]);

  const openSheet = useCallback(() => {
    setOpen(true);
    void load();
  }, [load]);

  const close = useCallback(() => {
    if (switchingTo) return;
    setError(null);
    setOpen(false);
  }, [switchingTo]);

  const switchTo = useCallback(
    async (device: BoundDevice) => {
      if (switchingTo) return;
      const isActive = device.peer_device_id === activePeer;
      // Tapping the machine already connected to is a no-op — do not tear the
      // live connection down just to re-dial the same peer.
      if (isActive && connState === "connected") {
        setOpen(false);
        return;
      }
      setSwitchingTo(device.peer_device_id);
      setError(null);
      // The caller clears the old machine's session list before the switch:
      // showing machine A's sessions while talking to machine B is a lie.
      onWillSwitch?.();
      try {
        await controller.connectToBoundDevice(device.peer_device_id);
        setOpen(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setSwitchingTo(null);
      }
    },
    [activePeer, connState, controller, onWillSwitch, switchingTo],
  );

  const activeDevice = devices.find((d) => d.peer_device_id === activePeer) ?? null;
  const activeLabel = activeDevice
    ? machineLabel(activeDevice)
    : activePeer
      ? machineLabel({ peer_device_id: activePeer })
      : "未选择电脑";
  const activePhase: MachinePhase = machinePhase(activePeer !== null, connState);

  return (
    <>
      <Pressable
        style={({ pressed }) => [localStyles.chip, { opacity: pressed ? 0.75 : 1 }]}
        onPress={openSheet}
        accessibilityRole="button"
        accessibilityLabel={`当前电脑 ${activeLabel}，点按切换`}
      >
        <View style={[localStyles.dot, { backgroundColor: PHASE_DOT[activePhase] }]} />
        <Text style={localStyles.chipLabel} numberOfLines={1}>
          {activeLabel}
        </Text>
        <Text style={localStyles.chipChevron}>{"\u25BE"}</Text>
      </Pressable>

      <Modal visible={open} transparent animationType="fade" onRequestClose={close}>
        <View style={modalStyles.backdrop}>
          <Pressable style={StyleSheet.absoluteFill} onPress={close} accessibilityLabel="关闭切换电脑" />
          <View style={modalStyles.card}>
            <View style={modalStyles.titleRow}>
              <Text style={modalStyles.title}>切换电脑</Text>
              <Pressable onPress={close} hitSlop={8} accessibilityRole="button" accessibilityLabel="关闭">
                <Text style={modalStyles.close}>{"\u2715"}</Text>
              </Pressable>
            </View>
            <Text style={modalStyles.subtitle}>
              已配对的电脑。点按切换到它的会话，绿色圆点表示当前已连接。
            </Text>

            {loading && devices.length === 0 ? (
              <View style={modalStyles.center}>
                <ActivityIndicator color={colors.accent} />
              </View>
            ) : devices.length === 0 ? (
              <Text style={modalStyles.empty}>
                还没有已配对的电脑。到「设置」解绑后重新扫描电脑端的配对二维码。
              </Text>
            ) : (
              <ScrollView
                style={modalStyles.body}
                nestedScrollEnabled
                showsVerticalScrollIndicator={false}
              >
                {devices.map((device) => {
                  const isActive = device.peer_device_id === activePeer;
                  const phase = machinePhase(isActive, connState);
                  return (
                    <MachineOption
                      key={device.peer_device_id}
                      device={device}
                      active={isActive}
                      phase={phase}
                      switching={switchingTo === device.peer_device_id}
                      disabled={switchingTo !== null}
                      onPress={() => void switchTo(device)}
                    />
                  );
                })}
              </ScrollView>
            )}

            {error ? <Text style={modalStyles.error}>{error}</Text> : null}
          </View>
        </View>
      </Modal>
    </>
  );
}

function MachineOption({
  device,
  active,
  phase,
  switching,
  disabled,
  onPress,
}: {
  device: BoundDevice;
  active: boolean;
  phase: MachinePhase;
  switching: boolean;
  disabled: boolean;
  onPress: () => void;
}) {
  const label = machineLabel(device);
  const host = relayHost(device.relay_endpoint);
  const meta = active
    ? [machinePhaseLabel(phase), host].filter(Boolean).join(" \u00b7 ")
    : (host ?? "relay endpoint unknown");

  return (
    <Pressable
      style={({ pressed }) => [
        optionStyles.row,
        active && optionStyles.rowActive,
        { opacity: disabled && !switching ? 0.5 : pressed ? 0.75 : 1 },
      ]}
      onPress={onPress}
      disabled={disabled}
      accessibilityRole="button"
      accessibilityState={{ selected: active }}
      accessibilityLabel={`${label}${active ? "，当前电脑" : ""}`}
    >
      <View style={[optionStyles.avatar, { backgroundColor: machineTint(device.peer_device_id) }]}>
        <Text style={styles.avatarText}>{label.trim()[0]?.toUpperCase() ?? "P"}</Text>
      </View>
      <View style={{ flex: 1, minWidth: 0 }}>
        <View style={localStyles.nameRow}>
          <Text style={optionStyles.name} numberOfLines={1}>
            {label}
          </Text>
          {active ? <View style={[localStyles.dot, { backgroundColor: PHASE_DOT[phase] }]} /> : null}
        </View>
        <Text style={optionStyles.meta} numberOfLines={1}>
          {meta}
        </Text>
      </View>
      {switching ? (
        <ActivityIndicator color={colors.accent} size="small" />
      ) : active && phase === "connected" ? (
        <Text style={optionStyles.check}>{"\u2713"}</Text>
      ) : null}
    </Pressable>
  );
}

const localStyles = StyleSheet.create({
  chip: {
    alignSelf: "flex-start",
    flexDirection: "row",
    alignItems: "center",
    maxWidth: "78%",
    paddingVertical: spacing.xs + 2,
    paddingHorizontal: spacing.md,
    borderRadius: radius.pill,
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surfaceAlt,
  },
  dot: {
    width: 8,
    height: 8,
    borderRadius: 4,
    marginRight: spacing.sm,
  },
  chipLabel: {
    color: colors.text,
    fontSize: 13,
    fontWeight: "700",
    flexShrink: 1,
  },
  chipChevron: {
    color: colors.textDim,
    fontSize: 10,
    marginLeft: spacing.sm,
  },
  nameRow: {
    flexDirection: "row",
    alignItems: "center",
  },
});

const modalStyles = StyleSheet.create({
  backdrop: {
    flex: 1,
    backgroundColor: "rgba(0,0,0,0.6)",
    alignItems: "center",
    justifyContent: "center",
    padding: spacing.lg,
  },
  card: {
    width: "100%",
    maxWidth: 420,
    maxHeight: "82%",
    backgroundColor: colors.surface,
    borderRadius: radius.lg,
    borderWidth: 1,
    borderColor: colors.border,
    padding: spacing.lg,
    ...shadows.raised,
  },
  titleRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
  },
  title: {
    color: colors.text,
    fontSize: 16,
    fontWeight: "800",
  },
  close: {
    color: colors.textDim,
    fontSize: 16,
    paddingHorizontal: spacing.xs,
  },
  subtitle: {
    color: colors.textFaint,
    fontSize: 12,
    lineHeight: 17,
    marginTop: spacing.xs,
    marginBottom: spacing.sm,
  },
  body: {
    flexGrow: 0,
  },
  center: {
    paddingVertical: spacing.xl,
    alignItems: "center",
  },
  empty: {
    color: colors.textDim,
    fontSize: 13,
    lineHeight: 19,
    paddingVertical: spacing.md,
  },
  error: {
    color: colors.danger,
    fontSize: 12,
    lineHeight: 17,
    marginTop: spacing.sm,
  },
});

const optionStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "center",
    backgroundColor: colors.surfaceAlt,
    borderRadius: radius.md,
    borderWidth: 1,
    borderColor: "transparent",
    paddingVertical: spacing.sm + 2,
    paddingHorizontal: spacing.md,
    marginTop: spacing.xs,
  },
  rowActive: {
    borderColor: colors.accent,
    backgroundColor: colors.surfaceRaised,
  },
  avatar: {
    width: 38,
    height: 38,
    borderRadius: 12,
    alignItems: "center",
    justifyContent: "center",
    marginRight: spacing.md,
  },
  name: {
    color: colors.text,
    fontSize: 15,
    fontWeight: "700",
    flexShrink: 1,
  },
  meta: {
    color: colors.textFaint,
    fontSize: 12,
    marginTop: 2,
  },
  check: {
    color: colors.success,
    fontSize: 16,
    fontWeight: "800",
    marginLeft: spacing.sm,
  },
});
