import type { EventFrame } from "../types/relay-protocol";
import type { UiSnapshot as Snapshot, PermissionInputRequest, UiSnapshotPatch } from "../types";
import {
  applySnapshotPatch,
  applyToolUpdated,
  applySessionStatus,
  materializeStreamingMessageBodies,
} from "./reducer";
import {
  appendStreamingMessageDelta,
  clearAllStreamingMessages,
  getStreamingMessageBody,
} from "./streaming-message-store";
import { diagnostics } from "../util/diagnostics";

type Listener = (snapshot: Snapshot | null) => void;
type PermissionHandler = (request: PermissionInputRequest) => void;
type ResyncHandler = () => void;

// Single UiSnapshot per active session + EventFrame application. Mirrors the
// desktop useWorkbenchSnapshot reducer + guard:
// - a SnapshotPatch is ignored if its session.id differs from the held session
//   or its revision is lower (stale). Revisions are per-session.
// - delta-only streaming patches fold the live stream store back into message
//   bodies, so assistant text grows in real time during a turn.
// - a revision GAP means a patch frame was lost on the wire; applying further
//   patches would misalign the delta chain, so a full resync is requested
//   instead (the controller debounces it into one GetState).
// - a GetState response that raced behind a newer pushed snapshot never
//   regresses the held state (it would wedge subsequent patches).
export class SessionStore {
  private snapshot: Snapshot | null = null;
  private lastLoggedUsageTokens: number | null = null;
  private activeSessionId: string | null = null;
  /** PC 端最后已知的"当前会话"（从任何到达的帧推断，含被丢弃的帧）。 */
  private pcActiveSessionId: string | null = null;
  private listeners = new Set<Listener>();
  private permissionHandler: PermissionHandler | null = null;
  private resyncHandler: ResyncHandler | null = null;

  /** 手机正在看（本地持有）的会话 id。 */
  get heldSessionId(): string | null {
    return this.activeSessionId;
  }

  /** PC 端当前会话的最后已知值；null = 未知。用于判断手机动作前是否需要
   *  把 PC 切回手机正在看的会话（见 services.ensurePcOnHeldSession）。 */
  get pcActiveHint(): string | null {
    return this.pcActiveSessionId;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => this.listeners.delete(listener);
  }

  get state(): Snapshot | null {
    return this.snapshot;
  }

  setPermissionHandler(handler: PermissionHandler | null): void {
    this.permissionHandler = handler;
  }

  /** Set the callback invoked when a lost patch (revision gap) requires a
   * full re-sync. The controller debounces this into a single GetState. */
  setResyncHandler(handler: ResyncHandler | null): void {
    this.resyncHandler = handler;
  }

  private requestResync(): void {
    this.resyncHandler?.();
  }

  /** Replace the entire snapshot (GetState response / SnapshotFull). A
   * same-session snapshot OLDER than the held one is dropped: it raced
   * behind patches that already landed, and accepting it would rewind the
   * revision so every subsequent patch fails the freshness guard. */
  setSnapshot(snapshot: Snapshot): void {
    // 跨会话守卫：PC 只会推它自己"当前会话"的状态（resync 的 GetState 响应、
    // 事件 Full 都一样）。桌面用户切到别的会话后，把这些内容收编进当前会话
    // 就是"会话 B 的消息覆盖了会话 A"的污染来源 —— 一律丢弃，只记下 PC 的
    // 位置供动作前切回（ensurePcOnHeldSession）。
    this.pcActiveSessionId = snapshot.session.id;
    if (this.activeSessionId && snapshot.session.id !== this.activeSessionId) {
      return;
    }
    if (
      this.snapshot !== null &&
      snapshot.session.id === this.snapshot.session.id &&
      snapshot.revision <= this.snapshot.revision
    ) {
      return;
    }
    this.activeSessionId = snapshot.session.id;
    this.snapshot = materializeStreamingMessageBodies(snapshot);
    this.emit();
  }

  /** Clear local state (e.g. on unbind/session switch reset). */
  clear(): void {
    this.activeSessionId = null;
    this.pcActiveSessionId = null;
    this.snapshot = null;
    clearAllStreamingMessages();
    this.emit();
  }

  /** Reset local snapshot for an incoming session before its data arrives.
   * Guards later full-snapshot events so a stale previous session cannot
   * overwrite the newly-selected one. Streaming bodies from the previous
   * session are dropped too (they only ever grow in memory otherwise). */
  beginSession(sessionId: string): void {
    this.activeSessionId = sessionId;
    this.snapshot = null;
    clearAllStreamingMessages();
    this.emit();
  }

