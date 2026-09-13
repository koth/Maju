import { useCallback, useEffect, useRef, useState } from "react";
import { View, Text, FlatList, Pressable, ActivityIndicator, RefreshControl, StyleSheet, Animated, Easing } from "react-native";
import { useFocusEffect } from "@react-navigation/native";
import { useAppController, useConnectionState } from "../../app/AppServicesContext";
import type { SessionListItem, WorkspaceSessionList } from "../../types";
import { splitSessionGroups } from "./session-groups";
import { MachineSwitcher } from "../machines/MachineSwitcher";
import { styles, colors, spacing, radius, shadows } from "../theme";
import { EmptyState } from "../ui/EmptyState";

// Lists sessions from `ListSessions`. The project-less chats workspace
// (marked `kind: "chats"`) is a first-class TAB next to 项目 — the header
// segmented control switches between the two, mirroring the desktop sidebar
// where chats sit beside projects instead of inside them. Projects render as
// collapsible rows that start collapsed (except the currently active
// workspace); expanding one reveals its session list. Pull-to-refresh
// re-issues `ListSessions`; the expand/collapse map is hoisted here so
// background refreshes never reset it.

type Group = WorkspaceSessionList;
type Tab = "chats" | "projects";

type Row =
  | { kind: "workspace"; key: string; group: Group }
  | {
      kind: "session";
      key: string;
      session: SessionListItem;
      isSessionActive: boolean;
      // Owning workspace root of this row's group — the PC needs it to route
      // the switch to the right workspace app.
      workspaceRoot: string;
    };

function sortSessions(sessions: SessionListItem[]): SessionListItem[] {
  return [...sessions].sort((a, b) => {
    return (
      getTimestamp(b.updated_at || b.created_at) -
      getTimestamp(a.updated_at || a.created_at)
    );
  });
}

