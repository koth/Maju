import type { PermissionInputRequest, PermissionInputResponse, ToolInvocation } from "../types";
import type { ControlClient } from "./control-client";

// Remote permission approval state machine. The phone is the SOLE approval
// gate for destructive remote operations (PC remote_mode blocks auto-approval).
// Destructive ops are default-deny: no "allow" is preselected, and a second
// explicit confirmation is required before ResolvePermission is sent. A
// per-request timeout watchdog dismisses stale requests (the PC aborts the
// tool on its own timeout).

export interface PendingApproval {
  permissionRequestId: string;
  toolCallId: string | null;
  toolName: string;
  request: PermissionInputRequest;
  receivedAtMs: number;
}

export type ApprovalListener = (pending: PendingApproval[]) => void;

const DEFAULT_APPROVAL_TIMEOUT_MS = 120_000;

/** Heuristic for whether an operation is destructive (requires 2nd confirm). */
export function isDestructive(tool: { name: string; kind: string }): boolean {
  const k = (tool.kind || "").toLowerCase();
  const n = (tool.name || "").toLowerCase();
  if (k === "edit" || k === "write" || k === "delete" || k === "command" || k === "shell") {
    return true;
  }
  // Match destructive keywords delimited by non-alphanumerics (so snake_case
  // tool names like `edit_file` / `run_bash` / `apply_patch` are caught). `\b`
  // would treat `_` as a word char and miss them.
  return /(?:^|[^a-z0-9])(write|edit|delete|remove|exec|run|bash|shell|apply_patch)(?:[^a-z0-9]|$)/.test(
    n,
  );
}

export class PermissionApprovalStore {
  private pending = new Map<string, PendingApproval>();
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  private listeners = new Set<ApprovalListener>();

  constructor(
    private controlClient: ControlClient | null,
    private readonly timeoutMs: number = DEFAULT_APPROVAL_TIMEOUT_MS,
  ) {
    // controlClient may arrive later (after pairing) via setControlClient.
  }

  /** Attach the control client once pairing completes and the loop starts. */
  setControlClient(client: ControlClient): void {
    this.controlClient = client;
  }

  subscribe(listener: ApprovalListener): () => void {
  this.listeners.add(listener);
  listener(this.snapshot());
  return () => this.listeners.delete(listener);
  }

  /** Surface a PermissionRequest (from EventFrame) as a pending approval. */
  surface(
  permissionRequestId: string,
  tool: Pick<ToolInvocation, "name" | "kind"> & { id: string; call_id: string },
  request: PermissionInputRequest,
  ): void {
  const approval: PendingApproval = {
  permissionRequestId,
  toolCallId: tool.call_id,
  toolName: tool.name,
  request,
  receivedAtMs: Date.now(),
  };
  this.pending.set(permissionRequestId, approval);
  const timer = setTimeout(
  () => this.dismiss(permissionRequestId),
  this.timeoutMs,
  );
  this.timers.set(permissionRequestId, timer);
  this.emit();
  }

  /**
   * Approve: send ResolvePermission with the chosen option. The caller is
   * responsible for the default-deny + second-confirm UX before calling this.
   */
  async approve(
    permissionRequestId: string,
    optionId: string | null,
    guidance?: string | null,
    inputResponse?: PermissionInputResponse | null,
  ): Promise<void> {
    if (!this.controlClient) throw new Error("control client not connected");
    await this.controlClient.resolvePermission({
      permission_request_id: permissionRequestId,
      option_id: optionId,
      guidance: guidance ?? null,
      input_response: inputResponse ?? null,
    });
    this.dismiss(permissionRequestId);
  }

  /** Deny: send ResolvePermission with no option (PC aborts the tool). */
  async deny(permissionRequestId: string, guidance?: string | null): Promise<void> {
    if (!this.controlClient) throw new Error("control client not connected");
    await this.controlClient.resolvePermission({
      permission_request_id: permissionRequestId,
      option_id: null,
      guidance: guidance ?? "denied by user",
      input_response: null,
    });
    this.dismiss(permissionRequestId);
  }

  /** Timeout/external dismiss: remove the pending approval without resolving
   * (the PC aborts on its own timeout). */
  dismiss(permissionRequestId: string): void {
  this.pending.delete(permissionRequestId);
  const timer = this.timers.get(permissionRequestId);
  if (timer) {
  clearTimeout(timer);
  this.timers.delete(permissionRequestId);
  }
  this.emit();
  }

  snapshot(): PendingApproval[] {
  return Array.from(this.pending.values());
  }

  private emit(): void {
  const snap = this.snapshot();
  for (const l of this.listeners) l(snap);
  }
}
// end of file
