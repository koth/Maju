import { useCallback, useEffect, useRef, useState } from "react";
import { View, Text, FlatList, Pressable, ActivityIndicator, RefreshControl, StyleSheet, Animated, Easing } from "react-native";
import { useFocusEffect } from "@react-navigation/native";
import { useAppController, useConnectionState } from "../../app/AppServicesContext";
import type { SessionListItem, WorkspaceSessionList } from "../../types";
import { splitSessionGroups } from "./session-groups";
import { NewSessionSheet } from "./NewSessionSheet";
import { MachineSwitcher } from "../machines/MachineSwitcher";
import { styles, typeScale, colors, spacing, radius } from "../theme";
import { EmptyState } from "../ui/EmptyState";

// Lists sessions from `ListSessions`. The project-less chats workspace
// (marked `kind: "chats"`) is a first-class TAB next to 项目 — the header
// segmented control switches between the two, mirroring the desktop sidebar
// where chats sit beside projects instead of inside them. Projects render as
// collapsible rows that start collapsed (except the currently active
// workspace); expanding one reveals its session list. Pull-to-refresh
// re-issues `ListSessions`; the expand/collapse map is hoisted here so
// background refreshes never reset it.
//
// Presentation (ChatGPT-leaning): the header carries only two things — which
// PC this list belongs to (left) and the primary 新建 action (right). Tabs are
// plain text with an accent underline, not bordered pills, and the project /
// session rows are flat full-width rows separated by hairlines instead of a
// bordered card per item (a card per row is what made the list read as noise).

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

/** Status is plain colored text, not a tinted pill: only the two abnormal
 * states earn a color, idle stays neutral (it is the default, so coloring it
 * just adds noise to every row). */
function statusColor(status: string): string {
  switch (status) {
    case "Streaming":
    case "WaitingForTool":
      return colors.success;
    case "Interrupted":
      return colors.danger;
    default:
      return colors.textFaint;
  }
}

export function SessionListScreen({
  onOpenSession,
}: {
  onOpenSession: (sessionId: string, title: string, workspaceRoot?: string | null) => void;
}) {
  const controller = useAppController();
  const connState = useConnectionState();
  const [tab, setTab] = useState<Tab>("projects");
  const [groups, setGroups] = useState<Group[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Target of the open new-session picker: which workspace the new session
  // lands in (null = the global project-less space). The picker sheet owns
  // the agent/preset choice and the create call itself; creation errors
  // surface inside the sheet, not here.
  const [pickerTarget, setPickerTarget] = useState<{ workspaceRoot: string | null } | null>(null);

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
    (group: Group) => {
      if (!connected || group.workspace.location?.kind === "remote_linux") return;
      setPickerTarget({ workspaceRoot: group.workspace.root });
    },
    [connected],
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
  // as before. Both paths open the agent/preset picker instead of creating
  // immediately — the target workspace is all that is decided here.
  const createFromHeader = useCallback(() => {
    if (!connected) return;
    if (tab === "chats" && chatsGroup && chatsGroup.connected) {
      createInWorkspace(chatsGroup);
      return;
    }
    setPickerTarget({ workspaceRoot: null });
  }, [connected, tab, chatsGroup, createInWorkspace]);

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
      {/* Header: which PC (left) + the primary action (right). Settings lives in
          the native header — repeating it here made the row read as four
          equally-weighted tabs. */}
      <View style={localStyles.header}>
        <MachineSwitcher onWillSwitch={handleWillSwitch} />
        <Pressable
          style={({ pressed }) => [
            localStyles.newButton,
            { opacity: !connected ? 0.4 : pressed ? 0.85 : 1 },
          ]}
          onPress={createFromHeader}
          disabled={!connected}
          accessibilityRole="button"
          accessibilityLabel="新建会话"
        >
          <Text style={localStyles.newButtonText}>新建</Text>
        </Pressable>
      </View>

      <View style={localStyles.tabs}>
        <TabButton label="项目" active={tab === "projects"} onPress={() => setTab("projects")} />
        <TabButton label="聊天" active={tab === "chats"} onPress={() => setTab("chats")} />
      </View>

      <View style={styles.hairline} />

      <FlatList
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingBottom: spacing.xl }}
        refreshControl={<RefreshControl refreshing={loading} onRefresh={refresh} tintColor={colors.textDim} />}
        data={rows}
        keyExtractor={(item) => item.key}
        renderItem={({ item }) =>
          item.kind === "workspace" ? (
            <WorkspaceRow
              group={item.group}
              expanded={expanded[item.group.workspace.root] ?? item.group.is_active}
              onToggle={() => toggleWorkspace(item.group.workspace.root)}
              onCreate={() => createInWorkspace(item.group)}
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
            <View style={localStyles.emptyLoading}><ActivityIndicator color={colors.textDim} /></View>
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
      {/* Capture the target at render time so onCreated navigates with the
          same workspace root the sheet created in, even after state moves on. */}
      <NewSessionSheet
        visible={pickerTarget !== null}
        workspaceRoot={pickerTarget?.workspaceRoot ?? null}
        onClose={() => setPickerTarget(null)}
        onCreated={(id) => {
          const workspaceRoot = pickerTarget?.workspaceRoot ?? null;
          setPickerTarget(null);
          onOpenSession(id, "新会话", workspaceRoot);
        }}
      />
    </View>
  );
}

