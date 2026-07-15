import type { EventFrame } from "../types/relay-protocol";
import type { UiSnapshot as Snapshot, PermissionInputRequest } from "../types";
import {
  applySnapshotPatch,
  applyToolUpdated,
  applySessionStatus,
} from "./reducer";
import { appendStreamingMessageDelta } from "./streaming-message-store";

type Listener = (snapshot: Snapshot | null) => void;
type PermissionHandler = (request: PermissionInputRequest) => void;

// Single UiSnapshot per active session + EventFrame application. Mirrors the
// desktop useWorkbenchSnapshot reducer + guard: a SnapshotPatch is ignored if
// its session.id differs from the held session or its revision is not greater
// than the held revision (stale/duplicate). Revisions are per-session.
export class SessionStore {
  private snapshot: Snapshot | null = null;
  private listeners = new Set<Listener>();
  private permissionHandler: PermissionHandler | null = null;

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

  /** Replace the entire snapshot (SnapshotFull). */
  setSnapshot(snapshot: Snapshot): void {
  this.snapshot = snapshot;
  this.emit();
  }

  /** Clear local state (e.g. on unbind/session switch reset). */
  clear(): void {
  this.snapshot = null;
  this.emit();
  }

  /** Apply an inbound EventFrame with the stale/duplicate patch guard. */
  applyEventFrame(frame: EventFrame): void {
  switch (frame.kind) {
  case "snapshot_full":
  this.snapshot = frame.snapshot as Snapshot;
  break;
  case "snapshot_patch": {
  if (!this.snapshot) break;
  const patch = frame.patch as unknown as import("../types").UiSnapshotPatch;
  const sameSession = patch.session.id === this.snapshot.session.id;
  const newerRevision = patch.revision > this.snapshot.revision;
  if (!sameSession || !newerRevision) break; // stale/duplicate guard
  for (const delta of patch.message_deltas) {
  appendStreamingMessageDelta(delta.id, delta.append);
  }
  this.snapshot = applySnapshotPatch(this.snapshot, patch);
  break;
  }
  case "tool_updated":
  if (this.snapshot) this.snapshot = applyToolUpdated(this.snapshot, frame.tool as unknown as import("../types").ToolInvocation);
  break;
  case "session_status_changed":
  if (this.snapshot) {
  this.snapshot = applySessionStatus(
  this.snapshot,
  frame.status as Snapshot["session"]["status"],
  );
  }
  break;
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
