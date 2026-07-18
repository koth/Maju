import { Fragment, useRef, useEffect, useMemo, useState, memo } from "react";
import type { FormEvent } from "react";
import { createPortal } from "react-dom";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";
import type { FileChangeSummary, MessageRole } from "../../types";
import type { UiSnapshot } from "../../types";
import { ChangesBar } from "../changes/ChangesBar";
import { ToolCallCard } from "../tooling/ToolCallCard";
import MarkdownBody, { CopyTextButton, repairCompactMarkdown } from "./MarkdownBody";
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
  onPermissionSelect: (requestId: string, optionId: string | null, guidance?: string | null) => void;
  turnChangeSetsByMessageId?: Record<string, TimelineTurnChangeSet>;
  onReviewFileSelect?: (path: string, changeSetId: string) => void;
  onReviewChangeSetSelect?: (changeSetId: string) => void;
  hiddenPermissionRequestIds?: ReadonlySet<string>;
  onRetryUserMessage?: (messageId: string, text: string) => Promise<void> | void;
  onCancelTurn?: () => Promise<void> | void;
  onStopTool?: (toolCallId: string) => Promise<void> | void;
}

export interface TimelineTurnChangeSet {
  changeSetId: string;
  files: FileChangeSummary[];
  updatedAt: string;
  timelineIndex?: number;
}

interface MessageRowProps {
  id: string;
  role: MessageRole;
  body: string;
  streaming: boolean;
  isSteer?: boolean;
  retryable?: boolean;
  copyable?: boolean;
  onRetry?: (messageId: string, text: string) => Promise<void> | void;
}

interface StreamingMarkdownProps {
  id: string;
  body: string;
}

interface UserMessageImage {
  alt: string;
  src: string;
  previewSrc: string;
}

type ContextCompactionState = "pending" | "completed";
type TimelineItem = UiSnapshot["timeline"][number];
type TimelineMessage = UiSnapshot["messages"][number];
type TimelineTool = UiSnapshot["tools"][number];

interface TimelineCollapseCandidate {
  index: number;
  item: TimelineItem;
  kind: "assistant" | "tool";
  message?: TimelineMessage;
}

interface TimelineCollapseGroup {
  key: string;
  items: TimelineCollapseCandidate[];
  itemCount: number;
  durationLabel: string | null;
}

interface TimelineCollapseState {
  groupsBySummaryIndex: Map<number, TimelineCollapseGroup>;
  hiddenIndexes: Set<number>;
}

function contextCompactionState(body: string): ContextCompactionState | null {
  const normalized = body.trim();
  if (normalized === "正在压缩上下文") {
    return "pending";
  }
  if (normalized === "上下文已压缩" || normalized === "上下文已自动压缩") {
    return "completed";
  }
  return null;
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

function TimelineCollapseSummary({
  group,
  expanded,
  onToggle,
}: {
  group: TimelineCollapseGroup;
  expanded: boolean;
  onToggle: () => void;
}) {
  const collapsible = group.itemCount > 0;
  const content = (
    <>
      <span className="timeline-turn-summary-main">
        <span className="timeline-collapse-label">
          已处理{group.durationLabel ? ` ${group.durationLabel}` : ""}
        </span>
        {collapsible && (
          <span className="timeline-collapse-chevron" aria-hidden="true">
            {"\u203A"}
          </span>
        )}
      </span>
      <span className="timeline-turn-summary-rule" aria-hidden="true" />
    </>
  );

  if (!collapsible) {
    return (
      <div className="timeline-turn-summary is-completed">
        {content}
      </div>
    );
  }

  return (
    <button
      type="button"
      className={`timeline-turn-summary timeline-collapse-toggle is-completed ${expanded ? "is-expanded" : ""}`}
      aria-expanded={expanded}
      aria-label={expanded ? "收起已处理上下文" : "展开已处理上下文"}
      onClick={onToggle}
    >
      {content}
    </button>
  );
}

function TimelineActiveTurnSummary({ durationLabel }: { durationLabel: string | null }) {
  return (
    <div
      className="timeline-turn-summary is-active"
      role="status"
      aria-live="polite"
    >
      <span className="timeline-turn-summary-main">
        <span className="timeline-collapse-label">
          正在处理{durationLabel ? ` ${durationLabel}` : ""}
        </span>
      </span>
      <span className="timeline-turn-summary-rule" aria-hidden="true" />
    </div>
  );
}

const UserImageStrip = memo(function UserImageStrip({ images }: { images: UserMessageImage[] }) {
  const [previewImage, setPreviewImage] = useState<UserMessageImage | null>(null);

  useEffect(() => {
    if (!previewImage) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setPreviewImage(null);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [previewImage]);

  return (
    <>
      <div className="msg-user-image-strip" aria-label="附加的图片">
        {images.map((image, index) => {
          const label = image.alt || `图片 ${index + 1}`;
          return (
            <button
              key={`${image.src}-${image.previewSrc}-${index}`}
              type="button"
              className="msg-user-image-button"
              onClick={() => setPreviewImage(image)}
              aria-label={`预览 ${label}`}
              title="预览图片"
            >
              <img
                className="msg-user-image"
                src={image.src}
                alt={image.alt || "附加的图片"}
              />
            </button>
          );
        })}
      </div>
      {previewImage && createPortal(
        <div
          className="msg-image-preview-backdrop"
          role="presentation"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              setPreviewImage(null);
            }
          }}
        >
          <div
            className="msg-image-preview-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={previewImage.alt ? `图片预览：${previewImage.alt}` : "图片预览"}
          >
            <button
              type="button"
              className="msg-image-preview-close"
              onClick={() => setPreviewImage(null)}
              aria-label="关闭图片预览"
              title="关闭"
            >
              ×
            </button>
            <img
              className="msg-image-preview-original"
              src={previewImage.previewSrc}
              alt={previewImage.alt || "附加的图片"}
            />
          </div>
        </div>,
        document.body,
      )}
    </>
  );
});

