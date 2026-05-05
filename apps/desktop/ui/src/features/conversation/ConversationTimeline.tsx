import { useRef, useEffect, Suspense, lazy } from "react";
import type { UiSnapshot } from "../../types";
import { ToolCallCard } from "../tooling/ToolCallCard";
import "./ConversationTimeline.css";

const MarkdownBody = lazy(() => import("./MarkdownBody"));

interface Props {
  snapshot: UiSnapshot;
  onPermissionSelect: (requestId: string, optionId: string | null) => void;
}

export function ConversationTimeline({ snapshot, onPermissionSelect }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const userScrolledUp = useRef(false);
  const prevLen = useRef(0);

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
    if (!userScrolledUp.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
    prevLen.current = snapshot.timeline.length;
  }, [snapshot.timeline.length, snapshot.thinking_status]);

  const isLastMessage = (index: number) =>
    index === snapshot.timeline.length - 1;

  return (
    <div className="timeline-scroll" ref={scrollRef}>
      <div className="timeline-items">
        {snapshot.timeline.map((item, i) => {
          if (typeof item === "string" && item === "Thinking") {
            return null;
          }

          if (typeof item === "object" && "Message" in item) {
            const msg = snapshot.messages.find((m) => m.id === item.Message);
            if (!msg) return null;

            if (msg.role === "User") {
              return (
                <div key={i} className="msg msg-user">
                  <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
                  <div className="msg-content msg-content-user">
                    <Suspense fallback={<pre className="msg-fallback">{msg.body}</pre>}>
                      <MarkdownBody content={msg.body} />
                    </Suspense>
                  </div>
                </div>
              );
            }

            if (msg.role === "Assistant") {
              const isStreaming =
                snapshot.session.status === "Streaming" && isLastMessage(i);
              return (
                <div key={i} className="msg msg-assistant">
                  <span className="msg-prefix msg-prefix-assistant">{"\u2022"} </span>
                  <div className="msg-content msg-content-assistant">
                    <Suspense fallback={<pre className="msg-fallback">{msg.body}</pre>}>
                      <MarkdownBody content={msg.body} />
                    </Suspense>
                    {isStreaming && <span className="streaming-cursor" />}
                  </div>
                </div>
              );
            }

            return (
              <div key={i} className="msg msg-system">
                <span className="msg-content msg-content-system">{msg.body}</span>
              </div>
            );
          }

          if (typeof item === "object" && "Tool" in item) {
            const tool = snapshot.tools.find((t) => t.id === item.Tool);
            if (!tool) return null;
            if (tool.call_id === "workspace.scan" && !tool.parent_call_id)
              return null;
            if (tool.parent_call_id) return null;

            return (
              <ToolCallCard
                key={i}
                tool={tool}
                snapshot={snapshot}
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
      </div>
    </div>
  );
}
