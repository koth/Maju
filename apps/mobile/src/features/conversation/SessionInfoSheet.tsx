import { memo, useEffect, useState } from "react";
import { View, Text, Pressable, Modal, StyleSheet, ScrollView } from "react-native";
import { useSnapshot } from "../../app/AppServicesContext";
import { diagnostics } from "../../util/diagnostics";
import type { UsageTokenBreakdown } from "../../types";
import { colors, spacing, radius } from "../theme";

// Kebab (⋮) button for the conversation's native header + the info sheet it
// opens (the session's model/agent and live context-usage numbers).
//
// The button and the sheet are SEPARATE components on purpose: the sheet is
// mounted inside ConversationScreen's tree, where snapshot emits provably
// re-render (the timeline and turn-changes bar live there). Header config
// components (react-native-screens `headerRight`) can miss context/state
// updates, so the header button only signals "open" through a tiny module
// emitter and never reads the snapshot itself.

type Listener = () => void;
const openListeners = new Set<Listener>();

function formatTokens(value: number | null | undefined): string {
  if (value == null) return "—";
  const abs = Math.abs(value);
  if (abs >= 1_000_000) return `${(value / 1_000_000).toFixed(abs >= 10_000_000 ? 0 : 2)}M`;
  if (abs >= 1_000) return `${(value / 1_000).toFixed(abs >= 10_000 ? 0 : 1)}K`;
  return String(value);
}

// The dsh token meter reports per-field breakdowns (input/output/cache) and
// never a total_tokens, so sum the fields the same way the desktop
// settings/workbench does: cache_read is a subset of input and reasoning a
// subset of output — adding either would double-count.
function usageTokenTotal(tokens?: UsageTokenBreakdown | null): number | null {
  if (!tokens) return null;
  if (tokens.total_tokens != null) return tokens.total_tokens;
  if (tokens.input_tokens == null && tokens.output_tokens == null) return null;
  return (tokens.input_tokens ?? 0) + (tokens.output_tokens ?? 0);
}

export function openSessionInfo(): void {
  for (const listener of openListeners) listener();
}

/** Header button: pure trigger, no data. */
export const SessionInfoButton = memo(function SessionInfoButton() {
  return (
    <Pressable
      onPress={openSessionInfo}
      hitSlop={8}
      style={sheetStyles.kebab}
      accessibilityRole="button"
      accessibilityLabel="会话信息"
    >
      <View style={sheetStyles.kebabColumn}>
        <View style={sheetStyles.dot} />
        <View style={sheetStyles.dot} />
        <View style={sheetStyles.dot} />
      </View>
    </Pressable>
  );
});

