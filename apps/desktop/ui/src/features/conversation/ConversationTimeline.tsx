import { Fragment, useRef, useEffect, useMemo, useState, memo } from "react";
import type { FileChangeSummary, MessageRole } from "../../types";
import type { UiSnapshot } from "../../types";
import { ChangesBar } from "../changes/ChangesBar";
import { ToolCallCard } from "../tooling/ToolCallCard";
import MarkdownBody from "./MarkdownBody";
import {
  ensureStreamingMessageBody,
  subscribeStreamingMessage,
} from "./streaming-message-store";
import "./ConversationTimeline.css";

const INITIAL_TIMELINE_WINDOW = 80;
const TIMELINE_WINDOW_STEP = 80;

function scrollElementIntoView(element: HTMLElement | null) {
  if (typeof element?.scrollIntoView !== "function") return;
  element.scrollIntoView({ block: "end" });
}

interface Props {
  snapshot: UiSnapshot;
  onPermissionSelect: (requestId: string, optionId: string | null) => void;
  turnChangeSetsByMessageId?: Record<string, TimelineTurnChangeSet>;
  onReviewFileSelect?: (path: string, changeSetId: string) => void;
  onReviewChangeSetSelect?: (changeSetId: string) => void;
  hiddenPermissionRequestIds?: ReadonlySet<string>;
}

export interface TimelineTurnChangeSet {
  changeSetId: string;
  files: FileChangeSummary[];
  updatedAt: string;
}

interface MessageRowProps {
  id: string;
  role: MessageRole;
  body: string;
  streaming: boolean;
}

interface StreamingMarkdownProps {
  id: string;
  body: string;
}

interface UserMessageImage {
  alt: string;
  src: string;
}

const StreamingMarkdown = memo(function StreamingMarkdown({ id, body }: StreamingMarkdownProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [content, setContent] = useState(() => ensureStreamingMessageBody(id, body));

  useEffect(() => {
    const currentBody = ensureStreamingMessageBody(id, body);
    setContent(currentBody);

    return subscribeStreamingMessage(id, (event) => {
      const node = hostRef.current;
      if (!node) {
        setContent((previous) =>
          event.type === "replace" ? event.text : `${previous}${event.text}`,
        );
        return;
      }
      const scrollEl = node.closest(".timeline-scroll") as HTMLDivElement | null;
      const wasAtBottom = scrollEl
        ? scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight < 80
        : false;
      setContent((previous) =>
        event.type === "replace" ? event.text : `${previous}${event.text}`,
      );
      if (scrollEl && wasAtBottom) {
        requestAnimationFrame(() => {
          const sentinel = scrollEl.querySelector(".timeline-bottom-sentinel") as
            | HTMLDivElement
            | null;
          scrollElementIntoView(sentinel);
        });
      }
    });
  }, [id, body]);

  return (
    <div ref={hostRef} className="msg-streaming-markdown">
      <MarkdownBody content={content} />
    </div>
  );
});

const MessageRow = memo(function MessageRow({ id, role, body, streaming }: MessageRowProps) {
  if (role === "User") {
    const { text, images } = splitUserMessageBody(body);
    if (images.length > 0) {
      return (
        <div key={id} className="msg msg-user msg-user-stacked">
          <div className="msg-user-image-strip" aria-label="附加的图片">
            {images.map((image, index) => (
              <img
                key={`${image.src}-${index}`}
                className="msg-user-image"
                src={image.src}
                alt={image.alt || "附加的图片"}
              />
            ))}
          </div>
          {text.trim().length > 0 && (
            <div className="msg-user-bubble">
              <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
              <div className="msg-content msg-content-user">
                <MarkdownBody content={text} />
              </div>
            </div>
          )}
        </div>
      );
    }

    return (
      <div key={id} className="msg msg-user">
        <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
        <div className="msg-content msg-content-user">
          <MarkdownBody content={body} />
        </div>
      </div>
    );
  }

  if (role === "Assistant") {
    return (
      <div key={id} className="msg msg-assistant">
        <span className="msg-prefix msg-prefix-assistant">{"\u2022"} </span>
        <div className="msg-content msg-content-assistant">
          {streaming ? (
            <StreamingMarkdown id={id} body={body} />
          ) : (
            <MarkdownBody content={body} />
          )}
          {streaming && <span className="streaming-cursor" />}
        </div>
      </div>
    );
  }

  return (
    <div key={id} className="msg msg-system">
      <span className="msg-content msg-content-system">{body}</span>
    </div>
  );
});

function shouldRenderMessage(role: MessageRole, body: string) {
  return role === "User" || body.trim().length > 0;
}

function splitUserMessageBody(body: string): { text: string; images: UserMessageImage[] } {
  const blocks = body.split(/\n{2,}/);
  const textBlocks: string[] = [];
  const images: UserMessageImage[] = [];

  for (const block of blocks) {
    const image = parseImageOnlyBlock(block);
    if (image) {
      images.push(image);
    } else {
      textBlocks.push(block);
    }
  }

  return {
    text: textBlocks.join("\n\n").trim(),
    images,
  };
}

