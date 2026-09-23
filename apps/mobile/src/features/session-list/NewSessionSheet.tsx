import { useEffect, useState } from "react";
import {
  View,
  Text,
  Pressable,
  Modal,
  StyleSheet,
  ScrollView,
  ActivityIndicator,
} from "react-native";
import { useAppController } from "../../app/AppServicesContext";
import type { AgentCliId, AgentOptionsList } from "../../types";
import { colors, spacing, radius } from "../theme";

// New-session picker: the agent (and, for the DeepSeek Harness, the agent
// preset / mode) chosen at creation time — the mobile mirror of the desktop
// sidebar's create modal. The choice list comes from the PC over
// `list_agent_options` (settings snapshot for agents, best-effort harness
// preset list). The preset half may spawn the `dsh web` host on the PC, so
// the fetch can take a moment: the sheet opens on a spinner instead of
// guessing a local list.
//
// Model selection deliberately does NOT live here: a session's model is
// switched in-conversation from the composer's model control (desktop
// parity — creation picks the agent/channel, not the model).
interface Props {
  visible: boolean;
  /** Target workspace for the new session; null = the global (project-less)
   *  session space, same as the header 新建 on the projects tab before the
   *  picker existed. */
  workspaceRoot: string | null;
  onClose: () => void;
  onCreated: (sessionId: string) => void;
}

