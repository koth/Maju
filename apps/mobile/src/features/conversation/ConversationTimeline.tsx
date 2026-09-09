import { memo, useCallback, useRef, useState } from "react";
import { View, Text, FlatList, Pressable, StyleSheet, Image, Modal } from "react-native";
import type { UiSnapshot, ChatMessage, ToolInvocation } from "../../types";
import { MarkdownBody } from "./MarkdownBody";
import { ToolCallCard } from "../tooling/ToolCallCard";
import { ThinkingIndicator } from "../ui/ThinkingIndicator";
import { EmptyState } from "../ui/EmptyState";
import { splitUserMessageBody, type UserMessageImage } from "./user-message-images";
import { styles, colors, spacing, radius } from "../theme";

interface Props {
  snapshot: UiSnapshot;
  onStopTool?: (toolCallId: string) => void;
}

type Row = { key: string; node: React.ReactNode };

const NEAR_BOTTOM_THRESHOLD = 80;

// Interleaves messages and tool calls chronologically along the timeline, the
// same shape the desktop ConversationTimeline renders. Message ids and tool
// ids are resolved against the snapshot's messages/tools arrays.
//
// The list is INVERTED with reversed data: the newest row anchors to offset 0,
// so incoming messages and streaming growth land on the pinned edge without a
// single scroll command. A chronological list with an explicit chase
// (onContentSizeChange → scrollToEnd) was tried and is measurably worse:
// every streaming patch re-measures markdown, each measurement fired another
// scroll teleport, and the screen visibly flickered for the whole turn.
// VirtualizedList cells are flow-laid-out in RN 0.81, so the inverted
// geometry has no stale-offset overlap failure mode either. Scrolling up to
// read history is stable too: new content inserts on the far side of the
// viewport, never shifting it.
function ConversationTimelineImpl({ snapshot, onStopTool }: Props) {
  const listRef = useRef<FlatList<Row> | null>(null);
  const [showJump, setShowJump] = useState(false);
  const messageById = new Map(snapshot.messages.map((message) => [message.id, message]));
  // Timeline items reference tools by `tool.id` (mirrors the desktop
  // ConversationTimeline, which indexes snapshot.tools by id). `call_id` is a
  // different identifier (used for permission requests / stop_tool), so keying
  // by it here made every tool card render as "(missing tool)". Keep a
  // call_id fallback for robustness against older relay payloads.
  const toolById = new Map<string, ToolInvocation>();
  for (const tool of snapshot.tools) {
    toolById.set(tool.id, tool);
    if (tool.call_id && !toolById.has(tool.call_id)) toolById.set(tool.call_id, tool);
  }

  // Stable row identity: message id / tool id instead of the timeline index.
  // Index keys shift whenever a patch re-splices the tail, remounting every
  // row after the splice point (markdown re-parse, height churn, scroll
  // jumps).
  const rows: Row[] = [];
  snapshot.timeline.forEach((item) => {
    if (item === "Thinking" || (typeof item === "object" && "Thinking" in item)) {
      // Timeline Thinking markers render as nothing — exactly like the
      // desktop. The reducer pushes one per reasoning segment and LEAVES it
      // in place once real content lands, and ThinkingActivity does not touch
      // session.status, so position/status heuristics miss every thinking
      // burst that starts while the status is still Idle (typically every
      // turn after the first). The live indicator is driven by
      // thinking_status instead — see below.
      return;
    }
    if ("Message" in item) {
      const message = messageById.get(item.Message);
      rows.push({
        key: `m:${item.Message}`,
        node: message ? (
          <MessageBubble message={message} />
        ) : (
          <Text style={styles.textDim}>(消息缺失)</Text>
        ),
      });
    } else {
      const tool = toolById.get(item.Tool);
      rows.push({
        key: `t:${item.Tool}`,
        node: tool ? (
          <ToolCallCard tool={tool} onStop={onStopTool} />
        ) : (
          <Text style={styles.textDim}>(工具缺失)</Text>
        ),
      });
    }
  });

  // Thinking indicator, aligned with the desktop: rendered after every
  // timeline item purely while `thinking_status === "Active"` (the reducer
  // clears it on TurnFinished and downgrades it to Completed as soon as
  // assistant text streams), so it reappears for every reasoning burst no
  // matter where the Thinking marker sits or what the session status is.
  if (snapshot.thinking_status === "Active") {
    rows.push({ key: "thinking-active", node: <ThinkingIndicator /> });
  }

  // Inverted list: data is reversed so the NEWEST row sits at index 0, which
  // the inverted scroll pins to offset 0. Incoming messages, streaming
  // growth, and ThinkingIndicator appear/disappear all land on that anchored
  // edge, so the viewport never moves — no scroll commands, no chase races,
  // no jitter. (A chronological list + scroll-command chase was tried here
  // and flickered on every streaming update — see the file header comment.)

  const handleScroll = useCallback((event: { nativeEvent: { contentOffset: { y: number }; contentSize: { height: number }; layoutMeasurement: { height: number } } }) => {
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    // Inverted: offset 0 is the newest message.
    const nearBottom = contentOffset.y < NEAR_BOTTOM_THRESHOLD;
    setShowJump(!nearBottom && contentSize.height > layoutMeasurement.height + 40);
  }, []);

  const jumpToLatest = useCallback(() => {
    setShowJump(false);
    requestAnimationFrame(() => {
      listRef.current?.scrollToOffset({ offset: 0, animated: true });
    });
  }, []);

  return (
    <View style={{ flex: 1 }}>
      <FlatList<Row>
        ref={listRef}
        style={{ flex: 1 }}
        contentContainerStyle={{ padding: spacing.md, paddingTop: spacing.xl }}
        data={rows.slice().reverse()}
        inverted
        keyExtractor={rowKey}
        renderItem={renderRow}
        ItemSeparatorComponent={RowSeparator}
        onScroll={handleScroll}
        scrollEventThrottle={120}
        initialNumToRender={24}
        maxToRenderPerBatch={16}
        windowSize={21}
        ListEmptyComponent={EmptyTimeline}
      />
      {showJump ? (
        <Pressable
          style={({ pressed }) => [timelineStyles.jumpPill, { opacity: pressed ? 0.85 : 1 }]}
          onPress={jumpToLatest}
          accessibilityRole="button"
          accessibilityLabel="跳到最新"
        >
          <Text style={timelineStyles.jumpText}>{"\u2193 最新"}</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

// --- Module-scope list pieces: FlatList remounts item/separator components
// whose identity changes every render, so they must be defined once here. ---

function rowKey(row: Row): string {
  return row.key;
}

function renderRow({ item }: { item: Row }) {
  return <View>{item.node}</View>;
}

function RowSeparator() {
  return <View style={timelineStyles.separator} />;
}

const EmptyTimeline = (
  <EmptyState glyph={"\u2728"} title="还没有消息" hint="在下方输入需求,让智能体开始工作。" />
);

// Message bubble: user messages align right in an accent-tinted bubble;
// assistant messages render boxless, full width on the timeline background
// (matching the desktop `.msg-assistant`, which has no border/fill). Steer
// messages are dimmed.
// Memoized: patch merges preserve object identity for untouched messages, so
// a snapshot emit re-renders only the rows that actually changed instead of
// re-parsing every mounted markdown body (which oscillated row heights and
// re-triggered the bottom chase on phones).
//
// Image attachments: user prompts embed images as `![alt](data:...;base64,...)`
// blocks. Rendering those through markdown shows the raw base64 on phones, so
// — mirroring the desktop — image-only blocks are pulled out into a thumbnail
// strip above the text bubble (square center-crop previews) and the remaining
// text renders through markdown.
const MessageBubble = memo(function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === "User";
  const isSteer = !!message.is_steer;

  let text = message.body;
  let images: UserMessageImage[] = [];
  if (isUser && !isSteer) {
    const split = splitUserMessageBody(message.body);
    text = split.text;
    images = split.images;
  } else if (isSteer) {
    text = splitUserMessageBody(message.body).text || message.body;
  }

  return (
    <View style={[timelineStyles.bubbleWrap, isUser ? { alignItems: "flex-end" } : { alignItems: "flex-start" }]}>
      {images.length > 0 ? <UserImageStrip images={images} /> : null}
      {text.trim().length > 0 ? (
        isUser ? (
          <View style={[timelineStyles.bubble, timelineStyles.bubbleUser, isSteer && { opacity: 0.6 }]}>
            <MarkdownBody body={text} />
          </View>
        ) : (
          // Assistant replies render boxless straight on the timeline
          // background — same as the desktop `.msg-assistant` (plain app-bg,
          // no border). A boxed surface per reply just stacks visual noise
          // against the tool cards and reads cluttered on a phone. No role
          // label either: user bubbles right-align in accent tint, so the
          // sides alone already tell who said what.
          <View style={[timelineStyles.assistantBody, isSteer && { opacity: 0.6 }]}>
            <MarkdownBody body={text} />
          </View>
        )
      ) : null}
    </View>
  );
});

// Thumbnail strip for a user message's attached images: square center-crop
// previews (resizeMode "cover" crops the longer edge to the middle — same
// presentation as the desktop `.msg-user-image` object-fit: cover). Tapping a
// thumbnail opens the full image (contain) in a dismissible overlay, matching
// the desktop preview dialog.
function UserImageStrip({ images }: { images: UserMessageImage[] }) {
  const [preview, setPreview] = useState<UserMessageImage | null>(null);
  const close = useCallback(() => setPreview(null), []);
  return (
    <>
      <View style={timelineStyles.imageStrip}>
        {images.map((image, index) => (
          <Pressable
            key={`${image.src.slice(-24)}-${index}`}
            onPress={() => setPreview(image)}
            accessibilityRole="imagebutton"
            accessibilityLabel={image.alt ? `预览 ${image.alt}` : "预览图片"}
          >
            <Image
              source={{ uri: image.src }}
              style={timelineStyles.imageThumb}
              resizeMode="cover"
            />
          </Pressable>
        ))}
      </View>
      <Modal visible={preview !== null} transparent animationType="fade" onRequestClose={close}>
        <Pressable style={timelineStyles.previewBackdrop} onPress={close} accessibilityLabel="关闭图片预览">
          {preview ? (
            <Image
              source={{ uri: preview.src }}
              style={timelineStyles.previewImage}
              resizeMode="contain"
            />
          ) : null}
        </Pressable>
      </Modal>
    </>
  );
}

const timelineStyles = StyleSheet.create({
  separator: { height: spacing.sm },
  bubbleWrap: { width: "100%" },
  bubble: { maxWidth: "88%", borderRadius: radius.lg, paddingHorizontal: spacing.md, paddingVertical: spacing.sm + 2, marginTop: spacing.xs },
  bubbleUser: { backgroundColor: colors.accentDim, borderBottomRightRadius: radius.sm, borderWidth: 1, borderColor: "rgba(91,140,255,0.25)" },
  // Assistant replies: NO box. Full-width plain container on the timeline bg —
  // the markdown palette (MarkdownBody) is already tuned for the app bg, and
  // tool cards above/below stay flush-aligned with the reply text.
  assistantBody: { alignSelf: "stretch", marginTop: spacing.xs },
  // Attached-image thumbnails: square center-crop previews (cover = the
  // shorter edge fills, the longer edge is cropped to its middle), mirroring
  // the desktop `.msg-user-image` object-fit: cover.
  imageStrip: { flexDirection: "row", flexWrap: "wrap", gap: spacing.sm, marginTop: spacing.xs },
  imageThumb: {
    width: 96,
    height: 96,
    borderRadius: radius.md,
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surfaceAlt,
  },
  previewBackdrop: {
    flex: 1,
    backgroundColor: "rgba(0,0,0,0.88)",
    alignItems: "center",
    justifyContent: "center",
    padding: spacing.lg,
  },
  previewImage: { width: "100%", height: "100%" },
  jumpPill: {
    position: "absolute",
    bottom: spacing.md,
    alignSelf: "center",
    backgroundColor: colors.surfaceRaised,
    borderRadius: radius.pill,
    paddingVertical: spacing.xs + 1,
    paddingHorizontal: spacing.md,
    borderWidth: 1,
    borderColor: colors.borderStrong,
    shadowColor: "#000",
    shadowOpacity: 0.4,
    shadowRadius: 10,
    shadowOffset: { width: 0, height: 4 },
    elevation: 5,
  },
  jumpText: { color: colors.accentBright, fontSize: 12, fontWeight: "700" },
});

export const ConversationTimeline = memo(ConversationTimelineImpl);
// end of file