/** Mount inside the conversation screen tree. Renders the info modal. */
export function SessionInfoSheet() {
  const snapshot = useSnapshot();
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const listener: Listener = () => setOpen(true);
    openListeners.add(listener);
    return () => {
      openListeners.delete(listener);
    };
  }, []);
  const close = () => setOpen(false);

  const session = snapshot?.session;
  const usage = snapshot?.usage;
  const used = usage?.context?.used_tokens ?? null;
  const windowTokens = usage?.context?.window_tokens ?? null;
  const pct =
    used != null && windowTokens ? Math.min(100, Math.max(0, Math.round((used / windowTokens) * 100))) : null;
  const turnTokens = usageTokenTotal(usage?.current_turn);
  const totalTokens = usageTokenTotal(usage?.session_total);
  // Provider: prefer the usage summary entry for the ACTIVE model (usage
  // events carry the provider route the harness actually billed); fall back
  // to the newest by-model entry, then to the model id's route prefix
  // (BYOK model ids are "provider/model").
  const modelId = session?.model ?? null;
  const provider =
    usage?.by_model?.find((entry) => entry.model === modelId)?.provider ??
    usage?.by_model?.[usage.by_model.length - 1]?.provider ??
    (modelId && modelId.includes("/") ? modelId.split("/")[0] : null);
  // dsh sessions report their agent preset through the mode field (the
  // LegacyMode control's value label lands in session.mode).
  const preset = session?.mode ?? null;

  // Diagnostics: when the sheet is open, report exactly what this component
  // sees, so a store-vs-view mismatch is observable from the Diagnostics log.
  useEffect(() => {
    if (!open) return;
    diagnostics.log(
      "usage",
      `sheet: rev=${snapshot?.revision ?? "?"} used=${usage?.context?.used_tokens ?? "null"} window=${usage?.context?.window_tokens ?? "null"} byModel=${usage?.by_model?.length ?? 0} turns=${snapshot?.turn_changes?.length ?? "?"} msgs=${snapshot?.messages?.length ?? 0}`,
    );
  }, [open, snapshot, usage]);

  return (
    <Modal visible={open} transparent animationType="fade" onRequestClose={close}>
      <View style={sheetStyles.backdrop}>
        {/* Full-screen tap-catcher under the card: tapping the backdrop
            dismisses, tapping the card (rendered above it) does not. */}
        <Pressable style={StyleSheet.absoluteFill} onPress={close} accessibilityLabel="关闭会话信息" />
        {/* maxHeight + scrollable body: a centered card taller than the
            available space overflows BOTH ends (flexbox centering), which
            clipped the bottom rows. Capping the height keeps every row
            reachable on short screens. */}
        <View style={sheetStyles.card}>
          <View style={sheetStyles.titleRow}>
            <Text style={sheetStyles.title}>会话信息</Text>
            <Pressable onPress={close} hitSlop={8} accessibilityRole="button" accessibilityLabel="关闭">
              <Text style={sheetStyles.close}>{"\u2715"}</Text>
            </Pressable>
          </View>
          <ScrollView style={sheetStyles.body} nestedScrollEnabled showsVerticalScrollIndicator={false}>

          {session ? (
            <>
              <InfoRow label="模型" value={session.model || "—"} mono />
              {preset ? <InfoRow label="预设" value={preset} /> : null}
              {provider ? <InfoRow label="Provider" value={provider} mono /> : null}
              {session.agent_cli ? <InfoRow label="智能体" value={session.agent_cli} /> : null}
            </>
          ) : null}

          <View style={sheetStyles.row}>
            <Text style={sheetStyles.rowLabel}>上下文</Text>
            {/* The value column must be a child of a ROW parent: `flex: 1` on
                a direct child of this (column) card collapses its height to
                zero, which rendered the bar and text "invisibly" even when the
                data was present. */}
            <View style={sheetStyles.valueCol}>
              {used != null || windowTokens != null ? (
                <>
                  {windowTokens ? (
                    <View style={sheetStyles.barTrack}>
                      <View style={[sheetStyles.barFill, { width: `${pct ?? 0}%` }]} />
                    </View>
                  ) : null}
                  <Text style={sheetStyles.rowValue}>
                    {windowTokens
                      ? `${formatTokens(used)} / ${formatTokens(windowTokens)}${pct != null ? ` \u00b7 ${pct}%` : ""}`
                      : `${formatTokens(used)} tokens`}
                  </Text>
                </>
              ) : (
                <Text style={[sheetStyles.rowValue, { color: colors.textFaint }]}>
                  暂无用量数据(跑一轮对话后上报)
                </Text>
              )}
            </View>
          </View>

          {turnTokens ? <InfoRow label="本轮" value={`${formatTokens(turnTokens)} tokens`} /> : null}
          {totalTokens ? <InfoRow label="会话累计" value={`${formatTokens(totalTokens)} tokens`} /> : null}
          </ScrollView>
        </View>
      </View>
    </Modal>
  );
}

function InfoRow({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <View style={sheetStyles.row}>
      <Text style={sheetStyles.rowLabel}>{label}</Text>
      <Text style={[mono ? sheetStyles.rowValueMono : sheetStyles.rowValue]} numberOfLines={2}>
        {value}
      </Text>
    </View>
  );
}

const sheetStyles = StyleSheet.create({
  kebab: {
    paddingHorizontal: spacing.sm + 2,
    paddingVertical: spacing.xs,
    marginRight: spacing.xs,
  },
  // Three separate dots: compact (3px dot, 3px gap), slightly nudged up so it
  // optically centers against the native header title.
  kebabColumn: {
    alignItems: "center",
    gap: 3,
    transform: [{ translateY: -1 }],
  },
  dot: {
    width: 3,
    height: 3,
    borderRadius: 1.5,
    backgroundColor: colors.text,
  },
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
  // Scrollable body: grows with content, caps at the card's maxHeight.
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
  row: {
    flexDirection: "row",
    alignItems: "flex-start",
    marginTop: spacing.sm,
  },
  rowLabel: {
    color: colors.textFaint,
    fontSize: 12,
    width: 64,
    lineHeight: 20,
  },
  rowValue: {
    color: colors.text,
    fontSize: 14,
    lineHeight: 20,
    flex: 1,
  },
  rowValueMono: {
    color: colors.text,
    fontSize: 13,
    lineHeight: 20,
    flex: 1,
    fontFamily: "monospace",
  },
  barTrack: {
    height: 6,
    borderRadius: 3,
    backgroundColor: colors.border,
    overflow: "hidden",
    marginBottom: spacing.xs + 2,
  },
  valueCol: {
    flex: 1,
    minWidth: 0,
  },
  barFill: {
    height: 6,
    borderRadius: 3,
    backgroundColor: colors.accent,
  },
});
// end of file
