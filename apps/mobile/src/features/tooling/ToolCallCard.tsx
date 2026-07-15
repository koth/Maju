import { memo, useState } from "react";
import { View, Text, Pressable } from "react-native";
import type { ToolInvocation } from "../../types";
import { styles, colors, spacing } from "../theme";

interface Props {
  tool: ToolInvocation;
  onStop?: (toolCallId: string) => void;
}

type Tab = "summary" | "diff" | "logs" | "raw";

function tabLabel(tool: ToolInvocation, tab: Tab): string {
  if (tab === "summary") return "Summary";
  if (tab === "diff") return `Diff${tool.diff_previews.length ? ` (${tool.diff_previews.length})` : ""}`;
  if (tab === "logs") return `Logs${tool.logs.length ? ` (${tool.logs.length})` : ""}`;
  return "Raw";
}

function tabDisabled(tool: ToolInvocation, tab: Tab): boolean {
  if (tab === "diff") return tool.diff_previews.length === 0;
  if (tab === "logs") return tool.logs.length === 0;
  if (tab === "raw") return !tool.raw_input && !tool.raw_output;
  return false;
}

// Fold/expand tool invocation card: summary + status, expandable diff
// previews, logs, and raw input/output. Ported from the desktop ToolCallCard
// but rendered with RN primitives (no Monaco/diff lib in the mobile MVP).
function ToolCallCardImpl({ tool, onStop }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [section, setSection] = useState<Tab>("summary");
  const running = tool.status === "Pending" || tool.status === "Running";
  const statusColor =
    tool.status === "Succeeded"
      ? colors.success
      : tool.status === "Failed" || tool.status === "Interrupted"
        ? colors.danger
        : running
          ? colors.warn
          : colors.textDim;

  const tabs: Tab[] = ["summary", "diff", "logs", "raw"];

  return (
    <View style={[styles.card, { padding: spacing.sm }]}>
      <Pressable style={styles.rowBetween} onPress={() => setExpanded((e) => !e)}>
        <View style={[styles.row, { flex: 1, flexWrap: "wrap" }]}>
          <Text style={[styles.textDim, { fontFamily: "monospace", fontSize: 12 }]}>{tool.name}</Text>
          {tool.kind && tool.kind !== "permission" ? (
            <Text style={[styles.badge, { marginLeft: spacing.xs, fontSize: 11 }]}>{tool.kind}</Text>
          ) : null}
        </View>
        <Text style={[styles.text, { fontSize: 12, color: statusColor }]}>{tool.status}</Text>
      </Pressable>

      {tool.summary ? (
        <Pressable onPress={() => setExpanded((e) => !e)}>
          <Text style={[styles.text, { marginTop: spacing.xs, fontSize: 14 }]}>{tool.summary}</Text>
        </Pressable>
      ) : null}

      {expanded ? (
        <View style={{ marginTop: spacing.sm }}>
          <View style={[styles.row, { marginBottom: spacing.xs, flexWrap: "wrap" }]}>
            {tabs.map((tab) => {
              const active = section === tab;
              const disabled = tabDisabled(tool, tab);
              return (
                <Pressable
                  key={tab}
                  onPress={() => setSection(tab)}
                  disabled={disabled}
                  style={[styles.badge, active && { backgroundColor: colors.accent }, { marginRight: spacing.xs, marginBottom: spacing.xs }, disabled && { opacity: 0.4 }]}
                >
                  <Text style={[styles.text, { fontSize: 12 }, active && { color: "#fff" }]}>{tabLabel(tool, tab)}</Text>
                </Pressable>
              );
            })}
          </View>

          {section === "summary" && tool.detail_text ? (
            <Text style={styles.textDim}>{tool.detail_text}</Text>
          ) : null}
          {section === "summary" && tool.error ? (
            <Text style={[styles.textDim, { color: colors.danger, marginTop: spacing.xs }]}>{tool.error}</Text>
          ) : null}

          {section === "diff"
            ? tool.diff_previews.map((preview) => (
                <View key={preview.path} style={{ marginTop: spacing.xs }}>
                  <Text style={[styles.mono, { color: colors.textDim }]}>{preview.path}</Text>
                  {preview.hunks.map((hunk, hi) => (
                    <View key={`${preview.path}:${hi}`} style={{ marginTop: spacing.xs }}>
                      {hunk.heading ? (
                        <Text style={[styles.mono, { color: colors.textDim }]}>{hunk.heading}</Text>
                      ) : null}
                      {hunk.lines.map((line, li) => (
                        <Text
                          key={`${preview.path}:${hi}:${li}`}
                          style={[
                            styles.mono,
                            {
                              color: line.kind === "Added" ? colors.success : line.kind === "Removed" ? colors.danger : colors.textDim,
                            },
                          ]}
                        >
                          {line.kind === "Added" ? "+ " : line.kind === "Removed" ? "- " : "  "}
                          {line.content}
                        </Text>
                      ))}
                    </View>
                  ))}
                </View>
              ))
            : null}

          {section === "logs"
            ? tool.logs.map((entry, idx) => (
                <View key={`log:${idx}`} style={{ marginTop: spacing.xs }}>
                  <Text style={[styles.text, { fontWeight: "600", fontSize: 12 }]}>{entry.title}</Text>
                  {entry.body ? <Text style={styles.mono}>{entry.body}</Text> : null}
                </View>
              ))
            : null}

          {section === "raw" ? (
            <View>
              {tool.raw_input ? (
                <View style={{ marginTop: spacing.xs }}>
                  <Text style={[styles.textDim, { fontSize: 12 }]}>input</Text>
                  <Text style={styles.mono}>{tool.raw_input}</Text>
                </View>
              ) : null}
              {tool.raw_output ? (
                <View style={{ marginTop: spacing.xs }}>
                  <Text style={[styles.textDim, { fontSize: 12 }]}>output</Text>
                  <Text style={styles.mono}>{tool.raw_output}</Text>
                </View>
              ) : null}
            </View>
          ) : null}

          {running && tool.can_stop && onStop ? (
            <Pressable
              style={[styles.buttonGhost, { marginTop: spacing.md, alignSelf: "flex-start" }]}
              onPress={() => onStop(tool.call_id)}
            >
              <Text style={[styles.text, { color: colors.danger }]}>Stop</Text>
            </Pressable>
          ) : null}
        </View>
      ) : null}
    </View>
  );
}

export const ToolCallCard = memo(ToolCallCardImpl);
// end of file
