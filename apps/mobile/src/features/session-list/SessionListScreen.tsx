import { useEffect, useState } from "react";
import { View, Text, FlatList, Pressable, ActivityIndicator, RefreshControl } from "react-native";
import { useAppController, useConnectionState } from "../../app/AppServicesContext";
import type { WorkspaceSessionList } from "../../types";
import { styles, colors, spacing } from "../theme";

type SessionListItem = WorkspaceSessionList["sessions"][number];

// Lists sessions from `ListSessions` grouped by workspace. "New session"
// calls `CreateSession` then routes to the conversation. Pull-to-refresh
// re-issues `ListSessions`.
export function SessionListScreen({ onOpenSession }: { onOpenSession: (sessionId: string, title: string) => void }) {
  const controller = useAppController();
  const connState = useConnectionState();
  const [groups, setGroups] = useState<WorkspaceSessionList[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const refresh = async () => {
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
  };

  useEffect(() => {
    if (connState === "connected") void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connState]);

  const createNew = async () => {
    setCreating(true);
    setError(null);
    try {
      const id = await controller.createSession();
      onOpenSession(id, "New session");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreating(false);
    }
  };

  const items: Array<{ key: string; workspace: string; session: SessionListItem }> = [];
  for (const group of groups) {
    for (const session of group.sessions) {
      items.push({
        key: `${group.workspace.id}:${session.id}`,
        workspace: group.workspace.name,
        session,
      });
    }
  }

  return (
    <View style={styles.screen}>
      <View style={[styles.rowBetween, { padding: spacing.md }]}>
        <Text style={styles.title}>Sessions</Text>
        <Pressable style={[styles.button, { paddingVertical: spacing.sm }]} onPress={createNew} disabled={creating}>
          {creating ? <ActivityIndicator color="#fff" /> : <Text style={styles.buttonText}>New</Text>}
        </Pressable>
      </View>
      <FlatList
        style={{ flex: 1 }}
        refreshControl={<RefreshControl refreshing={loading} onRefresh={refresh} tintColor={colors.accent} />}
        data={items}
        keyExtractor={(item) => item.key}
        renderItem={({ item }) => (
          <Pressable style={styles.card} onPress={() => onOpenSession(item.session.id, item.session.title)}>
            <Text style={styles.textDim}>{item.workspace}</Text>
            <Text style={[styles.text, { fontWeight: "600" }]}>{item.session.title}</Text>
            <Text style={styles.textDim}>{item.session.status}</Text>
          </Pressable>
        )}
        ListEmptyComponent={
          loading ? (
            <View style={styles.center}><ActivityIndicator color={colors.accent} /></View>
          ) : (
            <View style={styles.center}>
              <Text style={styles.textDim}>{error ?? "No sessions. Create one to start."}</Text>
            </View>
          )
        }
      />
    </View>
  );
}
// end of file