// Tabs are text + a 2px accent underline. A bordered pill around each tab is
// pure chrome: two pills plus the action button made the header look like
// four peer controls.
function TabButton({
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
      style={({ pressed }) => [localStyles.tab, { opacity: pressed ? 0.7 : 1 }]}
      onPress={onPress}
      accessibilityRole="tab"
      accessibilityState={{ selected: active }}
    >
      <Text style={[localStyles.tabText, active && localStyles.tabTextActive]}>{label}</Text>
      <View style={[localStyles.tabUnderline, active && localStyles.tabUnderlineActive]} />
    </Pressable>
  );
}

function WorkspaceRow({
  group,
  expanded,
  onToggle,
  onCreate,
}: {
  group: Group;
  expanded: boolean;
  onToggle: () => void;
  onCreate: () => void;
}) {
  const running = group.sessions.some(
    (s) => s.status === "Streaming" || s.status === "WaitingForTool",
  );
  const remote = group.workspace.location?.kind === "remote_linux";
  const dormant = remote && !group.connected;
  const initial = (group.workspace.name.trim()[0] ?? "?").toUpperCase();
  // Offline is the steady state, so it is NOT printed per row — only remote
  // offline (which the user must act on) earns copy.
  const meta = group.connected
    ? `${group.sessions.length} 个会话`
    : remote
      ? "远程 · 离线"
      : null;
  return (
    <View>
      <Pressable
        style={({ pressed }) => [localStyles.projectRow, pressed && localStyles.rowPressed]}
        onPress={onToggle}
        accessibilityRole="button"
        accessibilityState={{ expanded }}
      >
        <View style={localStyles.glyph}>
          <Text style={localStyles.glyphText}>{initial}</Text>
        </View>
        <View style={{ flex: 1, minWidth: 0 }}>
          <View style={styles.row}>
            <Text style={localStyles.projectName} numberOfLines={1}>
              {group.workspace.name}
            </Text>
            {running && !dormant ? <View style={localStyles.runningDot} /> : null}
          </View>
          {meta ? <Text style={localStyles.projectMeta} numberOfLines={1}>{meta}</Text> : null}
        </View>
        {!remote ? (
          <Pressable
            style={({ pressed }) => [localStyles.plusButton, { opacity: pressed ? 0.6 : 1 }]}
            hitSlop={10}
            onPress={(event) => {
              event.stopPropagation();
              onCreate();
            }}
            disabled={!group.connected}
            accessibilityRole="button"
            accessibilityLabel={`在 ${group.workspace.name} 新建会话`}
          >
            <Text style={[localStyles.plusText, !group.connected && { color: colors.textFaint }]}>+</Text>
          </Pressable>
        ) : null}
        <AnimatedChevron expanded={expanded} />
      </Pressable>
      <View style={styles.hairlineInset} />
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
  return (
    <Pressable
      style={({ pressed }) => [localStyles.sessionRow, pressed && localStyles.rowPressed]}
      onPress={onPress}
      disabled={!onPress}
    >
      {active ? <View style={localStyles.activeBar} /> : null}
      <View style={{ flex: 1, minWidth: 0 }}>
        <Text
          style={[localStyles.sessionTitle, active && { color: colors.accentBright }]}
          numberOfLines={1}
        >
          {session.title}
        </Text>
        <View style={[styles.row, { marginTop: 3 }]}>
          <Text style={[typeScale.meta, { color: statusColor(session.status) }]}>
            {statusLabel(session.status)}
          </Text>
          {time ? <Text style={[typeScale.meta, { color: colors.textFaint, marginLeft: spacing.sm }]}>{time}</Text> : null}
        </View>
      </View>
    </Pressable>
  );
}

const localStyles = StyleSheet.create({
  header: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: spacing.sm,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.md,
    paddingBottom: spacing.md,
  },
  newButton: {
    backgroundColor: colors.accent,
    borderRadius: radius.pill,
    paddingVertical: spacing.sm + 1,
    paddingHorizontal: spacing.lg + 4,
    alignItems: "center",
    justifyContent: "center",
    minWidth: 64,
    minHeight: 36,
  },
  newButtonText: { color: "#fff", fontSize: 14, fontWeight: "600" },

  tabs: {
    flexDirection: "row",
    alignItems: "flex-end",
    gap: spacing.xl,
    paddingHorizontal: spacing.lg,
  },
  tab: { paddingBottom: spacing.sm },
  tabText: { fontSize: 16, fontWeight: "600", color: colors.textFaint },
  tabTextActive: { color: colors.text },
  tabUnderline: { height: 2, borderRadius: 1, marginTop: spacing.sm, backgroundColor: "transparent" },
  tabUnderlineActive: { backgroundColor: colors.accent },

  rowPressed: { backgroundColor: colors.surface },

  projectRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.md,
    paddingHorizontal: spacing.lg,
    gap: spacing.md,
  },
  glyph: {
    width: 32,
    height: 32,
    borderRadius: radius.sm,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: colors.surfaceAlt,
  },
  glyphText: { color: colors.textDim, fontWeight: "600", fontSize: 14 },
  projectName: { color: colors.text, fontSize: 15, fontWeight: "500", flexShrink: 1 },
  projectMeta: { color: colors.textFaint, fontSize: 12, marginTop: 2 },
  runningDot: {
    width: 7,
    height: 7,
    borderRadius: 3.5,
    backgroundColor: colors.success,
    marginLeft: spacing.sm,
  },
  chevron: { color: colors.textFaint, fontSize: 15, width: 14, textAlign: "center" },
  plusButton: {
    width: 28,
    height: 28,
    borderRadius: radius.sm,
    alignItems: "center",
    justifyContent: "center",
  },
  plusText: { color: colors.textDim, fontSize: 20, lineHeight: 22 },

  sessionRow: {
    flexDirection: "row",
    alignItems: "center",
    paddingVertical: spacing.sm + 2,
    paddingLeft: spacing.lg + 32 + spacing.md,
    paddingRight: spacing.lg,
  },
  sessionTitle: { color: colors.textDim, fontSize: 14, fontWeight: "400" },
  activeBar: {
    position: "absolute",
    left: spacing.lg + 10,
    top: spacing.sm + 4,
    bottom: spacing.sm + 4,
    width: 2,
    borderRadius: 1,
    backgroundColor: colors.accent,
  },

  emptyLoading: { paddingVertical: spacing.xxl, alignItems: "center" },
  hint: {
    color: colors.textFaint,
    fontSize: 12,
    textAlign: "center",
    paddingVertical: spacing.sm,
  },
});
// end of file
