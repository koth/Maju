// Handoff dialog — "交接给下一个智能体".
//
// Shows the briefing built from the conversation window (see `handoff.ts`),
// lets the user edit it, polish it with the configured title model, and hands it
// off: copy it to the clipboard, or start a fresh session in this workspace —
// on the agent the user picks — with the briefing in its composer.
//
// The local digest is instant, so the dialog opens with text; polishing is an
// explicit action whose failure leaves that text untouched.

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, ClipboardCopy, Handshake, Sparkles } from "lucide-react";
import type { AgentCliId } from "../../types";
import { sessionHandoffSummary } from "../../lib/tauri";
import { AgentChoiceField } from "../session/AgentChoiceField";
import "./HandoffDialog.css";

interface HandoffDialogProps {
  /** The generated briefing; empty means there was nothing to summarize. */
  digest: string;
  /** Start a new session in the current workspace carrying this briefing. */
  onStartNewSession: (
    digest: string,
    agent: AgentCliId | null,
    preset: string | null,
  ) => Promise<void> | void;
  onClose: () => void;
}

export function HandoffDialog({ digest, onStartNewSession, onClose }: HandoffDialogProps) {
  const [text, setText] = useState(digest);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [polishing, setPolishing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [agent, setAgent] = useState<AgentCliId | null>(null);
  const [preset, setPreset] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    setText(digest);
  }, [digest]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy && !polishing) onClose();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [busy, polishing, onClose]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch (copyError) {
      setError(`复制失败：${copyError instanceof Error ? copyError.message : String(copyError)}`);
    }
  };

  /// Hand the local digest to the configured title model for a readable
  /// rewrite. A failure keeps the local text — it stays the source of truth.
  const polish = async () => {
    setPolishing(true);
    setError(null);
    try {
      setText(await sessionHandoffSummary(text));
    } catch (polishError) {
      setError(
        `模型润色失败，仍使用本地摘要：${
          polishError instanceof Error ? polishError.message : String(polishError)
        }`,
      );
    } finally {
      setPolishing(false);
    }
  };

  const startNewSession = async () => {
    setBusy(true);
    setError(null);
    try {
      await onStartNewSession(text, agent, preset);
      onClose();
    } catch (handoffError) {
      setError(
        `新建会话失败：${handoffError instanceof Error ? handoffError.message : String(handoffError)}`,
      );
      setBusy(false);
    }
  };

  const locked = busy || polishing;

  return createPortal(
    <div
      className="handoff-dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !locked) onClose();
      }}
    >
      <div className="handoff-dialog" role="dialog" aria-modal="true" aria-label="交接给下一个智能体">
        <div className="handoff-dialog-header">
          <div className="handoff-dialog-icon" aria-hidden="true">
            <Handshake size={16} strokeWidth={2.1} />
          </div>
          <div className="handoff-dialog-copy">
            <div className="handoff-dialog-title">交接给下一个智能体</div>
            <div className="handoff-dialog-subtitle">
              这是根据当前对话本地整理的交接说明，可以直接改，也可以用模型润色。新会话会把这段内容放进输入框，由你决定改成什么、什么时候发。
            </div>
          </div>
        </div>

        {digest ? (
          <textarea
            ref={textareaRef}
            className="handoff-dialog-text"
            aria-label="交接说明"
            value={text}
            spellCheck={false}
            readOnly={polishing}
            onChange={(event) => setText(event.target.value)}
          />
        ) : (
          <div className="handoff-dialog-empty">这段对话还没有可以交接的内容。</div>
        )}

        <div className="handoff-dialog-agent">
          <div className="handoff-dialog-section-label">交给哪个 Agent</div>
          <AgentChoiceField
            value={agent}
            onChange={setAgent}
            preset={preset}
            onPresetChange={setPreset}
            disabled={locked}
          />
        </div>

        {error && (
          <div className="handoff-dialog-error" role="alert">
            {error}
          </div>
        )}

        <div className="handoff-dialog-footer">
          <button
            type="button"
            className="handoff-dialog-btn"
            onClick={polish}
            disabled={!digest || locked}
            title="用「设置 → 会话标题」配置的模型整理成一段可读的交接稿"
          >
            <Sparkles size={15} strokeWidth={2} aria-hidden="true" />
            {polishing ? "正在润色…" : "用模型润色"}
          </button>
          <button
            type="button"
            className="handoff-dialog-btn"
            onClick={copy}
            disabled={!digest || locked}
          >
            {copied ? (
              <Check size={15} strokeWidth={2} aria-hidden="true" />
            ) : (
              <ClipboardCopy size={15} strokeWidth={2} aria-hidden="true" />
            )}
            {copied ? "已复制" : "复制"}
          </button>
          <button
            type="button"
            className="handoff-dialog-btn handoff-dialog-btn-primary"
            onClick={startNewSession}
            disabled={!digest || locked}
          >
            <Handshake size={15} strokeWidth={2} aria-hidden="true" />
            {busy ? "正在新建…" : "新建会话并带入"}
          </button>
          <button
            type="button"
            className="handoff-dialog-btn handoff-dialog-btn-quiet"
            onClick={onClose}
            disabled={busy}
          >
            取消
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
