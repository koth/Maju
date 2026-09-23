import { Fragment, useRef, useEffect, useLayoutEffect, useMemo, useState, useCallback, memo } from "react";
import type { FormEvent } from "react";
import { createPortal } from "react-dom";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";
import { GitFork, Handshake } from "lucide-react";
import { resolveAgentKind } from "../session/AgentIcon";
import type { FileChangeSummary, MessageRole } from "../../types";
import type { ToolInvocation, AgentCliId, UiSnapshot } from "../../types";
import { ChangesBar } from "../changes/ChangesBar";
import { ToolCallCard } from "../tooling/ToolCallCard";
import { ToolActivityGroupRow } from "../tooling/ToolActivityGroup";
import { buildToolActivityGroups } from "../tooling/tool-activity";
import MarkdownBody, { CopyTextButton, repairCompactMarkdown } from "./MarkdownBody";
import { buildFilePathCandidatePool } from "./file-path-candidates";
import {
  ensureStreamingMessageBody,
  subscribeStreamingMessage,
} from "./streaming-message-store";
import "./ConversationTimeline.css";
import { ForkDialog } from "./ForkDialog";
import { HandoffDialog } from "./HandoffDialog";
import { buildHandoffDigest } from "./handoff";
import { useProxyRetry, proxyRetryReasonLabel } from "./useProxyRetry";
import { useNearViewportOnce } from "./useNearViewportOnce";

/** Workspace root for resolving relative file paths inside markdown messages.
 *  Set once per timeline render; module-level so the memoized streaming
 *  component can read it without prop drilling through the stream store. */
let visibleWorkspaceRoot: string | undefined;

/** Paths of files in the current git changeset — the strongest signal for
 *  resolving bare file names (`Composer.tsx:548`) mentioned in assistant
 *  messages, since the assistant almost always discusses files it changed. */
let visibleChangedFiles: string[] = [];

const INITIAL_TIMELINE_WINDOW = 80;
const TIMELINE_WINDOW_STEP = 80;
// 历史折叠按「轮次」计（存在轮次开头的会话）：默认只展示最近 3 轮对话，
// 更早的收进「更多对话」；每次点击多展开 6 轮，之后每点击一次翻倍
// （12、24 …）。没有轮次开头的退化转录（例如纯助手回复的旧版回放）没有
// 轮次边界可切，回退为按条数折叠（INITIAL_TIMELINE_WINDOW 条一档）。
const INITIAL_TURN_REVEAL_COUNT = 3;
const TURN_REVEAL_BATCH = 6;
/** Distance from the bottom that still counts as "following" the stream. */
const STICKY_BOTTOM_THRESHOLD_PX = 96;
/** Minimum interval between React commits of the streaming markdown body.
 *  Each commit re-parses the whole body (react-markdown + Prism + compact
 *  repair) — tens of ms for a long reply — and dsh streams chunks far faster
 *  than that. Capping the commit rate bounds the per-frame UI work no matter
 *  how fast the model streams; the trailing timer guarantees the final text
 *  always lands (and turn completion swaps in the authoritative snapshot
 *  body anyway). */
const STREAMING_RENDER_MIN_INTERVAL_MS = 160;
/** Tail window rendered for thinking text. The backend keeps its live buffer
 *  tail-capped too; this second cap bounds the expanded panel's text node so
 *  mounting/updating it can never re-lay-out a multi-megabyte string (the
 *  "expanding the thinking panel froze the UI" complaint on long sessions). */
const THINKING_BODY_TAIL_CHARS = 20_000;

function thinkingBodyTail(text: string): string {
  if (text.length <= THINKING_BODY_TAIL_CHARS) return text;
  return `……（前面已省略 ${text.length - THINKING_BODY_TAIL_CHARS} 字符）\n${text.slice(
    -THINKING_BODY_TAIL_CHARS,
  )}`;
}

/**
 * Timeline stick-to-bottom controller. Owned by `ConversationTimeline` and
 * read by streaming message renders so chunk flushes share the same sticky
 * decision instead of each guessing from a one-shot layout snapshot.
 */
let timelineScrollController: {
  isSticky: () => boolean;
  stickToBottom: () => void;
} | null = null;

function distanceFromBottom(element: HTMLElement): number {
  return element.scrollHeight - element.scrollTop - element.clientHeight;
}

function isNearBottom(
  element: HTMLElement,
  threshold = STICKY_BOTTOM_THRESHOLD_PX,
): boolean {
  return distanceFromBottom(element) <= threshold;
}

function forceScrollToBottom(element: HTMLElement | null) {
  if (!element) return;
  // Direct scrollTop is more reliable than scrollIntoView here: the sentinel
  // can land slightly above the true bottom while markdown/layout is settling.
  element.scrollTop = element.scrollHeight;
}

/** How a forked branch is hosted. `workspace` continues in the current
 *  workspace; `worktree` continues in a fresh git worktree (agent must
 *  support re-rooting — gated by `forkWorktreeSupported`). */
export type ConversationForkMode = "workspace" | "worktree";

export interface ConversationForkCapability {
  forkSupported: boolean;
  worktreeSupported: boolean;
}

/** Fork availability for a session's agent. `agentCli` may carry either the
 *  serde id (`"deepseek-harness"`) or the display label (`"DeepSeek Harness"`)
 *  depending on when the session row was written — normalize through the same
 *  matcher the sidebar agent icons use. dsh forks via the harness
 *  `session.fork` RPC; codex via ACP `session/fork`. Worktree forks need the
 *  agent to support re-rooting (harness session cwd is immutable), so only
 *  codex gets that option. */
export function conversationForkCapability(
  agentCli?: string | null,
): ConversationForkCapability {
  const kind = resolveAgentKind(agentCli ?? null);
  return {
    forkSupported: kind === "deepseek" || kind === "codex",
    worktreeSupported: kind === "codex",
  };
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
  onFilePathClick?: (filePath: string, lineNumber?: number) => void;
  /** Page older history from the backend when the local window is exhausted. */
  onLoadOlderHistory?: (limit?: number) => Promise<boolean>;
  /** Fork the conversation from a completed assistant message. Absent → the
   *  fork affordance is hidden entirely (agent backend without fork support). */
  onForkConversation?: (messageId: string, mode: ConversationForkMode) => Promise<void> | void;
  /** Whether the "new worktree" fork destination is supported by the active
   *  agent backend; only gates the menu item's enabled state. */
  forkWorktreeSupported?: boolean;
  /** Hand the conversation off to another agent: the timeline builds a local
   *  briefing from the window and this starts the session that receives it —
   *  on the agent the user picks in the dialog, with the briefing pre-loaded in
   *  its composer. Absent → the handoff affordance is hidden. */
  onHandoff?: (
    digest: string,
    agent: AgentCliId | null,
    preset: string | null,
  ) => Promise<void> | void;
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
  onRetry?: (messageId: string, text: string) => Promise<void> | void;
  onFilePathClick?: (filePath: string, lineNumber?: number) => void;
  /** Readonly pool-owned array: identity stability is what lets memoized
   *  MessageRows skip re-render (and markdown re-parse) on streaming deltas. */
  candidatePaths?: readonly string[];
  /** Copy/fork actions live only under the final reply of a COMPLETED turn —
   *  earlier replies render bare (the trailing icon rows were just noise), and
   *  the active turn's intermediate segments never grow a row: the anchor
   *  would jump segment to segment mid-turn. */
  showActions?: boolean;
  /** Open the fork point picker anchored on this message's turn. */
  onForkOpen?: (messageId: string) => void;
  /** Open the handoff briefing for the conversation through this message. */
  onHandoffOpen?: (messageId: string) => void;
}

interface StreamingMarkdownProps {
  id: string;
  body: string;
  onFilePathClick?: (filePath: string, lineNumber?: number) => void;
  changedFiles?: string[];
  candidatePaths?: readonly string[];
  onImagePreview?: (src: string, alt?: string) => void;
}

interface UserMessageImage {
  alt: string;
  src: string;
  previewSrc: string;
}