const MessageRow = memo(function MessageRow({
  id,
  role,
  body,
  streaming,
  isSteer = false,
  retryable = false,
  copyable = false,
  onRetry,
}: MessageRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(body);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  useEffect(() => {
    if (!editing) setDraft(body);
  }, [body, editing]);

  const handleRetrySubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const nextText = draft.trim();
    if (!nextText || !onRetry) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      await onRetry(id, nextText);
      setEditing(false);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  };

  if (role === "User") {
    const { text, images } = splitUserMessageBody(body);
    const canRetry = retryable && images.length === 0 && !!onRetry;

    // Steers (追加指令) render as a compact annotation rather than a full
    // user-message bubble, visually distinguishing them from turn-starting
    // prompts while still showing the instruction text inline in the timeline.
    if (isSteer) {
      return (
        <div key={id} className="msg msg-steer" role="note" aria-label="追加指令">
          <span className="msg-steer-badge">追加指令</span>
          <span className="msg-steer-body">
            <UserMessageText text={text || body} />
          </span>
        </div>
      );
    }
    if (editing && canRetry) {
      const normalizedBody = normalizeUserMessageText(body).trim();
      const normalizedDraft = normalizeUserMessageText(draft).trim();
      return (
        <div key={id} className="msg msg-user msg-user-editing">
          <form className="msg-user-edit" onSubmit={handleRetrySubmit}>
            <textarea
              className="msg-user-edit-textarea"
              aria-label="编辑用户消息"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              disabled={submitting}
            />
            {submitError && <div className="msg-user-edit-error">{submitError}</div>}
            <div className="msg-user-edit-actions">
              <button
                type="button"
                className="msg-user-edit-cancel"
                onClick={() => {
                  setDraft(body);
                  setSubmitError(null);
                  setEditing(false);
                }}
                disabled={submitting}
              >
                取消
              </button>
              <button
                type="submit"
                className="msg-user-edit-submit"
                disabled={submitting || normalizedDraft.length === 0 || normalizedDraft === normalizedBody}
              >
                重新发送
              </button>
            </div>
          </form>
        </div>
      );
    }

    if (images.length > 0) {
      return (
        <div key={id} className="msg msg-user msg-user-stacked">
          <UserImageStrip images={images} />
          {text.trim().length > 0 && (
            <div className="msg-user-bubble">
              <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
              <div className="msg-content msg-content-user">
                <UserMessageText text={text} />
              </div>
            </div>
          )}
        </div>
      );
    }

    const messageBubble = (
      <div key={id} className="msg msg-user">
        <span className="msg-prefix msg-prefix-user">{"\u203A"} </span>
        <div className="msg-content msg-content-user">
          <UserMessageText text={body} />
        </div>
      </div>
    );

    if (!canRetry) return messageBubble;

    return (
      <div key={id} className="msg-user-wrap">
        {messageBubble}
        <div className="msg-user-actions">
          <button
            type="button"
            className="msg-user-edit-btn"
            onClick={() => {
              setDraft(body);
              setSubmitError(null);
              setEditing(true);
            }}
          >
            编辑并重发
          </button>
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
        {copyable && !streaming && (
          <div className="msg-assistant-actions">
            <CopyTextButton
              text={repairCompactMarkdown(body)}
              label="复制为 Markdown"
              copiedLabel="已复制"
              className="msg-copy-btn"
              copiedClassName="msg-copy-btn-copied"
            />
          </div>
        )}
      </div>
    );
  }

  const compactionState = role === "System" ? contextCompactionState(body) : null;
  if (compactionState) {
    return (
      <div
        key={id}
        className={`msg msg-system msg-context-compaction is-${compactionState}`}
        role={compactionState === "pending" ? "status" : undefined}
        aria-live={compactionState === "pending" ? "polite" : undefined}
      >
        <span className="msg-context-compaction-label">
          <span className="msg-context-compaction-icon" aria-hidden="true" />
          <span>{compactionState === "pending" ? "正在压缩上下文" : body.trim()}</span>
        </span>
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

function retryableUserMessageIds(snapshot: UiSnapshot) {
  const retryableIds = new Set<string>();
  if (snapshot.session.status === "Streaming" || snapshot.session.status === "WaitingForTool") {
    return retryableIds;
  }

  const messagesById = new Map(snapshot.messages.map((message) => [message.id, message]));
  for (const [index, item] of snapshot.timeline.entries()) {
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (message?.role !== "User") continue;

    let canRetry = true;
    for (const nextItem of snapshot.timeline.slice(index + 1)) {
      if (nextItem === "Thinking") continue;
      if (typeof nextItem === "object" && "Message" in nextItem) {
        const nextMessage = messagesById.get(nextItem.Message);
        if (nextMessage?.role === "System") continue;
      }
      canRetry = false;
      break;
    }
    if (canRetry) retryableIds.add(message.id);
  }
  return retryableIds;
}

function UserMessageText({ text }: { text: string }) {
  return <span className="msg-user-text">{normalizeUserMessageText(text)}</span>;
}

function normalizeUserMessageText(text: string) {
  return text.replace(/\r\n?/g, "\n");
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
    /^!\[([^\]]*)\]\((data:image\/(?:apng|avif|bmp|png|jpeg|jpg|gif|webp);base64,[A-Za-z0-9+/=]+|file:\/\/[^\s)]+)(?:\s+"(file:\/\/[^"]+)")?\)$/i,
  );
  if (!match) return null;
  return {
    alt: match[1],
    src: imageSrcForWebview(match[2]),
    previewSrc: imageSrcForWebview(match[3] ?? match[2]),
  };
}