export function NewSessionSheet({ visible, workspaceRoot, onClose, onCreated }: Props) {
  const controller = useAppController();
  const [options, setOptions] = useState<AgentOptionsList | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [agent, setAgent] = useState<AgentCliId | null>(null);
  const [preset, setPreset] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!visible) return;
    setOptions(null);
    setError(null);
    setAgent(null);
    setPreset(null);
    setSubmitting(false);
    let active = true;
    setLoading(true);
    controller
      .listAgentOptions()
      .then((opts) => {
        if (!active) return;
        setOptions(opts);
        // Preselect the PC's current default agent, falling back to the
        // first entry; uninstalled agents stay selectable-but-disabled in
        // the list, never preselected (the default is by definition usable).
        const preferred = opts.agents.find((entry) => entry.selected) ?? opts.agents[0] ?? null;
        setAgent(preferred?.id ?? null);
        setPreset(opts.dsh_default_preset ?? null);
      })
      .catch((e: unknown) => {
        if (active) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [visible, controller]);

  const busy = loading || submitting;
  const agents = options?.agents ?? [];
  const presets = options?.dsh_presets ?? [];
  const showPresets = agent === "deepseek-harness";

  const confirm = async () => {
    if (submitting || !agent) return;
    setSubmitting(true);
    setError(null);
    try {
      const id = await controller.createSession({
        workspaceRoot,
        agent,
        preset: showPresets ? preset : null,
      });
      onCreated(id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      visible={visible}
      transparent
      animationType="fade"
      onRequestClose={() => {
        if (!busy) onClose();
      }}
    >
      <View style={sheetStyles.backdrop}>
        {/* Full-screen tap-catcher under the card: tapping the backdrop
            dismisses, tapping the card (rendered above it) does not. */}
        <Pressable
          style={StyleSheet.absoluteFill}
          onPress={() => {
            if (!busy) onClose();
          }}
          accessibilityLabel="关闭新建会话"
        />
        <View style={sheetStyles.card}>
          <View style={sheetStyles.titleRow}>
            <Text style={sheetStyles.title}>新建会话</Text>
            <Pressable
              onPress={() => {
                if (!busy) onClose();
              }}
              hitSlop={8}
              accessibilityRole="button"
              accessibilityLabel="关闭"
            >
              <Text style={sheetStyles.close}>{"\u2715"}</Text>
            </Pressable>
          </View>

          {loading ? (
            <View style={sheetStyles.loadingRow}>
              <ActivityIndicator color={colors.textDim} />
              <Text style={sheetStyles.loadingText}>正在获取智能体列表…</Text>
            </View>
          ) : (
            <ScrollView style={sheetStyles.body} nestedScrollEnabled showsVerticalScrollIndicator={false}>
              <Text style={sheetStyles.sectionLabel}>智能体</Text>
              {agents.length === 0 && !error ? (
                <Text style={sheetStyles.hint}>没有可用的智能体，请先在桌面端安装并配置。</Text>
              ) : null}
              {agents.map((entry) => {
                const selected = entry.id === agent;
                const disabled = !entry.installed;
                return (
                  <Pressable
                    key={entry.id}
                    style={({ pressed }) => [
                      sheetStyles.optionRow,
                      selected && sheetStyles.optionRowSelected,
                      { opacity: disabled ? 0.4 : pressed ? 0.7 : 1 },
                    ]}
                    disabled={disabled}
                    onPress={() => setAgent(entry.id)}
                    accessibilityRole="radio"
                    accessibilityState={{ selected, disabled }}
                  >
                    <Text
                      style={[
                        sheetStyles.optionLabel,
                        selected && { color: colors.accentBright },
                      ]}
                    >
                      {entry.label}
                    </Text>
                    <Text style={sheetStyles.optionMeta}>
                      {disabled ? "未安装" : selected ? "\u2713" : ""}
                    </Text>
                  </Pressable>
                );
              })}

              {showPresets ? (
                <>
                  <Text style={[sheetStyles.sectionLabel, { marginTop: spacing.lg }]}>
                    模式（预设）
                  </Text>
                  <OptionRow
                    label="部署默认"
                    selected={preset === null}
                    onPress={() => setPreset(null)}
                  />
                  {presets.map((entry) => (
                    <OptionRow
                      key={entry.id}
                      label={entry.label}
                      description={entry.description ?? undefined}
                      selected={preset === entry.id}
                      onPress={() => setPreset(entry.id)}
                    />
                  ))}
                  {presets.length === 0 ? (
                    <Text style={sheetStyles.hint}>
                      未能获取预设列表（确认 dsh 已安装并运行），将使用部署默认。
                    </Text>
                  ) : null}
                </>
              ) : null}

              {error ? <Text style={sheetStyles.error}>{error}</Text> : null}

              <Pressable
                style={({ pressed }) => [
                  sheetStyles.confirmButton,
                  { opacity: submitting || !agent ? 0.5 : pressed ? 0.85 : 1 },
                ]}
                disabled={submitting || !agent}
                onPress={() => void confirm()}
                accessibilityRole="button"
                accessibilityLabel="创建会话"
              >
                {submitting ? (
                  <ActivityIndicator color="#fff" size="small" />
                ) : (
                  <Text style={sheetStyles.confirmText}>创建会话</Text>
                )}
              </Pressable>
            </ScrollView>
          )}
        </View>
      </View>
    </Modal>
  );
}

function OptionRow({
  label,
  description,
  selected,
  onPress,
}: {
  label: string;
  description?: string;
  selected: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      style={({ pressed }) => [
        sheetStyles.optionRow,
        selected && sheetStyles.optionRowSelected,
        { opacity: pressed ? 0.7 : 1 },
      ]}
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityState={{ selected }}
    >
      <View style={{ flex: 1, minWidth: 0 }}>
        <Text style={[sheetStyles.optionLabel, selected && { color: colors.accentBright }]}>
          {label}
        </Text>
        {description ? (
          <Text style={sheetStyles.optionDescription} numberOfLines={2}>
            {description}
          </Text>
        ) : null}
      </View>
      <Text style={sheetStyles.optionMeta}>{selected ? "\u2713" : ""}</Text>
    </Pressable>
  );
}

const sheetStyles = StyleSheet.create({
  backdrop: {
    flex: 1,
    backgroundColor: colors.scrim,
    alignItems: "center",
    justifyContent: "center",
    padding: spacing.lg,
  },
  card: {
    width: "100%",
    maxWidth: 420,
    maxHeight: "82%",
    backgroundColor: colors.surfaceRaised,
    borderRadius: radius.xl,
    padding: spacing.lg,
  },
  body: {
    flexGrow: 0,
  },
  titleRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    marginBottom: spacing.md,
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
  loadingRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: spacing.sm,
    paddingVertical: spacing.xxl,
  },
  loadingText: {
    color: colors.textDim,
    fontSize: 13,
  },
  sectionLabel: {
    color: colors.textFaint,
    fontSize: 12,
    fontWeight: "600",
    marginBottom: spacing.xs,
    textTransform: "uppercase",
    letterSpacing: 0.4,
  },
  optionRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.sm + 2,
    paddingHorizontal: spacing.md,
    borderRadius: radius.md,
    gap: spacing.md,
  },
  optionRowSelected: {
    backgroundColor: colors.surfaceAlt,
  },
  optionLabel: {
    color: colors.text,
    fontSize: 14,
    fontWeight: "500",
  },
  optionDescription: {
    color: colors.textFaint,
    fontSize: 12,
    marginTop: 2,
  },
  optionMeta: {
    color: colors.textFaint,
    fontSize: 12,
    minWidth: 36,
    textAlign: "right",
  },
  hint: {
    color: colors.textFaint,
    fontSize: 12,
    lineHeight: 17,
    paddingVertical: spacing.sm,
  },
  error: {
    color: colors.danger,
    fontSize: 12,
    lineHeight: 17,
    paddingVertical: spacing.sm,
  },
  confirmButton: {
    backgroundColor: colors.accent,
    borderRadius: radius.pill,
    paddingVertical: spacing.sm + 2,
    alignItems: "center",
    justifyContent: "center",
    marginTop: spacing.lg,
    minHeight: 40,
  },
  confirmText: {
    color: "#fff",
    fontSize: 14,
    fontWeight: "600",
  },
});
// end of file
