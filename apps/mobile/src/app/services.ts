import type { RelayTransport } from "../relay/transport";
import { RelayConnection } from "../relay/connection";
import { ControlClient } from "../session/control-client";
import { SessionStore } from "../session/store";
import {
  PermissionApprovalStore,
  type PendingApproval,
} from "../session/permission";
import { ConnectionStateMachine, type ConnectionState } from "../relay/state-machine";
import { runReceiveLoop, type EventSink } from "../relay/driver";
import type { DeviceIdentity, SecretStore } from "../crypto/identity";
import { loadOrCreateIdentity, deviceId } from "../crypto/identity";
import type { SessionKey } from "../crypto/session-key";
import type { PairingQrPayload, Envelope, EventFrame, SubscriptionStatus } from "../types/relay-protocol";
import { parsePairingQr } from "../pairing/qr-parse";
import { runPairingHandshake, buildDeviceAuthArgs } from "../pairing/pairing-flow";
import type { UiSnapshot, PermissionInputResponse } from "../types";
import {
  loadBoundDevice,
  clearBoundDevice,
  canReconnectWithoutRescan,
} from "../account/binding";
import {
  subscriptionStateFromStatus,
  demoteOnExpiry,
  NO_SUBSCRIPTION,
  type SubscriptionState,
} from "../account/subscription";

// Framework-agnostic controller wiring the relay connection, control client,
// session store, permission store, and connection state machine. The React
// provider in `AppServicesContext` constructs one with a real `WsTransport`
// factory + `SecureSecretStore`; the integration harness builds one with an
// in-memory `ChannelTransport`. Fail-open: connection errors are surfaced as
// state transitions, never thrown to the UI.
export class AppController {
  readonly sessionStore = new SessionStore();
  readonly connState = new ConnectionStateMachine();
  readonly permissions = new PermissionApprovalStore(null);
  private readonly secretStore: SecretStore;
  private identity: DeviceIdentity | null = null;
  private conn: RelayConnection | null = null;
  private control: ControlClient | null = null;
  private loopPromise: Promise<void> | null = null;
  private stopLoop = false;
  private subscription: SubscriptionState = { ...NO_SUBSCRIPTION };
  private onSubscriptionChange: ((state: SubscriptionState) => void) | null = null;

  constructor(secretStore: SecretStore) {
    this.secretStore = secretStore;
    // Surface/dismiss pending permissions from the snapshot: the phone derives
    // the permission_request_id (== tool call_id) from the tool, since the
    // EventFrame::PermissionRequest carries only the PermissionInputRequest.
    this.sessionStore.setPermissionHandler(() => this.rescanPendingPermissions());
    this.sessionStore.subscribe(() => this.rescanPendingPermissions());
  }

  get connectionState(): ConnectionState {
    return this.connState.state;
  }

  get snapshot(): UiSnapshot | null {
    return this.sessionStore.state;
  }

  get subscriptionState(): SubscriptionState {
    return this.subscription;
  }

  get deviceIdValue(): string | null {
    return this.identity ? deviceId(this.identity) : null;
  }

  get pendingApprovals(): PendingApproval[] {
    return this.permissions.snapshot();
  }

  /** Load (or create) the persistent device identity. Idempotent. */
  async ensureIdentity(): Promise<DeviceIdentity> {
    if (this.identity) return this.identity;
    this.identity = await loadOrCreateIdentity(this.secretStore);
    return this.identity;
  }

  setSubscriptionListener(fn: (state: SubscriptionState) => void): void {
    this.onSubscriptionChange = fn;
  }

  /**
   * Pair with a PC from a scanned QR payload (JSON). Dials `transport` (already
   * connected for the real WebSocket path), runs DeviceAuth + the E2E
   * handshake, installs the session key, starts the receive loop, and resyncs
   * state via GetState. Throws on protocol/transport failure.
   */
  async pairFromTransport(
    transport: RelayTransport,
    qrJson: string,
    allowInsecureWs = false,
  ): Promise<void> {
    const qr = parsePairingQr(qrJson, allowInsecureWs) as PairingQrPayload;
    const identity = await this.ensureIdentity();
    this.connState.transition("connecting");
    this.conn = new RelayConnection(transport);

    this.connState.transition("authenticating");
    const auth = buildDeviceAuthArgs(identity);
    await this.conn.authenticate(auth.deviceId, auth.signature, auth.timestampMs);

    this.connState.transition("paired/e2e");
    const result = await runPairingHandshake(this.conn, identity, qr);
    this.conn.installSessionKey(result.sessionKey, result.pcDeviceId);

    this.control = new ControlClient(this.conn);
    this.permissions.setControlClient(this.control);
    this.connState.transition("connected");

    this.stopLoop = false;
    this.loopPromise = this.runLoop().catch(() => {
      // fail-open: a loop error demotes to disconnected for reconnect.
    });

    try {
      const res = await this.control.getState();
      this.sessionStore.setSnapshot(res.snapshot);
    } catch {
      // best-effort; pushed events will still populate the snapshot
    }
  }

  private async runLoop(): Promise<void> {
    if (!this.conn || !this.control) return;
    const onEvent: EventSink = (frame: EventFrame) =>
      this.sessionStore.applyEventFrame(frame);
    const onOther = (env: Envelope) => this.handleOther(env);
    await runReceiveLoop(
      this.conn,
      this.control,
      onEvent,
      onOther,
      () => this.stopLoop,
    );
    this.connState.transition("disconnected");
  }