function imageSrcForWebview(src: string): string {
  if (!src.startsWith("file://") || !isTauri()) {
    return src;
  }
  const path = fileUrlToPath(src);
  return path ? convertFileSrc(path) : src;
}

function fileUrlToPath(src: string): string | null {
  try {
    const url = new URL(src);
    if (url.protocol !== "file:") {
      return null;
    }
    const path = decodeURIComponent(url.pathname);
    if (/^\/[A-Za-z]:\//.test(path)) {
      return path.slice(1);
    }
    return path;
  } catch {
    return null;
  }
}

function visibleTurnChangeSetsByMessageId(
  timeline: UiSnapshot["timeline"],
  turnChangeSetsByMessageId: Record<string, TimelineTurnChangeSet>,
): Record<string, TimelineTurnChangeSet> {
  const result: Record<string, TimelineTurnChangeSet> = {};
  for (const item of timeline) {
    if (typeof item !== "object" || !("Message" in item)) continue;
    const changeSet = turnChangeSetsByMessageId[item.Message];
    if (changeSet?.files.length) {
      result[item.Message] = changeSet;
    }
  }
  return result;
}

function buildTimelineCollapseState({
  timeline,
  timelineStart,
  messagesById,
  toolsById,
  hiddenPermissionRequestIds,
  turnIsActive,
  activeTurnStartIndex,
  turnChangeSetsByMessageId,
}: {
  timeline: UiSnapshot["timeline"];
  timelineStart: number;
  messagesById: Map<string, TimelineMessage>;
  toolsById: Map<string, TimelineTool>;
  hiddenPermissionRequestIds?: ReadonlySet<string>;
  turnIsActive: boolean;
  activeTurnStartIndex: number;
  turnChangeSetsByMessageId: Record<string, TimelineTurnChangeSet>;
}): TimelineCollapseState {
  const groupsBySummaryIndex = new Map<number, TimelineCollapseGroup>();
  const hiddenIndexes = new Set<number>();
  let turnStartMessage: TimelineMessage | null = null;
  let turnItems: TimelineCollapseCandidate[] = [];

  const flushTurn = () => {
    if (!turnStartMessage) {
      turnItems = [];
      return;
    }

    const finalAssistant = [...turnItems]
      .reverse()
      .find((candidate) => candidate.kind === "assistant" && candidate.message);
    if (!finalAssistant?.message) {
      turnItems = [];
      turnStartMessage = null;
      return;
    }

    const itemsToCollapse = turnItems.filter((candidate) => {
      if (candidate.index === finalAssistant.index) return false;
      if (
        candidate.message &&
        turnChangeSetsByMessageId[candidate.message.id]?.files.length
      ) {
        return false;
      }
      return true;
    });

    const isCurrentTurn =
      turnIsActive &&
      (activeTurnStartIndex < 0 || finalAssistant.index > activeTurnStartIndex);
    if (isCurrentTurn) {
      turnItems = [];
      turnStartMessage = null;
      return;
    }

    const groupHiddenIndexes = new Set(itemsToCollapse.map((candidate) => candidate.index));
    for (const index of groupHiddenIndexes) {
      hiddenIndexes.add(index);
    }

    const key = `${turnStartMessage?.id ?? "turn"}:${finalAssistant.message.id}`;
    groupsBySummaryIndex.set(finalAssistant.index, {
      key,
      items: itemsToCollapse,
      itemCount: itemsToCollapse.length,
      durationLabel: elapsedLabelForTurn(turnStartMessage, finalAssistant.message, itemsToCollapse),
    });

    turnItems = [];
    turnStartMessage = null;
  };

  for (const [offset, item] of timeline.entries()) {
    const index = timelineStart + offset;

    if (typeof item === "object" && "Message" in item) {
      const message = messagesById.get(item.Message);
      if (!message) continue;

      if (message.role === "User") {
        // Steers (追加指令) are NOT turn boundaries: they don't end the
        // previous turn and don't start a new one. Skipping flushTurn() here
        // prevents the original query's tools + responses from being
        // prematurely collapsed when the steer enters the timeline.
        if (message.is_steer) {
          continue;
        }
        flushTurn();
        turnStartMessage = message;
        continue;
      }

      if (message.role === "Assistant" && shouldRenderMessage(message.role, message.body)) {
        turnItems.push({ index, item, kind: "assistant", message });
      }
      continue;
    }

    if (typeof item === "object" && "Tool" in item) {
      const tool = toolsById.get(item.Tool);
      if (tool && shouldRenderTimelineTool(tool, hiddenPermissionRequestIds)) {
        turnItems.push({ index, item, kind: "tool" });
      }
    }
  }

  flushTurn();
  return { groupsBySummaryIndex, hiddenIndexes };
}

