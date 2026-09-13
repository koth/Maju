import { useEffect, useState } from "react";
import { View, Text, Pressable, Alert, Switch } from "react-native";
import { useAppController, useAlertSettings, useConnectionState, useSubscriptionState } from "../../app/AppServicesContext";
import {
  getNotificationPermissionState,
  requestNotificationPermission,
  type NotificationPermissionState,
} from "../notifications/local-notification";
import type { AlertSettings } from "../notifications/settings";
import { styles, colors, spacing } from "../theme";

// Settings: connection state, device id, subscription status, turn-completion
// alerts, unbind/re-pair, and a kill switch (disconnect). All state comes from
// the controller.
export function SettingsScreen({
  onRescan,
  onOpenDiagnostics,
}: {
  onRescan: () => void;
  onOpenDiagnostics?: () => void;
}) {
  const controller = useAppController();
  const connState = useConnectionState();
  const subscription = useSubscriptionState();
  const alerts = useAlertSettings();
  const [notifPermission, setNotifPermission] =
    useState<NotificationPermissionState>("undetermined");

  useEffect(() => {
    void getNotificationPermissionState().then(setNotifPermission);
  }, []);

  const updateAlerts = (patch: Partial<AlertSettings>) => {
    void controller.setAlertSettings({ ...alerts, ...patch });
  };

  const toggleSystemNotifications = async (value: boolean) => {
    if (!value) {
      updateAlerts({ systemNotifications: false });
      return;
    }
    // First enable requests the runtime permission (Android 13+ / iOS).
    const granted = await requestNotificationPermission();
    setNotifPermission(await getNotificationPermissionState());
    updateAlerts({ systemNotifications: granted });
  };

  const unbind = () => {
    Alert.alert(
      "解绑所有设备？",
      "This clears every bound machine from this phone. You will need to re-scan a QR code to pair again.",
      [
        { text: "取消", style: "cancel" },
        {
          text: "全部解绑",
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
  const connected = connState === "connected";

  return (
    <View style={styles.screen}>
      <View style={{ padding: spacing.lg }}>
        <Text style={[styles.subtitle, { marginTop: spacing.sm }]}>
          管理这台手机与 Maju 电脑之间的连接。
        </Text>

        <Text style={styles.sectionHeader}>连接</Text>
        <View style={styles.card}>
          <View style={styles.rowBetween}>
            <Text style={styles.textDim}>状态</Text>
            <View style={styles.row}>
              <View
                style={{
                  width: 7,
                  height: 7,
                  borderRadius: 3.5,
                  marginRight: spacing.sm,
                  backgroundColor: connected ? colors.success : colors.warn,
                }}
              />
              <Text style={{ color: colors.text, fontSize: 14, fontWeight: "500" }}>{connState}</Text>
            </View>
          </View>
        </View>

        <Text style={styles.sectionHeader}>完成提醒</Text>
        <View style={styles.card}>
          <AlertToggle
            label="轮次结束时提醒"
            value={alerts.enabled}
            onChange={(v) => updateAlerts({ enabled: v })}
          />
          <AlertToggle
            label="提示音"
            value={alerts.sound}
            disabled={!alerts.enabled}
            onChange={(v) => updateAlerts({ sound: v })}
          />
          <AlertToggle
            label="震动"
            value={alerts.vibration}
            disabled={!alerts.enabled}
            onChange={(v) => updateAlerts({ vibration: v })}
          />
          <AlertToggle
            label="仅后台时提醒"
            value={alerts.backgroundOnly}
            disabled={!alerts.enabled}
            onChange={(v) => updateAlerts({ backgroundOnly: v })}
          />
          <AlertToggle
            label="系统通知（应用在后台时）"
            value={alerts.systemNotifications}
            disabled={!alerts.enabled}
            onChange={(v) => void toggleSystemNotifications(v)}
          />
          {notifPermission !== "granted" ? (
            <Pressable
              onPress={() => {
                void (async () => {
                  await requestNotificationPermission();
                  setNotifPermission(await getNotificationPermissionState());
                })();
              }}
              style={({ pressed }) => ({ opacity: pressed ? 0.6 : 1 })}
            >
              <Text style={[styles.textFaint, { marginTop: spacing.xs, color: colors.warn }]}>
                {notifPermission === "denied"
                  ? "通知权限被拒绝：点此重新申请，或到 设置→应用→Maju→通知 手动开启。"
                  : "通知权限未授予：点此立即申请，否则应用在后台时收不到完成通知。"}
              </Text>
            </Pressable>
          ) : null}
          <Text style={[styles.textFaint, { marginTop: spacing.sm, lineHeight: 17 }]}>
            提醒依赖 App 进程与电脑的连接存活；应用被系统结束后无法收到提醒（后续版本将支持服务端推送）。部分国产
            ROM 会很快冻结后台应用，请将 Maju 的省电策略设为“无限制”并允许后台运行，后台通知才可靠。
          </Text>
        </View>

        <Text style={styles.sectionHeader}>设备</Text>
        <View style={styles.card}>
          <Text style={styles.textDim}>设备 ID</Text>
          <Text style={[styles.mono, { marginTop: spacing.xs, fontSize: 12, color: colors.text }]}>
            {controller.deviceIdValue ?? "(generating)"}
          </Text>
        </View>

        <Text style={styles.sectionHeader}>订阅</Text>
        <View style={styles.card}>
          <Text style={styles.textDim}>套餐</Text>
          <Text style={[styles.text, { marginTop: spacing.xs, fontWeight: "600" }]}>{subStatus}</Text>
          {subscription.expiresAt ? (
            <Text style={[styles.textFaint, { marginTop: spacing.xs }]}>
              {`到期 ${new Date(subscription.expiresAt).toLocaleDateString()}`}
            </Text>
          ) : null}
        </View>

        <Pressable
          style={({ pressed }) => [styles.buttonGhost, { marginTop: spacing.lg, opacity: pressed ? 0.7 : 1 }]}
          onPress={unbind}
        >
          <Text style={[styles.text, { color: colors.danger, fontWeight: "600" }]}>解绑所有设备</Text>
        </Pressable>
        {onOpenDiagnostics ? (
          <Pressable
            style={({ pressed }) => [styles.buttonGhost, { marginTop: spacing.sm, opacity: pressed ? 0.7 : 1 }]}
            onPress={onOpenDiagnostics}
          >
            <Text style={[styles.text, { fontWeight: "600" }]}>诊断日志</Text>
          </Pressable>
        ) : null}
        <Pressable
          style={({ pressed }) => [styles.buttonDanger, { marginTop: spacing.sm, opacity: pressed ? 0.9 : 1 }]}
          onPress={kill}
        >
          <Text style={styles.buttonText}>断开连接</Text>
        </Pressable>
      </View>
    </View>
  );
}

function AlertToggle({
  label,
  value,
  disabled,
  onChange,
}: {
  label: string;
  value: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <View style={[styles.rowBetween, { paddingVertical: spacing.xs, opacity: disabled ? 0.45 : 1 }]}>
      <Text style={styles.text}>{label}</Text>
      <Switch
        value={value}
        disabled={disabled}
        onValueChange={onChange}
        trackColor={{ false: colors.borderStrong, true: colors.text }}
        thumbColor={value ? colors.bg : colors.surfaceRaised}
      />
    </View>
  );
}