  /** Route non-control/non-event envelopes (subscription/bind messages). */
  private handleOther(env: Envelope): void {
    if (env.type === "subscription_status") {
      const status = env.payload as SubscriptionStatus;
      const { state, mustRescan } = demoteOnExpiry(this.subscription, status);
      this.subscription = state;
      this.onSubscriptionChange?.(state);
      // On expiry we demote to re-scan semantics but keep the session alive.
      if (mustRescan) this.sessionStore.clear();
    }
  }

  /** Best-effort bound reconnect: re-scan is required unless bound+active. */
  canReconnectWithoutRescan(): boolean {
    return canReconnectWithoutRescan(this.subscription.active, null);
  }

  async loadBoundIfAny(): Promise<boolean> {
    const bound = await loadBoundDevice(this.secretStore);
    return bound !== null && this.subscription.active;
  }

  async unbindAndClear(): Promise<void> {
    await clearBoundDevice(this.secretStore);
  }

  setSubscriptionFromStatus(status: SubscriptionStatus): void {
    this.subscription = subscriptionStateFromStatus(status);
    this.onSubscriptionChange?.(this.subscription);
  }

  /** Surface pending permissions from the snapshot (the snapshot is the
   * source of the tool call_id, which is the permission_request_id on the
   * wire). */
  private rescanPendingPermissions(): void {
    const snap = this.sessionStore.state;
    const pending = this.permissions.snapshot();
    const pendingIds = new Set(pending.map((p) => p.permissionRequestId));
    if (snap) {
      for (const tool of snap.tools) {
        if (
          tool.permission_input &&
          !tool.permission_decision &&
          !pendingIds.has(tool.call_id)
        ) {
          this.permissions.surface(
            tool.call_id,
            { name: tool.name, kind: tool.kind, id: tool.id, call_id: tool.call_id },
            tool.permission_input,
          );
        }
      }
    }
    // Dismiss approvals whose tool is no longer pending (resolved/denied).
    const stillPending = new Set(
      (snap?.tools ?? [])
        .filter((t) => t.permission_input && !t.permission_decision)
        .map((t) => t.call_id),
    );
    for (const p of pending) {
      if (!stillPending.has(p.permissionRequestId)) {
        this.permissions.dismiss(p.permissionRequestId);
      }
    }
  }

  // --- Session control ops (proxy to the control client) ---

  async listSessions() {
    return this.controlClient().listSessions();
  }

  async createSession(opts?: { workspaceRoot?: string | null; agent?: string | null }) {
    const res = await this.controlClient().createSession({
      workspace_root: opts?.workspaceRoot ?? null,
      agent: (opts?.agent ?? null) as never,
    });
    return res.session_id;
  }

  async switchSession(sessionId: string, workspaceRoot?: string | null) {
    return this.controlClient().switchSession(sessionId, workspaceRoot ?? null);
  }

  async sendPrompt(text: string) {
    return this.controlClient().sendPrompt([
      { type: "text", text } as never,
    ]);
  }

  async cancel() {
    return this.controlClient().cancel();
  }

  async stopTool(toolCallId: string) {
    return this.controlClient().stopTool(toolCallId);
  }

  /** Approve a pending permission by its (call_id) id and chosen option id. */
 async approvePermission(
   permissionRequestId: string,
   optionId: string | null,
   guidance?: string | null,
 ) {
   await this.permissions.approve(permissionRequestId, optionId, guidance ?? null, null);
 }

  async approvePermissionWithInput(
    permissionRequestId: string,
    optionId: string | null,
    guidance: string | null,
    inputResponse: PermissionInputResponse,
  ) {
    await this.permissions.approve(permissionRequestId, optionId, guidance, inputResponse);
  }

  async denyPermission(permissionRequestId: string, guidance?: string | null) {
    await this.permissions.deny(permissionRequestId, guidance ?? null);
  }

  /** Install a session key directly (used by bound reconnect / tests). */
  installSessionKey(key: SessionKey, peerDeviceId: string): void {
    if (!this.conn) throw new Error("no connection to install session key on");
    this.conn.installSessionKey(key, peerDeviceId);
  }

  /** Attach a transport + control client without the pairing handshake. */
  attachConnection(transport: RelayTransport, control?: ControlClient): ControlClient {
    this.conn = new RelayConnection(transport);
    this.control = control ?? new ControlClient(this.conn);
    this.permissions.setControlClient(this.control);
    return this.control;
  }

  /** Start (or restart) the receive loop using the current connection. */
  startReceiveLoop(): Promise<void> {
    this.stopLoop = false;
    this.loopPromise = this.runLoop().catch(() => {});
    return this.loopPromise;
  }

  /** Disconnect: stop the loop, clear the session, drop the session key. */
  async disconnect(): Promise<void> {
    this.stopLoop = true;
    try {
      await this.conn?.close();
    } catch {
      // ignore
    }
    this.conn = null;
    this.control = null;
    this.sessionStore.clear();
    this.connState.reset();
  }

  private controlClient(): ControlClient {
    if (!this.control) {
      throw new Error("not connected: pair with a PC first");
    }
    return this.control;
  }
}

export type { SecretStore };
// end of file