function elapsedLabelForTurn(
  turnStartMessage: TimelineMessage | null,
  finalAssistant: TimelineMessage,
  collapsedItems: TimelineCollapseCandidate[],
) {
  const startMs =
    parseTimestampMs(turnStartMessage?.created_at) ??
    parseTimestampMs(collapsedItems.find((item) => item.message?.created_at)?.message?.created_at);
  const endMs = parseTimestampMs(finalAssistant.created_at);
  if (startMs == null || endMs == null || endMs < startMs) return null;
  return formatElapsedDuration(endMs - startMs);
}

function parseTimestampMs(value?: string) {
  if (!value) return null;
  const trimmed = value.trim();
  if (/^\d+$/.test(trimmed)) {
    const numeric = Number(trimmed);
    if (!Number.isFinite(numeric)) return null;
    return numeric >= 1_000_000_000_000 ? numeric : numeric * 1000;
  }
  const timestamp = Date.parse(trimmed);
  return Number.isFinite(timestamp) ? timestamp : null;
}

function formatElapsedDuration(ms: number) {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const seconds = totalSeconds % 60;
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor(totalSeconds / 60) % 60;

  if (hours > 0) {
    return `${hours}h ${minutes}m ${seconds}s`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
}

export function ConversationTimeline({
  snapshot,
  onPermissionSelect,
  turnChangeSetsByMessageId = {},
  onReviewFileSelect,
  onReviewChangeSetSelect,
  hiddenPermissionRequestIds,
  onRetryUserMessage,
  onCancelTurn,
  onStopTool,
}: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  const bottomSentinelRef = useRef<HTMLDivElement>(null);
  const userScrolledUp = useRef(false);
  const manualScrollIntent = useRef(false);
  const visibleSessionId = useRef(snapshot.session.id);
  const [visibleCount, setVisibleCount] = useState(INITIAL_TIMELINE_WINDOW);
  const [expandedCollapseGroups, setExpandedCollapseGroups] = useState<Set<string>>(
    () => new Set(),
  );
  const activeTurnFallbackStart = useRef<{ key: string; startedAtMs: number } | null>(null);
  const [durationNowMs, setDurationNowMs] = useState(() => Date.now());

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
    setExpandedCollapseGroups(new Set());
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
  const allMessagesById = useMemo(
    () => new Map(snapshot.messages.map((message) => [message.id, message])),
    [snapshot.messages],
  );
  const allToolsById = useMemo(
    () => new Map(snapshot.tools.map((tool) => [tool.id, tool])),
    [snapshot.tools],
  );
  const activeTurnStartIndex = useMemo(() => {
    if (!turnIsActive) return -1;
    for (let index = snapshot.timeline.length - 1; index >= 0; index -= 1) {
      const item = snapshot.timeline[index];
      if (typeof item !== "object" || !("Message" in item)) continue;
      const message = allMessagesById.get(item.Message);
      // Steers (追加指令) are NOT turn boundaries — skip them so the
      // active-turn summary (and its timer) stays anchored to the original
      // user message instead of jumping to the steer and resetting the
      // elapsed-time clock.
      if (message?.role === "User" && !message.is_steer) return index;
    }
    return -1;
  }, [allMessagesById, snapshot.timeline, turnIsActive]);
  const activeTurnStartMessage = useMemo(() => {
    if (!turnIsActive || activeTurnStartIndex < 0) return null;
    const item = snapshot.timeline[activeTurnStartIndex];
    if (typeof item !== "object" || !("Message" in item)) return null;
    return allMessagesById.get(item.Message) ?? null;
  }, [activeTurnStartIndex, allMessagesById, snapshot.timeline, turnIsActive]);
  const activeTurnKey = activeTurnStartMessage
    ? `${snapshot.session.id}:${activeTurnStartMessage.id}`
    : null;
  const activeTurnStartedAtMs = (() => {
    if (!activeTurnKey) return null;
    const explicitStart = parseTimestampMs(activeTurnStartMessage?.created_at);
    if (explicitStart != null) return explicitStart;
    if (activeTurnFallbackStart.current?.key !== activeTurnKey) {
      activeTurnFallbackStart.current = {
        key: activeTurnKey,
        startedAtMs: Date.now(),
      };
    }
    return activeTurnFallbackStart.current.startedAtMs;
  })();
  const activeTurnDurationLabel =
    turnIsActive && activeTurnStartedAtMs != null
      ? formatElapsedDuration(durationNowMs - activeTurnStartedAtMs)
      : null;

  useEffect(() => {
    if (!turnIsActive) return;
    setDurationNowMs(Date.now());
    const interval = window.setInterval(() => {
      setDurationNowMs(Date.now());
    }, 1000);
    return () => window.clearInterval(interval);
  }, [activeTurnKey, turnIsActive]);

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

  const displayTurnChangeSetsByMessageId = useMemo(
    () =>
      visibleTurnChangeSetsByMessageId(
        visibleTimeline,
        turnChangeSetsByMessageId,
      ),
    [turnChangeSetsByMessageId, visibleTimeline],
  );

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

  const collapseState = useMemo(
    () =>
      buildTimelineCollapseState({
        timeline: snapshot.timeline,
        timelineStart: 0,
        messagesById: allMessagesById,
        toolsById: allToolsById,
        hiddenPermissionRequestIds,
        turnIsActive,
        activeTurnStartIndex,
        turnChangeSetsByMessageId,
      }),
    [
      activeTurnStartIndex,
      allMessagesById,
      allToolsById,
      hiddenPermissionRequestIds,
      snapshot.timeline,
      turnIsActive,
      turnChangeSetsByMessageId,
    ],
  );
  const retryableMessages = useMemo(() => retryableUserMessageIds(snapshot), [snapshot]);

  const toggleCollapseGroup = (key: string) => {
    setExpandedCollapseGroups((previous) => {
      const next = new Set(previous);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  const isLastMessage = (index: number) =>
    index === snapshot.timeline.length - 1;

  const renderTimelineItem = (
    item: TimelineItem,
    i: number,
    {
      keyPrefix = "",
      renderChanges = true,
    }: { keyPrefix?: string; renderChanges?: boolean } = {},
  ) => {
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
        renderChanges && msg.role === "Assistant" && !isStreaming && !isCurrentTurnMessage
          ? displayTurnChangeSetsByMessageId[msg.id]
          : undefined;
      const renderMessage = shouldRenderMessage(msg.role, msg.body);

      if (!renderMessage && !changesForMessage?.files.length) {
        return null;
      }

      return (
        <Fragment key={`${keyPrefix}${msg.id}`}>
          {renderMessage && (
            <MessageRow
              id={msg.id}
              role={msg.role}
              body={msg.body}
              streaming={isStreaming}
              isSteer={msg.is_steer}
              retryable={retryableMessages.has(msg.id)}
              copyable={isLastMessage(i)}
              onRetry={onRetryUserMessage}
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
      if (!shouldRenderTimelineTool(tool, hiddenPermissionRequestIds)) return null;

      return (
        <ToolCallCard
          key={`${keyPrefix}${tool.id}`}
          tool={tool}
          childToolsByParent={childToolsByParent}
          nested={false}
          onPermissionSelect={onPermissionSelect}
          hiddenPermissionRequestIds={hiddenPermissionRequestIds}
          onCancelTurn={onCancelTurn}
          onStopTool={onStopTool}
        />
      );
    }

    return null;
  };

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
          if (collapseState.hiddenIndexes.has(i)) return null;

          const group = collapseState.groupsBySummaryIndex.get(i);
          if (!group) {
            const renderedItem = renderTimelineItem(item, i);
            if (i !== activeTurnStartIndex || !turnIsActive) return renderedItem;
            return (
              <Fragment key={`active-turn:${activeTurnKey ?? i}`}>
                {renderedItem}
                <TimelineActiveTurnSummary durationLabel={activeTurnDurationLabel} />
              </Fragment>
            );
          }

          const expanded = expandedCollapseGroups.has(group.key);
          const expandedBeforeItems = group.items.filter((candidate) => candidate.index < i);
          const expandedAfterItems = group.items.filter((candidate) => candidate.index > i);
          const renderExpandedItems = (items: TimelineCollapseCandidate[]) =>
            items.length > 0 ? (
              <div className="timeline-collapse-content">
                {items.map((candidate) =>
                  renderTimelineItem(candidate.item, candidate.index, {
                    keyPrefix: `collapsed:${group.key}:`,
                    renderChanges: false,
                  }),
                )}
              </div>
            ) : null;
          return (
            <Fragment key={`collapse:${group.key}`}>
              <TimelineCollapseSummary
                group={group}
                expanded={expanded}
                onToggle={() => toggleCollapseGroup(group.key)}
              />
              {expanded && renderExpandedItems(expandedBeforeItems)}
              {renderTimelineItem(item, i)}
              {expanded && renderExpandedItems(expandedAfterItems)}
            </Fragment>
          );
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

function shouldHidePermissionTool(
  tool: UiSnapshot["tools"][number],
  hiddenPermissionRequestIds?: ReadonlySet<string>,
) {
  return (
    hiddenPermissionRequestIds?.has(tool.call_id) ||
    (tool.kind === "permission" &&
      (tool.status !== "Running" || !!tool.permission_decision))
  );
}

function shouldRenderTimelineTool(
  tool: UiSnapshot["tools"][number],
  hiddenPermissionRequestIds?: ReadonlySet<string>,
) {
  if (shouldHidePermissionTool(tool, hiddenPermissionRequestIds)) return false;
  if (tool.call_id === "workspace.scan" && !tool.parent_call_id) return false;
  if (tool.parent_call_id) return false;
  return true;
}
