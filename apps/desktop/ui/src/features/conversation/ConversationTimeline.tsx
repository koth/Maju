import { useRef, useEffect, useMemo, useState, Suspense, lazy, memo } from "react";
import type { ReactNode } from "react";
import type { MessageRole } from "../../types";
import type { UiSnapshot } from "../../types";
import { ToolCallCard } from "../tooling/ToolCallCard";
import {
  ensureStreamingMessageBody,
  subscribeStreamingMessage,
} from "./streaming-message-store";
import "./ConversationTimeline.css";

const MarkdownBody = lazy(() => import("./MarkdownBody"));
const INITIAL_TIMELINE_WINDOW = 80;
const TIMELINE_WINDOW_STEP = 80;

function scrollElementIntoView(element: HTMLElement | null) {
  if (typeof element?.scrollIntoView !== "function") return;
  element.scrollIntoView({ block: "end" });
}

interface Props {
  snapshot: UiSnapshot;
  onPermissionSelect: (requestId: string, optionId: string | null) => void;
  planPanel?: ReactNode;
}

interface MessageRowProps {
  id: string;
  role: MessageRole;
  body: string;
  streaming: boolean;
}

interface StreamingTextProps {
  id: string;
  body: string;
}

const StreamingText = memo(function StreamingText({ id, body }: StreamingTextProps) {
  const textRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    const currentBody = ensureStreamingMessageBody(id, body);
    if (textRef.current) {
      textRef.current.textContent = currentBody;
    }

    return subscribeStreamingMessage(id, (event) => {
      const node = textRef.current;
      if (!node) return;
      const scrollEl = node.closest(".timeline-scroll") as HTMLDivElement | null;
      const wasAtBottom = scrollEl
        ? scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight < 80
        : false;
      if (event.type === "replace") {
        node.textContent = event.text;
      } else {
        node.append(document.createTextNode(event.text));
      }
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

  return <pre ref={textRef} className="msg-streaming-text">{body}</pre>;
});

const MessageRow = memo(function MessageRow({ id, role, body, streaming }: MessageRowProps) {
  if (role === "User") {
    return (
      <div key={id} className="msg msg-user">
        <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
        <div className="msg-content msg-content-user">
          <Suspense fallback={<pre className="msg-fallback">{body}</pre>}>
            <MarkdownBody content={body} />
          </Suspense>
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
            <StreamingText id={id} body={body} />
          ) : (
            <Suspense fallback={<pre className="msg-fallback">{body}</pre>}>
              <MarkdownBody content={body} />
            </Suspense>
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

export function ConversationTimeline({ snapshot, onPermissionSelect, planPanel }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const bottomSentinelRef = useRef<HTMLDivElement>(null);
  const userScrolledUp = useRef(false);
  const visibleSessionId = useRef(snapshot.session.id);
  const [visibleCount, setVisibleCount] = useState(INITIAL_TIMELINE_WINDOW);

  const scrollToBottom = () => {
    scrollElementIntoView(bottomSentinelRef.current);
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    const handleScroll = () => {
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      userScrolledUp.current = !atBottom;
    };

    el.addEventListener("scroll", handleScroll);
    return () => el.removeEventListener("scroll", handleScroll);
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
    snapshot.agent_plan.length,
    snapshot.session_changes.length,
    snapshot.thinking_status,
  ]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      if (userScrolledUp.current) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(scrollToBottom);
    });
    observer.observe(el);
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
      <div className="timeline-items">
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

            return (
              <MessageRow
                key={msg.id}
                id={msg.id}
                role={msg.role}
                body={msg.body}
                streaming={isStreaming}
              />
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
        {planPanel}
        <div className="timeline-bottom-sentinel" ref={bottomSentinelRef} aria-hidden="true" />
      </div>
    </div>
  );
}