interface ImagePreviewState {
  alt: string;
  src: string;
}

type ContextCompactionState = "pending" | "completed" | "failed";
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
  toolCount: number;
  durationLabel: string | null;
  /** User message id that starts the turn; anchors the turn-nav ruler even
   *  when the turn is collapsed (its user row is not rendered). */
  userMessageId: string | null;
}

interface TimelineCollapseState {
  groupsBySummaryIndex: Map<number, TimelineCollapseGroup>;
  hiddenIndexes: Set<number>;
}

/// Which compaction-divider state a system notice renders as. Mirrors the
/// backend notice matchers (reducer `is_context_compaction_notice_body`) and
/// the dsh-bridge wording: the running notice may carry a `（compactionId）`
/// suffix, and the manual `/compact` outcome arrives as
/// "上下文压缩完成：{text}" / "上下文压缩失败…" — all lifecycle forms must
/// render as the divider, not as plain italic system rows.
function contextCompactionState(body: string): ContextCompactionState | null {
  const normalized = body.trim();
  if (normalized === "正在压缩上下文" || normalized.startsWith("正在压缩上下文（")) {
    return "pending";
  }
  if (
    normalized === "上下文已压缩" ||
    normalized === "上下文已自动压缩" ||
    normalized.startsWith("上下文压缩完成：")
  ) {
    return "completed";
  }
  if (
    normalized === "上下文压缩失败" ||
    normalized.startsWith("上下文压缩未完成：") ||
    normalized.startsWith("上下文压缩失败：")
  ) {
    return "failed";
  }
  return null;
}

/** Divider label for a settled notice. The success notice's
 *  "上下文压缩完成：" prefix is redundant inside the divider (the divider
 *  itself says the compaction finished), so only the harness's summary detail
 *  is shown; failure notices keep their full text. */
function contextCompactionDividerLabel(body: string, state: ContextCompactionState): string {
  const trimmed = body.trim();
  if (state === "completed" && trimmed.startsWith("上下文压缩完成：")) {
    const detail = trimmed.slice("上下文压缩完成：".length).trim();
    if (detail) return detail;
  }
  return trimmed;
}

const StreamingMarkdown = memo(function StreamingMarkdown({ id, body, onFilePathClick, changedFiles, candidatePaths, onImagePreview }: StreamingMarkdownProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [content, setContent] = useState(() => ensureStreamingMessageBody(id, body));
  const contentRef = useRef(content);
  const flushTimerRef = useRef<number | null>(null);
  const followRequestedRef = useRef(false);
  // Latest snapshot body, read by the mount/id effect below. The effect
  // intentionally does NOT depend on `body`: a streaming reply folds a new
  // body prop into the snapshot on every accepted patch, and re-running the
  // effect per fold tore down + recreated the store subscription and timer
  // each time. Store updates reach us through the subscription instead
  // (append events while streaming, a `replace` event when a full snapshot
  // re-aligns the store to the authoritative body).
  const bodyRef = useRef(body);
  bodyRef.current = body;

  useEffect(() => {
    // The stream store is the authoritative source while streaming (the
    // folded snapshot body is derived FROM it), so chunk events and body-prop
    // folds funnel into the same throttled commit: at most one markdown
    // re-parse per interval, always trailing, so the final text lands at most
    // one interval after the last chunk (and turn completion swaps in the
    // authoritative snapshot body anyway).
    contentRef.current = ensureStreamingMessageBody(id, bodyRef.current);

    const commit = () => {
      flushTimerRef.current = null;
      setContent(contentRef.current);
      if (followRequestedRef.current) {
        followRequestedRef.current = false;
        // Wait for React commit + layout so the new markdown height is
        // included before pinning to the bottom.
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            timelineScrollController?.stickToBottom();
          });
        });
      }
    };
    const scheduleCommit = () => {
      if (flushTimerRef.current != null) return;
      // Adaptive cadence: remark parse + reconciliation cost scales with the
      // body, so a long dsh reply widens the commit interval instead of
      // dropping frames. Short replies keep the snappy base interval.
      const length = contentRef.current.length;
      const interval =
        length > 64_000 ? 600 : length > 24_000 ? 320 : STREAMING_RENDER_MIN_INTERVAL_MS;
      flushTimerRef.current = window.setTimeout(commit, interval);
    };

    scheduleCommit();
    const unsubscribe = subscribeStreamingMessage(id, (event) => {
      if (timelineScrollController?.isSticky() ?? false) {
        followRequestedRef.current = true;
      }
      contentRef.current =
        event.type === "replace" ? event.text : `${contentRef.current}${event.text}`;
      scheduleCommit();
    });
    return () => {
      unsubscribe();
      if (flushTimerRef.current != null) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  return (
    <div ref={hostRef} className="msg-streaming-markdown">
      <MarkdownBody content={content} workspaceRoot={visibleWorkspaceRoot} onFilePathClick={onFilePathClick} changedFiles={changedFiles} candidatePaths={candidatePaths} onImagePreview={onImagePreview} />
    </div>
  );
});

/** Tail-capped body for the LIVE (in-progress) thinking segment. The parent
 *  re-renders on every streaming patch, so without the cap each chunk would
 *  re-lay-out the whole accumulated reasoning text. */
const ThinkingBodyTail = memo(function ThinkingBodyTail({ text }: { text: string }) {
  return <div className="thinking-body">{thinkingBodyTail(text)}</div>;
});

