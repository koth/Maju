import { memo } from "react";
import { View, Text, FlatList } from "react-native";
import type { UiSnapshot } from "../../types";
import { MarkdownBody } from "./MarkdownBody";
import { ToolCallCard } from "../tooling/ToolCallCard";
import { styles, colors, spacing } from "../theme";

interface Props {
  snapshot: UiSnapshot;
  onStopTool?: (toolCallId: string) => void;
}

// Interleaves messages and tool calls chronologically along the timeline, the
// same shape the desktop ConversationTimeline renders. Message ids and tool
// ids are resolved against the snapshot's messages/tools arrays.
function ConversationTimelineImpl({ snapshot, onStopTool }: Props) {
  const messageById = new Map(snapshot.messages.map((message) => [message.id, message]));
  const toolById = new Map(snapshot.tools.map((tool) => [tool.call_id, tool]));

  const rows = snapshot.timeline.map((item, index) => {
    let node: React.ReactNode;
    if (item === "Thinking") {
      node = <Text style={[styles.textDim, { fontStyle: "italic", padding: spacing.sm }]}>thinking…</Text>;
    } else if ("Message" in item) {
      const message = messageById.get(item.Message);
      node = message ? <MarkdownBody body={message.body} /> : <Text style={styles.textDim}>(missing message)</Text>;
    } else {
      const tool = toolById.get(item.Tool);
      node = tool ? <ToolCallCard tool={tool} onStop={onStopTool} /> : <Text style={styles.textDim}>(missing tool)</Text>;
    }
    return { key: `${index}`, node };
  });

  return (
    <FlatList
      style={{ flex: 1 }}
      contentContainerStyle={{ padding: spacing.sm, paddingBottom: spacing.xl }}
      data={rows}
      keyExtractor={(row) => row.key}
      renderItem={({ item }) => <View>{item.node}</View>}
      ItemSeparatorComponent={() => <View style={{ height: spacing.xs }} />}
      ListEmptyComponent={
        <View style={styles.center}>
          <Text style={styles.textDim}>No messages yet.</Text>
        </View>
      }
    />
  );
}

export const ConversationTimeline = memo(ConversationTimelineImpl);
// end of file