function parseImageOnlyBlock(block: string): UserMessageImage | null {
  const match = block.trim().match(
    /^!\[([^\]]*)\]\((data:image\/(?:png|jpeg|jpg|gif|webp);base64,[A-Za-z0-9+/=]+)\)$/i,
  );
  if (!match) return null;
  return {
    alt: match[1],
    src: match[2],
  };
}

export function ConversationTimeline({
  snapshot,
  onPermissionSelect,
  turnChangeSetsByMessageId = {},
  onReviewFileSelect,
  onReviewChangeSetSelect,
  hiddenPermissionRequestIds,
}: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  const bottomSentinelRef = useRef<HTMLDivElement>(null);
  const userScrolledUp = useRef(false);
  const manualScrollIntent = useRef(false);
  const visibleSessionId = useRef(snapshot.session.id);
  const [visibleCount, setVisibleCount] = useState(INITIAL_TIMELINE_WINDOW);

  const scrollToBottom = () => {
    scrollElementIntoView(bottomSentinelRef.current);
  };

  const turnChangesSignature = useMemo(
    () =>
      Object.entries(turnChangeSetsByMessageId)
        .map(([messageId, entry]) =>
          [
            messageId,
            entry.changeSetId,
            entry.files.length,
            entry.files
              .map((change) => `${change.path}:${change.added_lines}:${change.removed_lines}`)
              .join(","),
          ].join(":"),
        )
        .join("|"),
    [turnChangeSetsByMessageId],
  );

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    const markManualScrollIntent = () => {
      manualScrollIntent.current = true;
    };
    const markManualKeyboardScrollIntent = (event: KeyboardEvent) => {
      if (
        event.key === "ArrowUp" ||
        event.key === "ArrowDown" ||
        event.key === "PageUp" ||
        event.key === "PageDown" ||
        event.key === "Home" ||
        event.key === "End" ||
        event.key === " "
      ) {
        manualScrollIntent.current = true;
      }
    };
    const handleScroll = () => {
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      if (atBottom) {
        userScrolledUp.current = false;
        manualScrollIntent.current = false;
        return;
      }
      if (manualScrollIntent.current) {
        userScrolledUp.current = true;
      }
    };

    el.addEventListener("wheel", markManualScrollIntent, { passive: true });
    el.addEventListener("touchstart", markManualScrollIntent, { passive: true });
    el.addEventListener("pointerdown", markManualScrollIntent);
    el.addEventListener("keydown", markManualKeyboardScrollIntent);
    el.addEventListener("scroll", handleScroll);
    return () => {
      el.removeEventListener("wheel", markManualScrollIntent);
      el.removeEventListener("touchstart", markManualScrollIntent);
      el.removeEventListener("pointerdown", markManualScrollIntent);
      el.removeEventListener("keydown", markManualKeyboardScrollIntent);
      el.removeEventListener("scroll", handleScroll);
    };
  }, []);

  useEffect(() => {
    if (userScrolledUp.current) return;
    const frame = requestAnimationFrame(scrollToBottom);
    return () => cancelAnimationFrame(frame);
  }, [
    snapshot.revision,
    snapshot.timeline.length,
    snapshot.messages.length,
    snapshot.tools.length,
    turnChangesSignature,
    snapshot.thinking_status,
  ]);

  useEffect(() => {
    const items = itemsRef.current;
    if (!items || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      if (userScrolledUp.current) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(scrollToBottom);
    });
    observer.observe(items);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, []);

  useEffect(() => {
    visibleSessionId.current = snapshot.session.id;
    setVisibleCount(INITIAL_TIMELINE_WINDOW);
    userScrolledUp.current = false;
  }, [snapshot.session.id]);

  const effectiveVisibleCount =
    visibleSessionId.current === snapshot.session.id
      ? visibleCount
      : INITIAL_TIMELINE_WINDOW;
  const timelineStart = Math.max(0, snapshot.timeline.length - effectiveVisibleCount);
  const visibleTimeline = useMemo(
    () => snapshot.timeline.slice(timelineStart),
    [snapshot.timeline, timelineStart],
  );
  const hiddenCount = timelineStart;
  const turnIsActive =
    snapshot.session.status === "Streaming" ||
    snapshot.session.status === "WaitingForTool";
  const activeTurnStartIndex = useMemo(() => {
    if (!turnIsActive) return -1;
    const allMessagesById = new Map(snapshot.messages.map((message) => [message.id, message]));
    for (let index = snapshot.timeline.length - 1; index >= 0; index -= 1) {
      const item = snapshot.timeline[index];
      if (typeof item !== "object" || !("Message" in item)) continue;
      const message = allMessagesById.get(item.Message);
      if (message?.role === "User") return index;
    }
    return -1;
  }, [snapshot.messages, snapshot.timeline, turnIsActive]);

  const visibleMessageIds = useMemo(() => {
    const ids = new Set<string>();
    for (const item of visibleTimeline) {
      if (typeof item === "object" && "Message" in item) {
        ids.add(item.Message);
      }
    }
    return ids;
  }, [visibleTimeline]);

  const visibleToolIds = useMemo(() => {
    const ids = new Set<string>();
    for (const item of visibleTimeline) {
      if (typeof item === "object" && "Tool" in item) {
        ids.add(item.Tool);
      }
    }
    return ids;
  }, [visibleTimeline]);

  const messagesById = useMemo(() => {
    const map = new Map<string, UiSnapshot["messages"][number]>();
    if (visibleMessageIds.size === 0) return map;
    for (const message of snapshot.messages) {
      if (visibleMessageIds.has(message.id)) {
        map.set(message.id, message);
        if (map.size === visibleMessageIds.size) break;
      }
    }
    return map;
  }, [snapshot.messages, visibleMessageIds]);

  const { toolsById, childToolsByParent } = useMemo(() => {
    const toolsById = new Map<string, UiSnapshot["tools"][number]>();
    const visibleParentCallIds = new Set<string>();
    if (visibleToolIds.size === 0) {
      return { toolsById, childToolsByParent: new Map<string, UiSnapshot["tools"]>() };
    }

    for (const tool of snapshot.tools) {
      if (visibleToolIds.has(tool.id)) {
        toolsById.set(tool.id, tool);
        if (!tool.parent_call_id) {
          visibleParentCallIds.add(tool.call_id);
        }
        if (toolsById.size === visibleToolIds.size) break;
      }
    }

    const childToolsByParent = new Map<string, UiSnapshot["tools"]>();
    if (visibleParentCallIds.size > 0) {
      for (const tool of snapshot.tools) {
        const parentCallId = tool.parent_call_id;
        if (!parentCallId || !visibleParentCallIds.has(parentCallId)) continue;
        const children = childToolsByParent.get(parentCallId);
        if (children) {
          children.push(tool);
        } else {
          childToolsByParent.set(parentCallId, [tool]);
        }
      }
    }

    return { toolsById, childToolsByParent };
  }, [snapshot.tools, visibleToolIds]);

  const isLastMessage = (index: number) =>
    index === snapshot.timeline.length - 1;
  return (
    <div className="timeline-scroll" ref={scrollRef}>
      <div className="timeline-items" ref={itemsRef}>
        {hiddenCount > 0 && (
          <button
            className="timeline-load-older"
            type="button"
            onClick={() =>
              setVisibleCount((count) =>
                Math.min(snapshot.timeline.length, count + TIMELINE_WINDOW_STEP),
              )
            }
          >
            显示更早 {Math.min(hiddenCount, TIMELINE_WINDOW_STEP)} 条
          </button>
        )}
        {visibleTimeline.map((item, offset) => {
          const i = timelineStart + offset;
          if (typeof item === "string" && item === "Thinking") {
            return null;
          }

          if (typeof item === "object" && "Message" in item) {
            const msg = messagesById.get(item.Message);
            if (!msg) return null;
            const isStreaming =
              msg.role === "Assistant" &&
              snapshot.session.status === "Streaming" &&
              isLastMessage(i);
            const isCurrentTurnMessage =
              turnIsActive && (activeTurnStartIndex < 0 || i > activeTurnStartIndex);
            const changesForMessage =
              msg.role === "Assistant" && !isStreaming && !isCurrentTurnMessage
                ? turnChangeSetsByMessageId[msg.id]
                : undefined;
            const renderMessage = shouldRenderMessage(msg.role, msg.body);

            if (!renderMessage && !changesForMessage?.files.length) {
              return null;
            }

            return (
              <Fragment key={msg.id}>
                {renderMessage && (
                  <MessageRow
                    id={msg.id}
                    role={msg.role}
                    body={msg.body}
                    streaming={isStreaming}
                  />
                )}
                {changesForMessage && changesForMessage.files.length > 0 && (
                  <ChangesBar
                    changeSetId={changesForMessage.changeSetId}
                    changes={changesForMessage.files}
                    onFileSelect={onReviewFileSelect ?? (() => {})}
                    onReviewClick={onReviewChangeSetSelect}
                  />
                )}
              </Fragment>
            );
          }

          if (typeof item === "object" && "Tool" in item) {
            const tool = toolsById.get(item.Tool);
            if (!tool) return null;
            if (tool.call_id === "workspace.scan" && !tool.parent_call_id)
              return null;
            if (tool.parent_call_id) return null;

            return (
              <ToolCallCard
                key={tool.id}
                tool={tool}
                childToolsByParent={childToolsByParent}
                nested={false}
                onPermissionSelect={onPermissionSelect}
                hiddenPermissionRequestIds={hiddenPermissionRequestIds}
              />
            );
          }

          return null;
        })}
        {snapshot.thinking_status === "Active" && (
          <div className="thinking-indicator thinking-active">
            <span className="thinking-bullet">•</span>
            <span className="thinking-text">思考中</span>
          </div>
        )}
        <div className="timeline-bottom-sentinel" ref={bottomSentinelRef} aria-hidden="true" />
      </div>
    </div>
  );
}