function getTimestamp(value: string | undefined): number {
  if (!value) return 0;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function formatRelativeTime(value: string | undefined): string | null {
  const timestamp = getTimestamp(value);
  if (!timestamp) return null;
  const diffMs = Date.now() - timestamp;
  const minutes = Math.max(0, Math.floor(diffMs / 60000));
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  if (hours < 48) return "昨天";
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} 天前`;
  return new Date(timestamp).toLocaleDateString("zh-CN", {
    month: "numeric",
    day: "numeric",
  });
}

function statusLabel(status: string): string {
  switch (status) {
    case "Streaming":
    case "WaitingForTool":
      return "运行中";
    case "Interrupted":
      return "已中断";
    default:
      return "空闲";
  }
}

function statusTint(status: string): { color: string; bg: string; border: string } {
  switch (status) {
    case "Streaming":
    case "WaitingForTool":
      return { color: colors.success, bg: colors.successTint, border: colors.success };
    case "Interrupted":
      return { color: colors.danger, bg: colors.dangerTint, border: colors.danger };
    default:
      return { color: colors.textDim, bg: colors.surfaceAlt, border: colors.border };
  }
}

function avatarGradientColor(name: string): string {
  // Deterministic accent-ish hue per project so avatars read distinct.
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0;
  const palette = ["#5b8cff", "#8b5cf6", "#ec4899", "#f59e0b", "#10b981", "#06b6d4", "#f43f5e", "#a855f7"];
  return palette[h % palette.length];
}

export function SessionListScreen({
  onOpenSession,
  onOpenSettings,
}: {
  onOpenSession: (sessionId: string, title: string, workspaceRoot?: string | null) => void;
  onOpenSettings: () => void;
}) {
  const controller = useAppController();
  const connState = useConnectionState();
  const [tab, setTab] = useState<Tab>("projects");
  const [groups, setGroups] = useState<Group[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creatingFor, setCreatingFor] = useState<string | "global" | null>(null);

  // Per-workspace expand state keyed by workspace root. Unset entries fall
  // back to "expanded iff this is the active workspace" so the list mirrors
  // the desktop sidebar's default-collapsed projects.
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await controller.listSessions();
      setGroups(res.sessions);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [controller]);

  useEffect(() => {
    if (connState === "connected") void refresh();
    // A switch that ends in "disconnected" (handshake failed) must drop the
    // spinner handleWillSwitch raised, otherwise the list spins forever.
    else if (connState === "disconnected") setLoading(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connState]);

  useFocusEffect(
    useCallback(() => {
      if (connState === "connected") void refresh();
    }, [connState, refresh]),
  );

  const connected = connState === "connected";

  const toggleWorkspace = useCallback((root: string) => {
    setExpanded((current) => ({ ...current, [root]: !(current[root] ?? false) }));
  }, []);

  const createInWorkspace = useCallback(
    async (group: Group) => {
      if (!connected || group.workspace.location?.kind === "remote_linux") return;
      setCreatingFor(group.workspace.root);
      setError(null);
      try {
        const id = await controller.createSession({
          workspaceRoot: group.workspace.root,
        });
        onOpenSession(id, "新会话", group.workspace.root);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setCreatingFor(null);
      }
    },
    [connected, controller, onOpenSession],
  );

  const { chats: chatsGroups, projects: projectGroups } = splitSessionGroups(groups);
  const chatsGroup = chatsGroups[0];

  // A machine switch invalidates everything on screen: drop the previous
  // machine's projects immediately so they never render while the phone is
  // handshaking with the newly selected PC. The [connState] effect below
  // repopulates the list once the switch reaches "connected".
  const handleWillSwitch = useCallback(() => {
    setGroups([]);
    setLoading(true);
    setError(null);
  }, []);

  // 新建 is contextual: on the 聊天 tab it starts a chat in the chats
  // workspace; on the 项目 tab it creates a global (workspace-less) session
  // as before.
  const createFromHeader = useCallback(async () => {
    if (!connected) return;
    if (tab === "chats" && chatsGroup && chatsGroup.connected) {
      await createInWorkspace(chatsGroup);
      return;
    }
    setCreatingFor("global");
    setError(null);
    try {
      const id = await controller.createSession();
      onOpenSession(id, "新会话");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreatingFor(null);
    }
  }, [connected, tab, chatsGroup, createInWorkspace, controller, onOpenSession]);

  // Flatten the ACTIVE tab into section rows. Collapsed projects contribute
  // no session rows, so the list stays short like the desktop sidebar.
  const rows: Row[] = [];
  let anySessionVisible = false;
  if (tab === "chats") {
    if (chatsGroup) {
      for (const session of sortSessions(chatsGroup.sessions)) {
        rows.push({
          kind: "session",
          key: `${chatsGroup.workspace.root}:${session.id}`,
          session,
          isSessionActive:
            chatsGroup.is_active && session.id === chatsGroup.active_session_id,
          workspaceRoot: chatsGroup.workspace.root,
        });
      }
    }
  } else {
    for (const group of projectGroups) {
      rows.push({ kind: "workspace", key: `ws:${group.workspace.root}`, group });
      const isOpen = expanded[group.workspace.root] ?? group.is_active;
      if (!isOpen) continue;
      const sorted = sortSessions(group.sessions);
      for (const session of sorted) {
        anySessionVisible = true;
        rows.push({
          kind: "session",
          key: `${group.workspace.root}:${session.id}`,
          session,
          isSessionActive: group.is_active && session.id === group.active_session_id,
          workspaceRoot: group.workspace.root,
        });
      }
    }
  }

  return (
    <View style={styles.screen}>
      {/* Which PC this page is showing, with quick switching between bound
          machines right here (the machines landing screen is out of reach once
          a connection has been established). */}
      <View style={{ paddingHorizontal: spacing.lg, paddingTop: spacing.md }}>
        <MachineSwitcher onWillSwitch={handleWillSwitch} />
      </View>
      <View style={[styles.rowBetween, { paddingHorizontal: spacing.lg, paddingTop: spacing.sm, paddingBottom: spacing.md }]}>
        <View style={styles.row}>
          <TabPill label="项目" active={tab === "projects"} onPress={() => setTab("projects")} />
          <TabPill label="聊天" active={tab === "chats"} onPress={() => setTab("chats")} />
        </View>
        <View style={styles.row}>
          <Pressable
            style={({ pressed }) => [localStyles.headButton, { opacity: pressed ? 0.7 : 1 }]}
            onPress={onOpenSettings}
            hitSlop={8}
          >
            <Text style={[styles.text, { fontSize: 14, fontWeight: "600" }]}>设置</Text>
          </Pressable>
          <Pressable
            style={({ pressed }) => [
              localStyles.newButton,
              { opacity: pressed ? 0.9 : connected ? 1 : 0.45 },
            ]}
            onPress={() => void createFromHeader()}
            disabled={creatingFor !== null || !connected}
          >
            {creatingFor !== null || !connected ? (
              <ActivityIndicator color="#fff" size="small" />
            ) : (
              <Text style={styles.buttonText}>新建</Text>
            )}
          </Pressable>
        </View>
      </View>

      <View style={styles.hairline} />

      <FlatList
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingTop: spacing.sm, paddingBottom: spacing.xl }}
        refreshControl={<RefreshControl refreshing={loading} onRefresh={refresh} tintColor={colors.accent} />}
        data={rows}
        keyExtractor={(item) => item.key}
        renderItem={({ item }) =>
          item.kind === "workspace" ? (
            <WorkspaceRow
              group={item.group}
              expanded={expanded[item.group.workspace.root] ?? item.group.is_active}
              creating={creatingFor === item.group.workspace.root}
              onToggle={() => toggleWorkspace(item.group.workspace.root)}
              onCreate={() => void createInWorkspace(item.group)}
            />
          ) : (
            <SessionRow
              session={item.session}
              active={item.isSessionActive}
              onPress={
                item.session.id === ""
                  ? undefined
                  : () => onOpenSession(item.session.id, item.session.title, item.workspaceRoot)
              }
            />
          )
        }
        ListEmptyComponent={
          loading ? (
            <View style={styles.center}><ActivityIndicator color={colors.accent} /></View>
          ) : tab === "chats" ? (
            <EmptyState
              glyph={"\u{1F4AC}"}
              title={error ? "聊天加载失败" : "还没有聊天"}
              hint={error ? "下拉重试。" : "点右上角「新建」开始一个聊天。"}
            />
          ) : (
            <EmptyState
              glyph={"\u2302"}
              title={error ? "项目加载失败" : "还没有项目"}
              hint={error ? "下拉重试。" : "在桌面端打开工作区后,它会显示在这里。"}
            />
          )
        }
        ListFooterComponent={
          rows.length > 0 && error ? (
            <View style={{ padding: spacing.lg }}>
              <Text style={[styles.textFaint, { textAlign: "center" }]}>{error}</Text>
            </View>
          ) : null
        }
      />
      {tab === "projects" && !anySessionVisible && projectGroups.length > 0 && !loading ? (
        <Text style={localStyles.hint}>点开项目查看其中的会话</Text>
      ) : null}
    </View>
  );
}

// Segmented-control pill for the 聊天/项目 tabs.
function TabPill({
  label,
  active,
  onPress,
}: {
  label: string;
  active: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      style={({ pressed }) => [
        localStyles.tabPill,
        active ? localStyles.tabPillActive : null,
        { opacity: pressed ? 0.75 : 1 },
      ]}
      onPress={onPress}
      accessibilityRole="tab"
      accessibilityState={{ selected: active }}
    >
      <Text style={[localStyles.tabText, active ? localStyles.tabTextActive : null]}>{label}</Text>
    </Pressable>
  );
}

function WorkspaceRow({
  group,
  expanded,
  creating,
  onToggle,
  onCreate,
}: {
  group: Group;
  expanded: boolean;
  creating: boolean;
  onToggle: () => void;
  onCreate: () => void;
}) {
  const running = group.sessions.some(
    (s) => s.status === "Streaming" || s.status === "WaitingForTool",
  );
  const remote = group.workspace.location?.kind === "remote_linux";
  const dormant = remote && !group.connected;
  const initial = (group.workspace.name.trim()[0] ?? "?").toUpperCase();
  const tint = avatarGradientColor(group.workspace.name);
  return (
    <View
      style={[
        localStyles.projectCard,
        group.is_active ? { borderColor: colors.accent, ...shadows.card } : null,
      ]}
    >
      <Pressable
        style={({ pressed }) => [localStyles.projectRow, { opacity: pressed ? 0.8 : 1 }]}
        onPress={onToggle}
        accessibilityRole="button"
        accessibilityState={{ expanded }}
      >
        <View style={[localStyles.projectAvatar, { backgroundColor: tint }]}>
          <Text style={styles.avatarText}>{initial}</Text>
        </View>
        <View style={{ flex: 1, minWidth: 0 }}>
          <View style={styles.row}>
            <Text style={localStyles.projectName} numberOfLines={1}>
              {group.workspace.name}
            </Text>
            {running && !dormant ? <View style={localStyles.runningDot} /> : null}
          </View>
          <Text style={localStyles.projectMeta} numberOfLines={1}>
            {group.connected
              ? `${group.sessions.length} 个会话`
              : remote
                ? "远程 \u00b7 离线"
                : "离线"}
          </Text>
        </View>
        <AnimatedChevron expanded={expanded} />
        {!remote ? (
          <Pressable
            style={({ pressed }) => [localStyles.plusButton, { opacity: pressed ? 0.6 : 1 }]}
            hitSlop={10}
            onPress={(event) => {
              event.stopPropagation();
              onCreate();
            }}
            disabled={!group.connected || creating}
          >
            {creating ? (
              <ActivityIndicator color={colors.accent} size="small" />
            ) : (
              <Text style={localStyles.plusText}>+</Text>
            )}
          </Pressable>
        ) : null}
      </Pressable>
    </View>
  );
}

// Rotating disclosure arrow (single glyph, rotated 0deg -> 90deg) so the
// expand/collapse affordance animates instead of snapping between glyphs.
function AnimatedChevron({ expanded }: { expanded: boolean }) {
  const spin = useRef(new Animated.Value(expanded ? 1 : 0)).current;
  useEffect(() => {
    Animated.timing(spin, {
      toValue: expanded ? 1 : 0,
      duration: 180,
      easing: Easing.out(Easing.quad),
      useNativeDriver: true,
    }).start();
  }, [expanded, spin]);
  return (
    <Animated.View style={{ transform: [{ rotate: spin.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "90deg"] }) }] }}>
      <Text style={localStyles.chevron}>{"\u203A"}</Text>
    </Animated.View>
  );
}

function SessionRow({
  session,
  active,
  onPress,
}: {
  session: SessionListItem;
  active: boolean;
  onPress?: () => void;
}) {
  const time = formatRelativeTime(session.updated_at || session.created_at);
  const tint = statusTint(session.status);
  return (
    <Pressable
      style={({ pressed }) => [localStyles.sessionRow, active && localStyles.sessionActive, { opacity: pressed ? 0.7 : 1 }]}
      onPress={onPress}
      disabled={!onPress}
    >
      {active ? <View style={localStyles.activeBar} /> : null}
      <View style={{ flex: 1, minWidth: 0 }}>
        <Text style={[styles.text, { fontWeight: "600", fontSize: 15 }]} numberOfLines={1}>
          {session.title}
        </Text>
        <View style={[styles.row, { marginTop: 4 }]}>
          <View style={[styles.chip, { backgroundColor: tint.bg, borderColor: tint.border }]}>
            <Text style={{ color: tint.color, fontSize: 11, fontWeight: "600" }}>{statusLabel(session.status)}</Text>
          </View>
          {time ? <Text style={[styles.textFaint, { marginLeft: spacing.sm }]}>{time}</Text> : null}
        </View>
      </View>
    </Pressable>
  );
}

const localStyles = StyleSheet.create({
  tabPill: {
    paddingVertical: spacing.xs + 1,
    paddingHorizontal: spacing.md + 2,
    borderRadius: radius.pill,
    borderWidth: 1,
    borderColor: "transparent",
    marginRight: spacing.sm,
  },
  tabPillActive: {
    backgroundColor: colors.accentTint,
    borderColor: colors.accent,
  },
  tabText: {
    fontSize: 16,
    fontWeight: "700",
    color: colors.textDim,
  },
  tabTextActive: {
    color: colors.accent,
  },
  headButton: {
    paddingVertical: spacing.sm,
    paddingHorizontal: spacing.md,
    borderRadius: radius.pill,
    borderWidth: 1,
    borderColor: colors.borderStrong,
    backgroundColor: colors.surface,
    marginRight: spacing.sm,
  },
  newButton: {
    backgroundColor: colors.accent,
    borderRadius: radius.pill,
    paddingVertical: spacing.sm + 1,
    paddingHorizontal: spacing.lg + 4,
    alignItems: "center",
    justifyContent: "center",
    minWidth: 56,
    minHeight: 36,
    ...shadows.glow,
  },
  projectCard: {
    backgroundColor: colors.surface,
    borderRadius: radius.lg,
    marginHorizontal: spacing.sm,
    marginTop: spacing.xs,
    marginBottom: spacing.xs,
    borderWidth: 1,
    borderColor: colors.border,
    overflow: "hidden",
  },
  projectRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.md,
    paddingHorizontal: spacing.md,
  },
  projectAvatar: {
    width: 38,
    height: 38,
    borderRadius: 12,
    alignItems: "center",
    justifyContent: "center",
    marginRight: spacing.md,
  },
  projectName: {
    color: colors.text,
    fontSize: 15,
    fontWeight: "700",
    flexShrink: 1,
  },
  projectMeta: {
    color: colors.textFaint,
    fontSize: 12,
    marginTop: 2,
  },
  chevron: {
    color: colors.textDim,
    fontSize: 13,
    width: 16,
    marginLeft: spacing.sm,
  },
  runningDot: {
    width: 8,
    height: 8,
    borderRadius: 4,
    backgroundColor: colors.success,
    marginLeft: spacing.sm,
  },
  plusButton: {
    width: 30,
    height: 30,
    borderRadius: 10,
    alignItems: "center",
    justifyContent: "center",
    borderWidth: 1,
    borderColor: colors.borderStrong,
    backgroundColor: colors.surfaceAlt,
    marginLeft: spacing.sm,
  },
  plusText: {
    color: colors.text,
    fontSize: 18,
    lineHeight: 20,
  },
  sessionRow: {
    flexDirection: "row",
    alignItems: "center",
    backgroundColor: colors.surfaceAlt,
    borderRadius: radius.md,
    paddingVertical: spacing.sm + 2,
    paddingHorizontal: spacing.md,
    marginHorizontal: spacing.md,
    marginTop: spacing.xs,
    borderWidth: 1,
    borderColor: "transparent",
  },
  sessionActive: {
    borderColor: colors.accent,
    backgroundColor: colors.surface,
  },
  activeBar: {
    width: 4,
    height: 26,
    borderRadius: 2,
    backgroundColor: colors.accent,
    marginRight: spacing.sm,
  },
  hint: {
    color: colors.textFaint,
    fontSize: 12,
    textAlign: "center",
    paddingVertical: spacing.sm,
  },
});
// end of file