  /** Apply an inbound EventFrame with the stale/duplicate patch guard. */
  applyEventFrame(frame: EventFrame): void {
    switch (frame.kind) {
      case "snapshot_full": {
        const incoming = frame.snapshot as Snapshot;
        // 记下 PC 的位置（含被丢弃的帧）：桌面用户切走后，手机动作前据此把
        // PC 切回手机正在看的会话（ensurePcOnHeldSession）。
        this.pcActiveSessionId = incoming.session.id;
        if (this.activeSessionId && incoming.session.id !== this.activeSessionId) {
          break;
        }
        this.activeSessionId = incoming.session.id;
        this.snapshot = materializeStreamingMessageBodies(incoming);
        const fullUsed = incoming.usage?.context?.used_tokens;
        diagnostics.log(
          "usage",
          `snapshot_full: session=${incoming.session.id.slice(0, 8)} used=${fullUsed ?? "null"} window=${incoming.usage?.context?.window_tokens ?? "null"} byModel=${incoming.usage?.by_model?.length ?? 0}`,
        );
      }
      break;
      case "snapshot_patch": {
        if (!this.snapshot) break;
        const patch = frame.patch as unknown as UiSnapshotPatch;
        if (patch.session.id !== this.snapshot.session.id) break;
        if (patch.revision < this.snapshot.revision) break; // stale
        const hasDeltas = (patch.message_deltas?.length ?? 0) > 0;
        if (patch.revision === this.snapshot.revision && !hasDeltas) break; // duplicate
        // A revision gap means a patch frame was lost: the cursor-based
        // diffs that follow are relative to a state we never saw. Ask for a
        // full snapshot instead of merging a misaligned chain.
        if (patch.revision > this.snapshot.revision + 1) {
          this.requestResync();
          break;
        }
        // Diagnostics: log usage only when the context occupancy CHANGES, so
        // streaming patch bursts don't spam the ring buffer.
        const patchUsed = patch.usage?.context?.used_tokens;
        if (patchUsed != null && patchUsed !== this.lastLoggedUsageTokens) {
          this.lastLoggedUsageTokens = patchUsed;
          diagnostics.log(
            "usage",
            `snapshot_patch: used=${patchUsed} window=${patch.usage?.context?.window_tokens ?? "null"} byModel=${patch.usage?.by_model?.length ?? 0}`,
          );
        }
        // Delta-only patches (empty `messages`) carry the growing assistant
        // text as `message_deltas`: land them in the streaming store (the
        // first delta is seeded with the snapshot body, mirroring the
        // desktop's render-time `ensureStreamingMessageBody`), apply the rest
        // of the patch (tools/timeline/status), then fold the live stream
        // bodies back in so the timeline renders the growing text instead of
        // a truncated prefix.
        const messageBodies = new Map(
          this.snapshot.messages.map((m) => [m.id, m.body] as const),
        );
        for (const delta of patch.message_deltas ?? []) {
          // The streaming store is append-only and cannot recover from a
          // desync by appending. The backend stamps each delta with the
          // UTF-16 length of the base body it extends; a mismatch means a
          // frame was dropped/duplicated upstream — request a full snapshot
          // instead of appending a misaligned suffix.
          if (typeof delta.base_len === "number") {
            const local =
              getStreamingMessageBody(delta.id) ?? messageBodies.get(delta.id) ?? "";
            if (local.length !== delta.base_len) {
              this.requestResync();
              continue;
            }
          }
          appendStreamingMessageDelta(
            delta.id,
            delta.append,
            messageBodies.get(delta.id),
          );
        }
        this.snapshot = materializeStreamingMessageBodies(
          applySnapshotPatch(this.snapshot, patch),
        );
        break;
      }
      case "tool_updated":
        if (this.snapshot) this.snapshot = applyToolUpdated(this.snapshot, frame.tool as unknown as import("../types").ToolInvocation);
        break;
      case "session_status_changed": {
        this.pcActiveSessionId = frame.session_id;
        // 状态帧属于它声明的会话：桌面用户在别的会话里切流/停流时，绝不能
        // 改写手机正在看的会话的状态。
        if (this.snapshot && frame.session_id !== this.snapshot.session.id) {
          break;
        }
        if (this.snapshot) {
          this.snapshot = applySessionStatus(
            this.snapshot,
            frame.status as Snapshot["session"]["status"],
          );
        }
        break;
      }
      case "permission_request":
        if (this.permissionHandler) this.permissionHandler(frame.request);
        break;
    }
    this.emit();
  }

  private emit(): void {
    for (const l of this.listeners) l(this.snapshot);
  }
}
// end of file