function TimelineCollapseSummary({
  group,
  expanded,
  onToggle,
  navUserId,
}: {
  group: TimelineCollapseGroup;
  expanded: boolean;
  onToggle: () => void;
  navUserId?: string | null;
}) {
  const collapsible = group.itemCount > 0;
  const content = (
    <>
      <span className="timeline-turn-summary-main">
        <span className="timeline-collapse-label">
          已处理
          {group.toolCount > 0 ? ` ${group.toolCount} 次工具调用` : ""}
          {group.durationLabel ? ` · ${group.durationLabel}` : ""}
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
      <div
        className="timeline-turn-summary is-completed"
        data-nav-user-id={navUserId ?? undefined}
      >
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
      data-nav-user-id={navUserId ?? undefined}
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
  const [previewImage, setPreviewImage] = useState<ImagePreviewState | null>(null);

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
              onClick={() => setPreviewImage({ alt: label, src: image.previewSrc })}
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
              src={previewImage.src}
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
  onRetry,
  onFilePathClick,
  candidatePaths,
  showActions = false,
  onForkOpen,
  onHandoffOpen,
}: MessageRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(body);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [previewImage, setPreviewImage] = useState<ImagePreviewState | null>(null);
  // Deferred markdown hydration: a long-history session can hold thousands of
  // mounted rows (nav jumps / paging expand the window to the whole timeline),
  // and eagerly parsing every assistant body froze the UI on each expansion.
  // Large bodies render a placeholder until the row nears the viewport.
  const markdownHostRef = useRef<HTMLDivElement | null>(null);
  const nearViewport = useNearViewportOnce(markdownHostRef, body.length > 1200);

  useEffect(() => {
    if (!editing) setDraft(body);
  }, [body, editing]);

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

  const handleImagePreview = (src: string, alt?: string) => {
    setPreviewImage({ alt: alt || "附加的图片", src });
  };

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
        <div key={id} className="msg msg-steer" role="note" aria-label="追加指令" data-nav-user-id={id}>
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
        <div key={id} className="msg msg-user msg-user-editing" data-nav-user-id={id}>
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
        <div key={id} className="msg msg-user msg-user-stacked" data-nav-user-id={id}>
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
      <div key={id} className="msg msg-user" data-nav-user-id={id}>
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
      <div key={id} className="msg msg-assistant" data-message-id={id}>
        <span className="msg-prefix msg-prefix-assistant">{"\u2022"} </span>
        <div className="msg-content msg-content-assistant" ref={markdownHostRef}>
          {streaming ? (
            <StreamingMarkdown id={id} body={body} onFilePathClick={onFilePathClick} changedFiles={visibleChangedFiles} candidatePaths={candidatePaths} onImagePreview={handleImagePreview} />
          ) : nearViewport ? (
            <MarkdownBody
              content={body}
              workspaceRoot={visibleWorkspaceRoot}
              onFilePathClick={onFilePathClick}
              changedFiles={visibleChangedFiles}
              candidatePaths={candidatePaths}
              onImagePreview={handleImagePreview}
            />
          ) : (
            // Not yet near the viewport: skip the react-markdown parse until
            // the row approaches (useNearViewportOnce flips this per row).
            <div className="msg-markdown-lazy" aria-hidden="true" />
          )}
          {streaming && <span className="streaming-cursor" />}
        </div>
        {!streaming && showActions && (
          <div className="msg-assistant-actions">
            <CopyTextButton
              text={repairCompactMarkdown(body)}
              label="复制回复文本"
              copiedLabel="已复制"
              className="msg-copy-btn"
              copiedClassName="msg-copy-btn-copied"
            />
            {onForkOpen && (
              <button
                type="button"
                className="msg-copy-btn msg-fork-btn"
                aria-label="分叉对话"
                title="分叉对话"
                aria-haspopup="dialog"
                onClick={() => onForkOpen(id)}
              >
                <GitFork size={14} strokeWidth={2.1} aria-hidden="true" />
              </button>
            )}
            {onHandoffOpen && (
              <button
                type="button"
                className="msg-copy-btn msg-handoff-btn"
                aria-label="交接给下一个智能体"
                title="交接给下一个智能体"
                aria-haspopup="dialog"
                onClick={() => onHandoffOpen(id)}
              >
                <Handshake size={14} strokeWidth={2.1} aria-hidden="true" />
              </button>
            )}
          </div>
        )}
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
                src={previewImage.src}
                alt={previewImage.alt || "附加的图片"}
              />
            </div>
          </div>,
          document.body,
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
          <span>
            {compactionState === "pending"
              ? "正在压缩上下文"
              : contextCompactionDividerLabel(body, compactionState)}
          </span>
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
    // A `/compact` command never receives a turn response — its outcome rides
    // system compaction notices, which this heuristic ignores, so the retry
    // affordance would render permanently and read as a failed send. Mirrors
    // the backend compact-slash interception (eq_ignore_ascii_case).
    if (message.body.trim().toLowerCase() === "/compact") continue;

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

/** 一轮对话的开头：非 steer、非 `/compact` 拦截的用户消息（与复制/分叉
 *  操作行、轮次导航的轮次判定保持一致）。 */
function isTurnOpeningMessage(message: TimelineMessage): boolean {
  return (
    message.role === "User" &&
    !message.is_steer &&
    message.body.trim().toLowerCase() !== "/compact"
  );
}

/** Minimum window size that keeps the last completed turn's user prompt and
 *  final assistant reply visible. A long codex session can end with hundreds
 *  of tool rows between two replies; a fixed 80-entry initial window then
 *  starts mid-tool-run, hides both boundary messages, and every visible tool
 *  collapses into the turn summary — leaving an apparently empty timeline.
 */
function minimumVisibleCountForLastTurn(
  timeline: UiSnapshot["timeline"],
  messages: UiSnapshot["messages"],
) {
  const messagesById = new Map(messages.map((message) => [message.id, message]));
  let finalAssistantIndex = -1;
  for (let index = timeline.length - 1; index >= 0; index -= 1) {
    const item = timeline[index];
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (message?.role === "Assistant" && message.body.trim().length > 0) {
      finalAssistantIndex = index;
      break;
    }
  }
  if (finalAssistantIndex < 0) return 0;
  for (let index = finalAssistantIndex - 1; index >= 0; index -= 1) {
    const item = timeline[index];
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (message?.role === "User" && !message.is_steer) {
      return timeline.length - index;
    }
  }
  return timeline.length - finalAssistantIndex;
}

function UserMessageText({ text }: { text: string }) {
  return <span className="msg-user-text">{normalizeUserMessageText(text)}</span>;
}

function normalizeUserMessageText(text: string) {
  return text.replace(/\r\n?/g, "\n");
}

/** 导航预览卡的首部摘要：压平空白后截取前 maxLength 个字符。 */
function excerptPreviewText(text: string, maxLength = 60) {
  const flattened = text.replace(/\s+/g, " ").trim();
  if (flattened.length <= maxLength) return flattened;
  return `${flattened.slice(0, maxLength)}…`;
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

    const lastItem = turnItems[turnItems.length - 1];
    const isCurrentTurn =
      turnIsActive &&
      (activeTurnStartIndex < 0 || (lastItem ? lastItem.index > activeTurnStartIndex : false));

    if (isCurrentTurn) {
      // The turn-level summary is a *finished-turn* affordance: it appears when
      // the turn ends and folds everything but the final reply. While the turn
      // is still running nothing is folded away here — the live view keeps every
      // assistant segment readable, and the only collapsing is the per-run
      // activity groups (`buildToolActivityGroups`), which turn contiguous tool
      // calls into one summary row each. Folding narration mid-turn hid the
      // agent's own progress notes behind a top bar that then moved around as
      // newer segments streamed in below it.
      turnItems = [];
      turnStartMessage = null;
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
      // Intermediate assistant segments (dsh progress narration) collapse
      // into the turn summary together with the tools: leaving them expanded
      // sandwiched the summary bar between blocks of assistant prose and it
      // appeared "in the middle of the conversation". They stay reachable by
      // expanding the summary.
      //
      // A row carrying a change set the final reply does NOT carry stays out of
      // the fold: that change set is drawn on the row it is anchored to (the
      // reply that produced the edits), not as the turn footer below.
      if (
        candidate.message &&
        turnChangeSetsByMessageId[candidate.message.id]?.files.length
      ) {
        return false;
      }
      return true;
    });

    const groupHiddenIndexes = new Set(itemsToCollapse.map((candidate) => candidate.index));
    for (const index of groupHiddenIndexes) {
      hiddenIndexes.add(index);
    }

    const key = `${turnStartMessage?.id ?? "turn"}:${finalAssistant.message.id}`;
    const toolCount = itemsToCollapse.filter((candidate) => candidate.kind === "tool").length;
    groupsBySummaryIndex.set(finalAssistant.index, {
      key,
      items: itemsToCollapse,
      itemCount: itemsToCollapse.length,
      toolCount,
      durationLabel: elapsedLabelForTurn(turnStartMessage, finalAssistant.message, itemsToCollapse),
      userMessageId: turnStartMessage?.id ?? null,
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
  onFilePathClick,
  onLoadOlderHistory,
  onForkConversation,
  forkWorktreeSupported = false,
  onHandoff,
}: Props) {
  // 分叉点选择器（从这里创建聊天分支）：记录触发分叉按钮的消息 id，
  // 打开时预选该消息所在轮次。
  const [forkPickerMessageId, setForkPickerMessageId] = useState<string | null>(null);
  // 交接（交给下一个智能体）：记录触发交接按钮的消息 id，摘要只覆盖该轮
  // 及之前的对话；摘要本身在打开时由 handoff.ts 本地生成。
  const [handoffMessageId, setHandoffMessageId] = useState<string | null>(null);
  // Remote workspaces store a synthetic ssh:// key in workspace.root. File
  // link resolution/open needs the real remote filesystem root instead.
  visibleWorkspaceRoot =
    snapshot.workspace.location?.kind === "remote_linux"
      ? snapshot.workspace.location.remote_path
      : snapshot.workspace.root;
  const scrollRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  const bottomSentinelRef = useRef<HTMLDivElement>(null);
  const userScrolledUp = useRef(false);
  const manualScrollIntent = useRef(false);
  // User's manual scroll position. WKWebView does not support
  // `overflow-anchor`, so any re-render that nudges layout lets WebKit's
  // scroll anchoring yank the viewport back to the anchor node — the
  // "scroll down a bit, snap back" loop. While the user is browsing
  // (manual mode), we capture the position before the commit and restore it
  // after the browser's layout/anchoring pass, so a background refresh can
  // never move the user.
  const manualScrollTopRef = useRef<number | null>(null);
  // Upstream retry status (502/429/transport) pushed by the codex_api_proxy
  // via the `proxy:retry` event. Rendered as a retry animation near the
  // timeline bottom while the proxy is backing off and resending.
  const proxyRetry = useProxyRetry();
  const stickCorrectionActive = useRef(false);
  /** 原生滚动条拖拽中（pointerdown 在滚动条上 → pointerup）。期间禁止吸底，
      否则 ResizeObserver/布局效应会把 thumb 拽回底部造成抖动。 */
  const scrollbarDragging = useRef(false);
  const visibleSessionId = useRef(snapshot.session.id);
  const [visibleCount, setVisibleCount] = useState(INITIAL_TIMELINE_WINDOW);
  // 按轮次折叠的展开深度：默认 3 轮，每次点击的增量从 6 轮起翻倍。
  const [turnReveal, setTurnReveal] = useState({
    turns: INITIAL_TURN_REVEAL_COUNT,
    batch: TURN_REVEAL_BATCH,
  });
  const [loadingOlder, setLoadingOlder] = useState(false);
  const mountedRef = useRef(true);
  const [expandedCollapseGroups, setExpandedCollapseGroups] = useState<Set<string>>(
    () => new Set(),
  );
  const [thinkingExpanded, setThinkingExpanded] = useState(false);
  const activeTurnFallbackStart = useRef<{ key: string; startedAtMs: number } | null>(null);
  const [durationNowMs, setDurationNowMs] = useState(() => Date.now());
  const [navHoverId, setNavHoverId] = useState<string | null>(null);
  const [navActiveId, setNavActiveId] = useState<string | null>(null);
  // 滚动时同步高亮当前轮次的回调，由下面的 effect 每次渲染更新，
  // handleScroll（闭包于挂载时）通过 ref 调用以拿到最新的导航数据。
  const syncNavActiveOnScrollRef = useRef<(() => void) | null>(null);

  const scrollToBottom = () => {
    const el = scrollRef.current;
    if (!el || userScrolledUp.current) return;
    stickCorrectionActive.current = true;
    forceScrollToBottom(el);
    // Markdown / right-panel layout can settle a frame or two later. Keep the
    // pin alive across those commits so Git/Files clicks don't leave us mid-list.
    requestAnimationFrame(() => {
      if (!userScrolledUp.current) forceScrollToBottom(el);
      requestAnimationFrame(() => {
        if (!userScrolledUp.current) forceScrollToBottom(el);
        stickCorrectionActive.current = false;
      });
    });
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

  // Stable identity for changed-file path list so right-panel-only re-renders
  // (Git/Files tab clicks) don't thrash every markdown row.
  const changedFilePathsSignature = (snapshot.repository?.changed_files ?? [])
    .map((file) => file.path)
    .join("|");
  const stableChangedFilePaths = useMemo(
    () => snapshot.repository?.changed_files?.map((file) => file.path) ?? [],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [changedFilePathsSignature],
  );
  visibleChangedFiles = stableChangedFilePaths;

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    timelineScrollController = {
      isSticky: () => !userScrolledUp.current,
      stickToBottom: scrollToBottom,
    };

    // Only real user "scroll up" gestures can unpin sticky follow. Programmatic
    // stick-to-bottom writes, layout reflow, and pointer clicks must not.
    // Unpin immediately on the gesture (not on the later scroll event) so a
    // concurrent parent re-render cannot snap us back mid-gesture.
    //
    // Inertia/smooth scrolling means the wheel event fires *before* the scroll
    // position actually leaves the near-bottom zone. A light flick may only
    // move a few px, so the very next `scroll` event can still be within the
    // sticky threshold and would wrongly re-pin (yanking the user back down).
    // Suppress re-pinning for a short window after any upward gesture.
    let lastUpwardIntentAt = 0;
    const unpinFromUser = () => {
      lastUpwardIntentAt = performance.now();
      manualScrollIntent.current = true;
      userScrolledUp.current = true;
    };
    const markWheelIntent = (event: WheelEvent) => {
      if (event.deltaY < 0) unpinFromUser();
    };
    let touchStartY: number | null = null;
    const markTouchStart = (event: TouchEvent) => {
      touchStartY = event.touches[0]?.clientY ?? null;
    };
    const markTouchMove = (event: TouchEvent) => {
      const currentY = event.touches[0]?.clientY;
      if (touchStartY == null || currentY == null) return;
      // Finger moving down => content scrolling up.
      if (currentY - touchStartY > 8) unpinFromUser();
    };
    const markKeyboardIntent = (event: KeyboardEvent) => {
      if (
        event.key === "ArrowUp" ||
        event.key === "PageUp" ||
        event.key === "Home" ||
        (event.key === " " && event.shiftKey)
      ) {
        unpinFromUser();
      }
    };
    const markScrollbarDragIntent = (event: PointerEvent) => {
      // Clicking the scrollbar track/thumb is a user scroll gesture. Content
      // clicks should not unpin follow mode.
      if (event.target === el) {
        unpinFromUser();
        scrollbarDragging.current = true;
      }
    };
    const clearScrollbarDrag = () => {
      if (!scrollbarDragging.current) return;
      scrollbarDragging.current = false;
      // 松手时以最终停留位置裁决：明确停在底部才恢复 follow；
      // 拖拽途中经过底部附近不算数，避免临界抖动导致的误吸。
      if (isNearBottom(el)) {
        userScrolledUp.current = false;
        manualScrollIntent.current = false;
      } else {
        userScrolledUp.current = true;
      }
    };
    const handleScroll = () => {
      // 滚动时同步左侧轮次导航的高亮到当前可视位置对应的轮次。
      syncNavActiveOnScrollRef.current?.();
      if (scrollbarDragging.current) {
        // 拖拽途中的位置变化全部忽略，由松手时统一裁决，
        // 避免 thumb 掠过底部时 userScrolledUp 被意外清零。
        return;
      }
      if (isNearBottom(el)) {
        // Within the upward-gesture grace window the user is scrolling up but
        // hasn't cleared the threshold yet — don't re-pin to the bottom.
        if (performance.now() - lastUpwardIntentAt < 400) {
          userScrolledUp.current = true;
          return;
        }
        if (turnIsActiveRef.current) {
          userScrolledUp.current = false;
          manualScrollIntent.current = false;
        } else {
          // Idle/restored sessions are free to browse: near the bottom is not
          // "following", it is just where the user stopped. Never re-pin.
          userScrolledUp.current = true;
        }
        return;
      }
      if (manualScrollIntent.current || userScrolledUp.current) {
        userScrolledUp.current = true;
        return;
      }
      // Still sticky, but layout/overflow-anchor/right-panel work nudged us off
      // the bottom. Snap back instead of leaving the transcript stranded.
      if (turnIsActiveRef.current && !stickCorrectionActive.current) {
        scrollToBottom();
      }
    };

    el.addEventListener("wheel", markWheelIntent, { passive: true });
    el.addEventListener("touchstart", markTouchStart, { passive: true });
    el.addEventListener("touchmove", markTouchMove, { passive: true });
    el.addEventListener("pointerdown", markScrollbarDragIntent);
    el.addEventListener("keydown", markKeyboardIntent);
    el.addEventListener("scroll", handleScroll, { passive: true });
    el.addEventListener("pointerup", clearScrollbarDrag);
    el.addEventListener("pointercancel", clearScrollbarDrag);
    el.addEventListener("lostpointercapture", clearScrollbarDrag);
    window.addEventListener("pointerup", clearScrollbarDrag);
    window.addEventListener("pointercancel", clearScrollbarDrag);
    return () => {
      el.removeEventListener("wheel", markWheelIntent);
      el.removeEventListener("touchstart", markTouchStart);
      el.removeEventListener("touchmove", markTouchMove);
      el.removeEventListener("pointerdown", markScrollbarDragIntent);
      el.removeEventListener("keydown", markKeyboardIntent);
      el.removeEventListener("scroll", handleScroll);
      el.removeEventListener("pointerup", clearScrollbarDrag);
      el.removeEventListener("pointercancel", clearScrollbarDrag);
      el.removeEventListener("lostpointercapture", clearScrollbarDrag);
      window.removeEventListener("pointerup", clearScrollbarDrag);
      window.removeEventListener("pointercancel", clearScrollbarDrag);
      if (timelineScrollController?.stickToBottom === scrollToBottom) {
        timelineScrollController = null;
      }
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

  // Parent workbench re-renders (right-panel Git/Files clicks, git status
  // refresh) can reflow the center column without bumping timeline revision.
  // While a turn is active and sticky, re-pin after commit/layout so we don't
  // stay stranded. Idle sessions must never re-pin — the user is browsing.
  useLayoutEffect(() => {
    if (!turnIsActive || userScrolledUp.current || scrollbarDragging.current) return;
    scrollToBottom();
  });

  // Neutralize WebKit's scroll anchoring for manual browsing: after the new
  // layout runs (and anchoring may have yanked the viewport back to the
  // anchor node), restore the exact position captured before the commit.
  // The local `target` is read before `el.scrollTop` forces the layout, so a
  // yank-induced scroll event cannot pollute the value we restore to.
  useLayoutEffect(() => {
    if (!userScrolledUp.current || scrollbarDragging.current) return;
    const target = manualScrollTopRef.current;
    if (target == null) return;
    const el = scrollRef.current;
    if (!el) return;
    if (Math.abs(el.scrollTop - target) > 1) {
      el.scrollTop = target;
    }
  });

  useEffect(() => {
    const scroller = scrollRef.current;
    const items = itemsRef.current;
    if (!scroller || !items || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    let timeout = 0;
    const scheduleStick = () => {
      if (userScrolledUp.current || scrollbarDragging.current) return;
      cancelAnimationFrame(frame);
      window.clearTimeout(timeout);
      // Wait for the workbench split/right-panel layout to settle. Clicking
      // Git/Files can reflow the center column after React commit.
      frame = requestAnimationFrame(() => {
        frame = requestAnimationFrame(scrollToBottom);
      });
      // One trailing pass for async right-panel mounts (FileTree/Git status).
      timeout = window.setTimeout(() => {
        if (!userScrolledUp.current) scrollToBottom();
      }, 50);
    };
    const observer = new ResizeObserver(scheduleStick);
    // Observe both:
    // - items: streaming markdown / tool cards growing
    // - scroller: center panel width/height changes when the right rail toggles
    observer.observe(items);
    observer.observe(scroller);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(timeout);
      observer.disconnect();
    };
  }, []);

  useEffect(() => {
    visibleSessionId.current = snapshot.session.id;
    setVisibleCount(INITIAL_TIMELINE_WINDOW);
    setTurnReveal({ turns: INITIAL_TURN_REVEAL_COUNT, batch: TURN_REVEAL_BATCH });
    setExpandedCollapseGroups(new Set());
    userScrolledUp.current = false;
    manualScrollIntent.current = false;
    stickCorrectionActive.current = false;
    lastUserMessageIdRef.current = null;
  }, [snapshot.session.id]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const allMessagesById = useMemo(
    () => new Map(snapshot.messages.map((message) => [message.id, message])),
    [snapshot.messages],
  );
  const allToolsById = useMemo(
    () => new Map(snapshot.tools.map((tool) => [tool.id, tool])),
    [snapshot.tools],
  );
  // 轮次开头的时间线下标。历史折叠以此按「轮次」切片——切片永不落在轮次
  // 中间（每轮开头的用户消息与其后的回复/工具始终一起显示或一起隐藏）。
  const turnStartIndexes = useMemo(() => {
    const starts: number[] = [];
    for (let index = 0; index < snapshot.timeline.length; index += 1) {
      const item = snapshot.timeline[index];
      if (typeof item !== "object" || !("Message" in item)) continue;
      const message = allMessagesById.get(item.Message);
      if (message && isTurnOpeningMessage(message)) starts.push(index);
    }
    return starts;
  }, [snapshot.timeline, allMessagesById]);
  const useTurnFolding = turnStartIndexes.length > 0;
  const hiddenTurnCount = useTurnFolding
    ? Math.max(0, turnStartIndexes.length - turnReveal.turns)
    : 0;
  // 顶部切口对齐：历史分页按「条」加载，页首可能落在轮次中间（该轮开头的
  // 用户消息还在更早的一页里）。只要还有未加载的历史，第一个轮次开头之前
  // 的条目就是这种切口——收起不显示，让可视边界始终对齐到轮次开头；等它的
  // 开头随下一页加载进来后自然归位。全部历史加载完毕（history_earliest_seq
  // 为空）时，这些条目是真正的会话开场，正常显示。
  const hideLeadingTurnCut =
    useTurnFolding &&
    turnStartIndexes[0] > 0 &&
    snapshot.history_earliest_seq != null;

  const effectiveVisibleCount = Math.max(
    visibleSessionId.current === snapshot.session.id
      ? visibleCount
      : INITIAL_TIMELINE_WINDOW,
    minimumVisibleCountForLastTurn(snapshot.timeline, snapshot.messages),
  );
  // 有轮次边界时按轮次折叠（默认最近 3 轮，「更多对话」逐级展开）；没有
  // 轮次边界的退化转录回退为按条数折叠。
  const timelineStart = useTurnFolding
    ? hiddenTurnCount > 0
      ? turnStartIndexes[hiddenTurnCount]
      : hideLeadingTurnCut
        ? turnStartIndexes[0]
        : 0
    : Math.max(0, snapshot.timeline.length - effectiveVisibleCount);
  const visibleTimeline = useMemo(
    () => snapshot.timeline.slice(timelineStart),
    [snapshot.timeline, timelineStart],
  );
  const hiddenCount = timelineStart;
  const turnIsActive =
    snapshot.session.status === "Streaming" ||
    snapshot.session.status === "WaitingForTool";
  // Fresh value for the once-registered scroll/observer handlers: sticky
  // follow must only apply while a turn is actually running, never on an
  // idle restored session (otherwise every re-render yanks the user back to
  // the bottom and they can never browse the history).
  const turnIsActiveRef = useRef(turnIsActive);
  turnIsActiveRef.current = turnIsActive;
  // Pre-commit capture for the manual scroll restore below: read while the
  // DOM is still the pre-render one so we hold the user's exact position
  // before any layout/anchoring pass runs against the new content. Re-read
  // every render so user scrolls between renders are picked up; the restore
  // effect below only overrides when the browser moved the viewport itself.
  if (userScrolledUp.current) {
    manualScrollTopRef.current = scrollRef.current?.scrollTop ?? null;
  }
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

  // 复制/分叉操作行锚定在**每轮对话的收尾回复**下方（每轮一个，而不是每条
  // 助手消息一个）：以轮次开头的用户消息（非 steer、非 /compact 拦截）为界，
  // 该轮内最后一条非空助手回复获得操作行。即便轮次以工具调用收尾，操作行
  // 仍锚在该轮最后的回复上。
  // 退化兜底：若时间线里完全没有轮次开头的用户消息（旧版分叉子会话在
  // user/message 回放修复之前重建，转录只有助手回复），则每条非空助手
  // 回复各自成为锚点，操作行保持可达。
  const turnFinalAssistantMessageIds = useMemo(() => {
    const anchors = new Set<string>();
    const assistantIds = new Set<string>();
    let sawTurnOpening = false;
    let currentAnchor: string | null = null;
    for (const item of snapshot.timeline) {
      if (typeof item !== "object" || !("Message" in item)) continue;
      const message = allMessagesById.get(item.Message);
      if (!message) continue;
      if (message.role === "User") {
        const isTurnOpening = isTurnOpeningMessage(message);
        if (isTurnOpening) {
          sawTurnOpening = true;
          if (currentAnchor) anchors.add(currentAnchor);
          currentAnchor = null;
        }
        continue;
      }
      if (message.role === "Assistant" && message.body.trim().length > 0) {
        assistantIds.add(message.id);
        currentAnchor = message.id;
      }
    }
    if (currentAnchor) anchors.add(currentAnchor);
    if (!sawTurnOpening) return assistantIds;
    return anchors;
  }, [snapshot.timeline, allMessagesById]);

  // 对话导航（左侧虚线刻度）：覆盖全部历史用户消息（不含追加指令），
  // 不限于当前可视窗口；超出窗口的轮次点击时先扩大窗口再跳转。
  // 预览文本截取用户消息首部与其后第一条助手回复的首部。
  // Nav excerpts are immutable once built (user message bodies never change,
  // and a turn's first non-empty reply does not either) — cache them by id so
  // the per-patch O(timeline) walk stays a pointer chase instead of re-running
  // body-splitting regexes over every historical user message on a long
  // session.
  const userNavExcerptCacheRef = useRef(
    new Map<string, { id: string; timelineIndex: number; userExcerpt: string; replyExcerpt: string }>(),
  );
  const userNavEntries = useMemo(() => {
    const cache = userNavExcerptCacheRef.current;
    const entries: {
      id: string;
      timelineIndex: number;
      userExcerpt: string;
      replyExcerpt: string;
    }[] = [];
    for (let i = 0; i < snapshot.timeline.length; i += 1) {
      const item = snapshot.timeline[i];
      if (typeof item !== "object" || !("Message" in item)) continue;
      const msg = allMessagesById.get(item.Message);
      if (!msg || msg.role !== "User" || msg.is_steer) continue;
      const cached = cache.get(msg.id);
      if (cached) {
        cached.timelineIndex = i;
        entries.push(cached);
        continue;
      }
      const { text } = splitUserMessageBody(msg.body);
      const entry = {
        id: msg.id,
        timelineIndex: i,
        userExcerpt: excerptPreviewText(text || msg.body),
        replyExcerpt: "",
      };
      cache.set(msg.id, entry);
      entries.push(entry);
    }
    for (let i = 0; i < entries.length; i += 1) {
      if (entries[i].replyExcerpt) continue;
      const searchEnd =
        i + 1 < entries.length ? entries[i + 1].timelineIndex : snapshot.timeline.length;
      for (let j = entries[i].timelineIndex + 1; j < searchEnd; j += 1) {
        const item = snapshot.timeline[j];
        if (typeof item !== "object" || !("Message" in item)) continue;
        const candidate = allMessagesById.get(item.Message);
        if (candidate?.role === "Assistant" && candidate.body.trim().length > 0) {
          entries[i].replyExcerpt = excerptPreviewText(candidate.body);
          break;
        }
      }
    }
    return entries;
  }, [snapshot.timeline, allMessagesById]);

  useEffect(() => {
    if (userNavEntries.length === 0) return;
    if (userNavEntries.some((entry) => entry.id === navActiveId)) return;
    setNavActiveId(userNavEntries[userNavEntries.length - 1].id);
  }, [userNavEntries, navActiveId]);

  // 用户提交新 prompt 后（timeline 末尾新增一条 User 消息，含 steer 追加指令），
  // 重新接管吸底并滚动到新消息处——即使用户之前向上浏览过历史
  // （userScrolledUp = true）。普通 revision 刷新 / 后台轮询不会触发，
  // 因为最后一条 User 消息 id 未变化。
  const lastUserMessageIdRef = useRef<string | null>(null);
  useEffect(() => {
    let lastUserId: string | null = null;
    for (let i = snapshot.timeline.length - 1; i >= 0; i -= 1) {
      const item = snapshot.timeline[i];
      if (typeof item !== "object" || !("Message" in item)) continue;
      if (allMessagesById.get(item.Message)?.role === "User") {
        lastUserId = item.Message;
        break;
      }
    }
    const prev = lastUserMessageIdRef.current;
    lastUserMessageIdRef.current = lastUserId ?? prev;
    if (prev === null || lastUserId === null || lastUserId === prev) return;
    // 刚提交的消息需要立刻可见：解除手动浏览状态并钉回底部。
    userScrolledUp.current = false;
    manualScrollIntent.current = false;
    manualScrollTopRef.current = null;
    scrollToBottom();
  }, [snapshot.timeline, allMessagesById]);

  const navHoverEntry = navHoverId
    ? userNavEntries.find((entry) => entry.id === navHoverId) ?? null
    : null;

  // 滚动时把导航高亮同步到当前可视位置对应的轮次：
  // 取最后一个锚点不高于可视区中线的用户消息刻度；若所有刻度都在中线之下
  // （还在第一段之前的顶部），取第一个；都在中线之上（滚到底）取最后一个。
  useEffect(() => {
    syncNavActiveOnScrollRef.current = () => {
      const scroller = scrollRef.current;
      if (!scroller || userNavEntries.length === 0) return;
      const midY = scroller.getBoundingClientRect().top + scroller.clientHeight * 0.4;
      // One querySelectorAll instead of one querySelector per entry: a
      // long-history session has hundreds of nav entries but only a handful
      // of rendered anchors, and per-entry DOM queries on every scroll event
      // made expanding content above the fold jank.
      const anchorById = new Map<string, HTMLElement>();
      for (const node of scroller.querySelectorAll<HTMLElement>("[data-nav-user-id]")) {
        const id = node.dataset.navUserId;
        if (id) anchorById.set(id, node);
      }
      let currentId: string | null = null;
      for (const entry of userNavEntries) {
        const node = anchorById.get(entry.id);
        if (!node) continue; // 该轮次尚未渲染（在可视窗口外），跳过
        if (node.getBoundingClientRect().top <= midY) {
          currentId = entry.id;
        } else {
          break;
        }
      }
      // 顶部没有任何渲染锚点在中线之上时，高亮第一个已渲染轮次。
      if (currentId === null) {
        const firstVisible = userNavEntries.find((entry) => anchorById.has(entry.id));
        currentId = firstVisible?.id ?? userNavEntries[userNavEntries.length - 1].id;
      }
      setNavActiveId((prev) => (prev === currentId ? prev : currentId));
    };
  });

  // 导航数据或可视渲染范围变化后（扩窗跳转、流式新增轮次）同步一次高亮。
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      syncNavActiveOnScrollRef.current?.();
    });
    return () => cancelAnimationFrame(frame);
  }, [userNavEntries, visibleCount, hiddenTurnCount]);

  // 「更多对话」：按轮次展开——每次多展示一批轮次（6 轮起），之后每点击
  // 一次该批翻倍（12、24 …）。
  const revealMoreTurns = useCallback(() => {
    setTurnReveal(({ turns, batch }) => ({ turns: turns + batch, batch: batch * 2 }));
  }, []);

  const handleUserNavJump = useCallback((messageId: string) => {
    const scroller = scrollRef.current;
    if (!scroller) return;
    // 视为一次手动滚动：取消吸底，让跳转后的位置稳定停留。
    userScrolledUp.current = true;
    manualScrollIntent.current = true;
    const scrollToTarget = () => {
      const target = scroller.querySelector<HTMLElement>(
        `[data-nav-user-id="${messageId}"]`,
      );
      if (!target) return;
      scroller.scrollTop = Math.max(0, target.offsetTop - scroller.clientHeight * 0.2);
    };
    const target = scroller.querySelector<HTMLElement>(
      `[data-nav-user-id="${messageId}"]`,
    );
    if (target) {
      scrollToTarget();
    } else {
      // 目标在当前可视窗口之外：扩窗渲染后等 React 提交 + 布局再跳。
      if (useTurnFolding) {
        setTurnReveal((prev) => ({ ...prev, turns: turnStartIndexes.length }));
      } else {
        setVisibleCount(snapshot.timeline.length);
      }
      requestAnimationFrame(() => {
        requestAnimationFrame(scrollToTarget);
      });
    }
    setNavActiveId(messageId);
  }, [snapshot.timeline.length, turnStartIndexes, useTurnFolding]);

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

  // Message meta (id/role/is_steer) with a structural identity: roles are
  // immutable per id, so this map survives streaming body growth. Keying the
  // file-path pool on it (instead of the per-delta `allMessagesById`) is what
  // keeps the pool from being rebuilt — a regex sweep over EVERY historical
  // tool's raw input/output — on each streamed chunk of a long dsh session.
  const messageMetaSignature = `${snapshot.session.id}:${snapshot.messages.length}:${snapshot.messages[0]?.id ?? ""}`;
  const messageMetaById = useMemo(
    () =>
      new Map(
        snapshot.messages.map((message) => [
          message.id,
          { id: message.id, role: message.role, is_steer: message.is_steer },
        ]),
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [messageMetaSignature],
  );

  // Per-turn pool of file paths harvested from shell tool inputs/outputs and
  // turn file changes; MarkdownBody matches message file references against
  // this pool instead of searching the whole repository. Rebuilt only when
  // the timeline/tool structure changes — never per streaming text delta
  // (per-tool scans are additionally cached by object identity, see
  // file-path-candidates.ts).
  const filePathCandidatePool = useMemo(
    () =>
      buildFilePathCandidatePool(
        snapshot.timeline,
        messageMetaById,
        allToolsById,
        turnChangeSetsByMessageId,
      ),
    [snapshot.timeline, messageMetaById, allToolsById, turnChangeSetsByMessageId],
  );

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

  // Codex-style activity runs: contiguous groups of ordinary tool calls render
  // as one collapsed summary that expands on click, instead of stacking every
  // row. Derived from the timeline/tool structure only — never per streaming
  // delta (the tool objects are identity-stable between deltas).
  const toolActivity = useMemo(
    () =>
      buildToolActivityGroups(snapshot.timeline, allToolsById, (tool) =>
        shouldRenderTimelineTool(tool, hiddenPermissionRequestIds),
      ),
    [snapshot.timeline, allToolsById, hiddenPermissionRequestIds],
  );

  // Stable identity: an inline arrow here would break MessageRow memoization
  // for the anchor rows on every streaming delta (each break costs a full
  // markdown re-parse of that message).
  const openForkPicker = useCallback((messageId: string) => {
    setForkPickerMessageId(messageId);
  }, []);

  // Stable identity for the same reason as `openForkPicker`: MessageRow is
  // memoized, so an inline arrow would re-render every row per streaming delta.
  const openHandoff = useCallback((messageId: string) => {
    setHandoffMessageId(messageId);
  }, []);

  // The briefing is derived from the window the user is looking at, through the
  // turn they clicked; recomputed only when that changes.
  const handoffDigest = useMemo(
    () => (handoffMessageId ? buildHandoffDigest(snapshot, handoffMessageId) : ""),
    [snapshot, handoffMessageId],
  );

  // One row renderer for every tool, shared by the standalone rows and the
  // expanded members of an activity group. Stable identity keeps the group's
  // memo intact across streaming deltas.
  const renderToolRow = useCallback(
    (member: ToolInvocation) => (
      <ToolCallCard
        tool={member}
        childToolsByParent={childToolsByParent}
        nested={false}
        onPermissionSelect={onPermissionSelect}
        hiddenPermissionRequestIds={hiddenPermissionRequestIds}
        onCancelTurn={onCancelTurn}
        onStopTool={onStopTool}
      />
    ),
    [
      childToolsByParent,
      onPermissionSelect,
      hiddenPermissionRequestIds,
      onCancelTurn,
      onStopTool,
    ],
  );

  const isLastMessage = (index: number) =>
    index === snapshot.timeline.length - 1;

  /**
   * The turn change set a timeline row carries, if it may be shown yet.
   *
   * Single source of truth for both render paths: the plain row, and the
   * collapsed turn block (where the bar is drawn as the block's footer instead
   * of under the anchor row). Guards: a streaming tail has no settled change
   * set, and the live turn keeps its changes in the changes panel until the
   * turn is anchored to a finished reply.
   */
  const turnChangeSetForRow = (
    item: TimelineItem,
    i: number,
  ): TimelineTurnChangeSet | undefined => {
    if (typeof item !== "object" || !("Message" in item)) return undefined;
    const msg = messagesById.get(item.Message);
    if (!msg || msg.role !== "Assistant") return undefined;
    const isStreaming =
      snapshot.session.status === "Streaming" && isLastMessage(i);
    const isCurrentTurnMessage =
      turnIsActive && (activeTurnStartIndex < 0 || i > activeTurnStartIndex);
    if (isStreaming || isCurrentTurnMessage) return undefined;
    const changeSet = displayTurnChangeSetsByMessageId[msg.id];
    return changeSet?.files.length ? changeSet : undefined;
  };

  const renderTurnChangesBar = (changeSet: TimelineTurnChangeSet | undefined) =>
    changeSet && changeSet.files.length > 0 ? (
      <ChangesBar
        changeSetId={changeSet.changeSetId}
        changes={changeSet.files}
        onFileSelect={onReviewFileSelect ?? (() => {})}
        onReviewClick={onReviewChangeSetSelect}
      />
    ) : null;

  const renderTimelineItem = (
    item: TimelineItem,
    i: number,
    {
      keyPrefix = "",
      renderChanges = true,
    }: { keyPrefix?: string; renderChanges?: boolean } = {},
  ) => {
    if (item === "Thinking" || (typeof item === "object" && "Thinking" in item)) {
      // Historical thinking segments are live-only: once a reasoning segment
      // ends, its conclusions are in the reply that follows — keeping a
      // collapsed block per turn just clutters the timeline. The reducer
      // still folds the (tail-capped) text into the segment item; it simply
      // is not rendered.
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
      const changesForMessage = renderChanges
        ? turnChangeSetForRow(item, i)
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
              onRetry={onRetryUserMessage}
              onFilePathClick={onFilePathClick}
              candidatePaths={
                msg.role === "Assistant"
                  ? filePathCandidatePool.byMessageId.get(msg.id) ?? filePathCandidatePool.all
                  : undefined
              }
              showActions={
                msg.role === "Assistant" &&
                turnFinalAssistantMessageIds.has(msg.id) &&
                // 轮次进行中整行隐藏（copy/fork 都不出现在中间阶段）：锚点会
                // 随着中间文本段推进不断跳动，工具运行间隙还会在上一段文本
                // 下冒出来 —— 操作行只属于已完成轮次的收尾回复。
                !(turnIsActive && isCurrentTurnMessage)
              }
              // 分叉只对已完成轮次开放，由后端再兜底校验一次。点击打开分叉点选择器。
              onForkOpen={
                onForkConversation && !(turnIsActive && msg.role === "Assistant" && isCurrentTurnMessage)
                  ? openForkPicker
                  : undefined
              }
              // 交接同样只在已完成轮次上出现：摘要覆盖到这一轮为止。
              onHandoffOpen={
                onHandoff && !(turnIsActive && msg.role === "Assistant" && isCurrentTurnMessage)
                  ? openHandoff
                  : undefined
              }
            />
          )}
          {renderTurnChangesBar(changesForMessage)}
        </Fragment>
      );
    }

    if (typeof item === "object" && "Tool" in item) {
      const tool = toolsById.get(item.Tool);
      if (!tool) return null;
      if (!shouldRenderTimelineTool(tool, hiddenPermissionRequestIds)) return null;

      // A run of ordinary calls collapses into one summary row (Codex-style);
      // its first index renders the group and the rest render nothing.
      const activityGroup = toolActivity.byStartIndex.get(i);
      if (activityGroup) {
        return (
          <ToolActivityGroupRow
            key={`${keyPrefix}activity:${activityGroup.startIndex}`}
            group={activityGroup}
            renderTool={renderToolRow}
          />
        );
      }
      if (toolActivity.memberIndexes.has(i)) return null;

      return <Fragment key={`${keyPrefix}${tool.id}`}>{renderToolRow(tool)}</Fragment>;
    }

    return null;
  };

  return (
    <div className="timeline-host">
      <div className="timeline-scroll" ref={scrollRef}>
        <div className="timeline-items" ref={itemsRef}>
        {useTurnFolding ? (
          hiddenTurnCount > 0 && (
            <button
              className="timeline-load-older"
              type="button"
              onClick={revealMoreTurns}
            >
              更多对话（还有 {hiddenTurnCount} 轮）
            </button>
          )
        ) : (
          hiddenCount > 0 && (
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
          )
        )}
        {/* 本地全部展开后才允许向后端翻页（顶部切口不算"未展开"，
            它只能靠继续翻页让该轮开头加载进来后归位）。 */}
        {(useTurnFolding ? hiddenTurnCount === 0 : hiddenCount === 0) &&
          snapshot.timeline.length > 0 &&
          snapshot.history_earliest_seq != null &&
          onLoadOlderHistory && (
          <button
            className="timeline-load-older"
            type="button"
            disabled={loadingOlder}
            onClick={async () => {
              setLoadingOlder(true);
              try {
                const loaded = await onLoadOlderHistory(200);
                if (!mountedRef.current) return;
                if (loaded) {
                  // Show the newly prepended page immediately.
                  if (useTurnFolding) {
                    revealMoreTurns();
                  } else {
                    setVisibleCount((count) => count + TIMELINE_WINDOW_STEP);
                  }
                }
              } finally {
                if (mountedRef.current) setLoadingOlder(false);
              }
            }}
          >
            {loadingOlder ? "正在加载更早历史…" : "加载更早历史"}
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
          const anchorChangeSet = turnChangeSetForRow(item, i);
          return (
            <Fragment key={`collapse:${group.key}`}>
              <TimelineCollapseSummary
                group={group}
                expanded={expanded}
                onToggle={() => toggleCollapseGroup(group.key)}
                navUserId={group.userMessageId}
              />
              {expanded && renderExpandedItems(expandedBeforeItems)}
              {renderTimelineItem(item, i, { renderChanges: !anchorChangeSet })}
              {expanded && renderExpandedItems(expandedAfterItems)}
              {/* When the change set is anchored to THIS turn's final reply, the
                  bar is the turn's FOOTER: a turn can keep working after that
                  reply — an interrupted turn ends on the tool call the user
                  stopped — and drawing the bar under the reply put "本轮对话"
                  in the middle of the turn with the agent's last call hanging
                  below it. With the reply last (the common case) the bar lands
                  exactly where it always did. A change set anchored to an
                  EARLIER reply is left on that reply instead (see
                  `itemsToCollapse`), so it keeps reading as produced-by-that-reply. */}
              {renderTurnChangesBar(anchorChangeSet)}
            </Fragment>
          );
        })}
        {snapshot.thinking_status === "Active" && (
          <div className="thinking-block">
            <button
              type="button"
              className={`thinking-indicator thinking-active ${snapshot.thinking_text ? "is-expandable" : ""}`}
              onClick={() => snapshot.thinking_text && setThinkingExpanded((v) => !v)}
              aria-expanded={snapshot.thinking_text ? thinkingExpanded : undefined}
            >
              <span className="thinking-bullet">•</span>
              <span className="thinking-text">
                {snapshot.thinking_text ? "思考过程" : "思考中"}
              </span>
              {snapshot.thinking_text && (
                <span className={`thinking-chevron ${thinkingExpanded ? "is-expanded" : ""}`}>›</span>
              )}
            </button>
            {thinkingExpanded && snapshot.thinking_text && (
              <ThinkingBodyTail text={snapshot.thinking_text} />
            )}
          </div>
        )}
        {proxyRetry?.active && (
          <div className="proxy-retry-indicator" role="status" aria-live="polite">
            <span className="proxy-retry-spinner" aria-hidden="true" />
            <span className="proxy-retry-text">
              {proxyRetryReasonLabel(proxyRetry.reason)}，正在自动重试
              {proxyRetry.max_attempts > 0
                ? `（${proxyRetry.attempt}/${proxyRetry.max_attempts}）`
                : ""}
            </span>
          </div>
        )}
        <div className="timeline-bottom-sentinel" ref={bottomSentinelRef} aria-hidden="true" />
      </div>
      </div>
      {userNavEntries.length > 0 && (
        // 导航作为 timeline-host 的同级覆盖层，absolute 相对非滚动的面板定位，
        // 与滚动完全解耦——无论滚到哪里都钉在可视区左侧垂直居中。
        <nav
          className="timeline-user-nav"
          aria-label="对话导航"
          onMouseLeave={() => setNavHoverId(null)}
        >
          {userNavEntries.map((entry, index) => {
            const hoverIndex = userNavEntries.findIndex((item) => item.id === navHoverId);
            // hover 的刻度最长，向两侧按距离线性衰减（参考 Cursor 的轮次导航）。
            const width =
              hoverIndex < 0
                ? undefined
                : Math.max(8, 22 - Math.abs(index - hoverIndex) * 3);
            return (
              <button
                key={entry.id}
                type="button"
                className={`timeline-user-nav-tick ${entry.id === navActiveId ? "is-active" : ""} ${entry.id === navHoverId ? "is-hovered" : ""}`}
                style={width !== undefined ? ({ "--nav-tick-w": width } as React.CSSProperties) : undefined}
                aria-label={`跳转到：${entry.userExcerpt}`}
                onMouseEnter={() => setNavHoverId(entry.id)}
                onFocus={() => setNavHoverId(entry.id)}
                onBlur={() => setNavHoverId(null)}
                onClick={() => handleUserNavJump(entry.id)}
              />
            );
          })}
          {navHoverEntry && (
            (() => {
              const hoverIndex = userNavEntries.findIndex((item) => item.id === navHoverId);
              const total = userNavEntries.length;
              // 预览卡垂直跟随被 hover 的刻度：按其在列中的比例定位。
              const ratio = total <= 1 ? 0.5 : hoverIndex / (total - 1);
              return (
                <div
                  className="timeline-user-nav-preview"
                  role="tooltip"
                  style={{ top: `${ratio * 100}%` }}
                >
              <div className="timeline-user-nav-preview-user">{navHoverEntry.userExcerpt}</div>
              {navHoverEntry.replyExcerpt && (
                <div className="timeline-user-nav-preview-reply">{navHoverEntry.replyExcerpt}</div>
              )}
                </div>
              );
            })()
          )}
        </nav>
      )}

      {/* 分叉点选择器：全量轮次列表 + 分叉方式（当前工作空间 / 新工作树）。 */}
      {forkPickerMessageId && onForkConversation && (
        <ForkDialog
          initialMessageId={turnOpeningUserMessageIdBefore(
            forkPickerMessageId,
            snapshot.timeline,
            allMessagesById,
          )}
          worktreeSupported={forkWorktreeSupported}
          onFork={onForkConversation}
          onClose={() => setForkPickerMessageId(null)}
        />
      )}

      {/* 交接：本地生成的交接说明（可编辑），可复制或新建会话带入。 */}
      {handoffMessageId && onHandoff && (
        <HandoffDialog
          digest={handoffDigest}
          onStartNewSession={onHandoff}
          onClose={() => setHandoffMessageId(null)}
        />
      )}
    </div>
  );
}

/** The turn-opening user prompt at-or-before a message: the fork picker
 *  preselects the turn containing the clicked fork button's reply. */
function turnOpeningUserMessageIdBefore(
  messageId: string,
  timeline: UiSnapshot["timeline"],
  messagesById: Map<string, UiSnapshot["messages"][number]>,
): string | null {
  const index = timeline.findIndex(
    (item) => typeof item === "object" && "Message" in item && item.Message === messageId,
  );
  const start = index >= 0 ? index : timeline.length - 1;
  for (let i = start; i >= 0; i -= 1) {
    const item = timeline[i];
    if (typeof item !== "object" || !("Message" in item)) continue;
    const message = messagesById.get(item.Message);
    if (!message || message.role !== "User") continue;
    if (message.is_steer || message.body.trim().toLowerCase() === "/compact") continue;
    return message.id;
  }
  return null;
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
