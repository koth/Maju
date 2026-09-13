import type { RelayTransport } from "../relay/transport";
import { RelayConnection } from "../relay/connection";
import { ControlClient } from "../session/control-client";
import { SessionStore } from "../session/store";
import {
  PermissionApprovalStore,
  isPendingPermissionTool,
  type PendingApproval,
} from "../session/permission";
import { ConnectionStateMachine, type ConnectionState } from "../relay/state-machine";
import { runReceiveLoop, type EventSink } from "../relay/driver";
import type { DeviceIdentity, SecretStore } from "../crypto/identity";
import { loadOrCreateIdentity, deviceId } from "../crypto/identity";
import type { SessionKey } from "../crypto/session-key";
import type { PairingQrPayload, Envelope, EventFrame, SubscriptionStatus } from "../types/relay-protocol";
import { fromMessage } from "../relay/framing";
import { parsePairingQr } from "../pairing/qr-parse";
import {
  runPairingHandshake,
  runPairingResume,
  buildDeviceAuthArgs,
} from "../pairing/pairing-flow";
import type { UiSnapshot, PermissionInputResponse } from "../types";
import {
  loadBoundDevices,
  upsertBoundDevice,
  removeBoundDevice as removeBoundDeviceRecord,
  clearAllBoundDevices,
} from "../account/binding";
import type { BoundDevice } from "../account/binding";
import { diagnostics } from "../util/diagnostics";
import {
  loadSession,
  persistSession,
  clearSession,
} from "../account/session";
import {
  subscriptionStateFromStatus,
  demoteOnExpiry,
  NO_SUBSCRIPTION,
  type SubscriptionState,
} from "../account/subscription";
import {
  TurnCompletionWatcher,
  type AlertPresenter,
} from "../session/turn-completion";
import {
  DEFAULT_ALERT_SETTINGS,
  loadAlertSettings,
  saveAlertSettings,
  type AlertSettings,
} from "../features/notifications/settings";

// Framework-agnostic controller wiring the relay connection, control client,
// session store, permission store, and connection state machine. The React
// provider in `AppServicesContext` constructs one with a real `WsTransport`
// factory + `SecureSecretStore`; the integration harness builds one with an
// in-memory `ChannelTransport`. Fail-open: connection errors are surfaced as
// state transitions, never thrown to the UI.
type ReconnectTransportFactory = (endpoint: string) => Promise<RelayTransport>;

export class AppController {
  readonly sessionStore = new SessionStore();
  readonly connState = new ConnectionStateMachine();
  readonly permissions = new PermissionApprovalStore(null);
  private readonly secretStore: SecretStore;
  private identity: DeviceIdentity | null = null;
  private conn: RelayConnection | null = null;
  private control: ControlClient | null = null;
  private loopPromise: Promise<void> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private connectingPromise: Promise<boolean> | null = null;
  private reconnectAttempt = 0;
  private connectionGeneration = 0;
  private lastSessionKey: SessionKey | null = null;
  private lastPeerDeviceId: string | null = null;
  /** The machine the user paired with or explicitly connected to. Auto-resume
   * (live-drop recovery) always targets this machine, never the whole list. */
  private activeBound: BoundDevice | null = null;
  /** True while `lastSessionKey` was restored from storage rather than
   * established in-process. A restored key cannot be trusted for the cached
   * fast path: the PC keeps its copy in memory only, so any PC restart (or
   * key rotation) makes every fast-path frame silently undecryptable and the
   * phone hangs until a full control-request timeout. Cold starts therefore
   * always run the explicit pairing_resume handshake (~1 round trip). */
  private sessionKeyNeedsResume = false;
  private stopLoop = false;
  private subscription: SubscriptionState = { ...NO_SUBSCRIPTION };
  private onSubscriptionChange: ((state: SubscriptionState) => void) | null = null;
  private readonly reconnectTransportFactory: ReconnectTransportFactory | null;
  /** Turn-completion alerting: the watcher is always attached (it is pure
   * snapshot-diff logic); the presenter is injected by the app shell, which
   * owns the native adapters. Without a presenter the watcher is a no-op. */
  private readonly turnWatcher: TurnCompletionWatcher;
  private alertPresenter: AlertPresenter | null = null;
  private alertSettingsState: AlertSettings = { ...DEFAULT_ALERT_SETTINGS };
  private alertSettingsListeners = new Set<(settings: AlertSettings) => void>();

  constructor(
    secretStore: SecretStore,
    reconnectTransportFactory?: ReconnectTransportFactory,
  ) {
    this.secretStore = secretStore;
    this.reconnectTransportFactory = reconnectTransportFactory ?? null;
    // Surface/dismiss pending permissions from the snapshot: the phone derives
    // the permission_request_id (== tool call_id) from the tool, since the
    // EventFrame::PermissionRequest carries only the PermissionInputRequest.
    this.sessionStore.setPermissionHandler(() => this.rescanPendingPermissions());
    this.sessionStore.subscribe(() => this.rescanPendingPermissions());
    // A lost patch frame (revision gap) wedges the incremental chain: the
    // store asks for a full re-sync, debounced here into one GetState.
    this.sessionStore.setResyncHandler(() => this.scheduleSnapshotResync());
    // Turn-completion alerts: dispatch through the shell-installed presenter
    // (null until the React layer wires the native adapters).
    this.turnWatcher = new TurnCompletionWatcher(this.sessionStore, {
      onTurnCompleted: (ctx) => this.alertPresenter?.onTurnCompleted(ctx),
      onTurnInterrupted: (ctx) => this.alertPresenter?.onTurnInterrupted(ctx),
    });
  }

  private resyncTimer: ReturnType<typeof setTimeout> | null = null;

  /** Debounced full-snapshot re-sync after a detected patch gap. */
  private scheduleSnapshotResync(): void {
    if (this.resyncTimer !== null) return;
    this.resyncTimer = setTimeout(() => {
      this.resyncTimer = null;
      if (this.connState.state !== "connected" || !this.control) return;
      void this.getState().catch((e) => {
        diagnostics.log(
          "services",
          `snapshot resync failed: ${e instanceof Error ? e.message : String(e)}`,
        );
      });
    }, 400);
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

  /** The machine the controller is currently bound to (the active pairing),
   * or null when none is selected. Drives the machine switcher's "which PC am
   * I on" chip and its green connected dot. */
  get activePeerDeviceId(): string | null {
    return this.activeBound?.peer_device_id ?? null;
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

    let result;
    try {
      this.connState.transition("authenticating");
      const auth = buildDeviceAuthArgs(identity);
      await this.conn.authenticate(auth.deviceId, auth.devicePubkey, auth.signature, auth.timestampMs);

      this.connState.transition("paired/e2e");
      result = await runPairingHandshake(this.conn, identity, qr);
    } catch (e) {
      // Surface the failure: roll the state machine back so the UI leaves
      // the connecting spinner and shows the error on the pairing screen.
      this.connState.transition("disconnected");
      throw e;
    }
    const bound: BoundDevice = {
      device_id: deviceId(identity),
      // The free/bound account path retains a resumable pairing token after
      // the first scan. `auth_token` is empty until the account bind flow can
      // fill it; resume only needs the pairing token + peer static key.
      auth_token: "",
      pairing_token: result.pairingToken,
      peer_device_id: result.pcDeviceId,
      peer_static_pubkey_b64: result.pcStaticPubkeyB64,
      relay_endpoint: qr.relay_endpoint,
      // The PC names itself in its QR; without it the machines list can only
      // show a device-id prefix, which is unrecognizable across several PCs.
      label: qr.pc_name,
      // The PC's own address, NOT the relay host: every machine shares the
      // relay host, so displaying it made the list indistinguishable.
      peer_ip: qr.pc_ip,
      bound_at: Date.now(),
    };
    // Multi-machine: every scan produces its own pairing token, so machines
    // accumulate in the list (keyed by peer device id) instead of the new
    // scan overwriting the previous binding.
    await upsertBoundDevice(this.secretStore, bound);
    this.activeBound = bound;
    // Diagnostic: log session key prefix + peer id so it can be matched
    // against the PC's derived key. (Hermes console -> logcat ReactNativeJS.)
    const keyHex = Array.from(result.sessionKey.bytes.slice(0, 8))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
    diagnostics.log(
      "pairing",
      `sessionKey prefix=${keyHex} pcDeviceId=${result.pcDeviceId} phoneDeviceId=${result.phoneDeviceId}`,
    );
    this.conn.installSessionKey(result.sessionKey, result.pcDeviceId);
    this.lastSessionKey = result.sessionKey;
    this.lastPeerDeviceId = result.pcDeviceId;
    await persistSession(this.secretStore, {
      key: result.sessionKey,
      peer_device_id: result.pcDeviceId,
    });

    this.control = new ControlClient(this.conn);
    this.permissions.setControlClient(this.control);
    this.connState.transition("connected");

    const generation = ++this.connectionGeneration;
    this.stopLoop = false;
    this.loopPromise = this.runLoop(generation).catch(() => {});
    this.startHeartbeat();
  }

  private async runLoop(generation: number): Promise<void> {
    if (!this.conn || !this.control) return;
    const onEvent: EventSink = (frame: EventFrame) =>
      this.sessionStore.applyEventFrame(frame);
    const onOther = (env: Envelope) => this.handleOther(env);
    try {
      await runReceiveLoop(
        this.conn,
        this.control,
        onEvent,
        onOther,
        () => this.stopLoop,
      );
    } catch (e) {
      // A crashing receive loop must still trigger reconnection handling;
      // swallowing the error here would strand the socket half-dead (open
      // transport, no dispatcher) until the next heartbeat write fails.
      diagnostics.log("conn", `receive loop crashed: ${e instanceof Error ? e.message : String(e)}`);
    }
    if (this.stopLoop || generation !== this.connectionGeneration) return;
    // Fail any in-flight control requests immediately: their socket is gone
    // (or being replaced), so waiting out the 60s timeout is pure latency.
    this.control.failAll("connection lost");
    void this.handleConnectionLoss();
  }

  /** A receive loop ended. Reconnect when a persisted pairing exists, or
   * surface disconnected otherwise. */
  private async handleConnectionLoss(): Promise<void> {
    this.stopHeartbeat();
    if (this.stopLoop) {
      this.connState.transition("disconnected");
      return;
    }
    const resumed = await this.tryAutoResume();
    if (!resumed && !this.stopLoop) {
      this.connState.transition("disconnected");
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  private startHeartbeat(): void {
    this.stopHeartbeat();
    this.heartbeatTimer = setInterval(() => {
      if (!this.conn || this.stopLoop) return;
      // Plaintext heartbeat: the relay must see the frame to keep the
      // connection alive. An encrypted envelope carries `to_device_id` and
      // is routed to the peer instead of being consumed by the relay, so
      // the relay's heartbeat_timeout would reap this connection.
      this.conn
        .sendPlaintext(fromMessage(null, { type: "heartbeat", payload: null }))
        .catch(() => {});
    }, 20_000);
  }

  private stopHeartbeat(): void {
    if (this.heartbeatTimer !== null) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
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

  // --- Multi-machine binding management ---

  /** Every persisted machine binding (for the machines list screen). */
  async listMachines(): Promise<BoundDevice[]> {
    return loadBoundDevices(this.secretStore);
  }

  /** Forget one machine. If it is the active one, drop its session state too
   * and disconnect when a live connection exists. Returns the remaining list. */
  async removeMachine(peerDeviceId: string): Promise<BoundDevice[]> {
    const remaining = await removeBoundDeviceRecord(this.secretStore, peerDeviceId);
    if (this.activeBound?.peer_device_id === peerDeviceId) {
      this.activeBound = null;
      this.lastSessionKey = null;
      this.lastPeerDeviceId = null;
      await clearSession(this.secretStore);
      if (this.conn) await this.disconnect();
    }
    return remaining;
  }

  /**
   * User-initiated connect from the machines list: dial the machine's relay
   * endpoint, run DeviceAuth + a fresh E2E resume handshake with ITS pairing
   * token + static key, restart the receive loop, and resync state via
   * GetState. Single attempt — failures surface to the caller so the UI can
   * show the error next to the machine row (no background retry loop).
   */
  async connectToBoundDevice(peerDeviceId: string): Promise<void> {
    if (this.connectingPromise) {
      // A retry loop (auto-resume) or another connect is in flight; unroll it
      // before starting the user-directed attempt.
      this.stopLoop = true;
      this.connectionGeneration += 1;
      try {
        await this.connectingPromise;
      } catch {
        // Auto-resume failure is irrelevant here — the user picked a machine.
      }
      this.connectingPromise = null;
    }
    const devices = await loadBoundDevices(this.secretStore);
    const bound = devices.find((d) => d.peer_device_id === peerDeviceId);
    if (!bound) {
      throw new Error("machine is not bound; scan its QR code first");
    }
    // Tear down any live connection (or lingering half-open one) before
    // dialing the selected machine: switching machines must not leave the
    // previous machine's receive loop running against a replaced socket.
    if (this.conn) {
      await this.disconnect();
    }
    // Switching machines invalidates the previous machine's cached key and
    // snapshot: the persisted session key is per-peer and the session store
    // must not show machine A's sessions while talking to machine B.
    if (this.lastPeerDeviceId !== peerDeviceId) {
      this.lastSessionKey = null;
      this.lastPeerDeviceId = null;
      this.sessionKeyNeedsResume = true;
      await clearSession(this.secretStore);
      this.sessionStore.clear();
    }
    this.activeBound = bound;
    this.connectingPromise = (async () => {
      await this.connectOnce(bound);
      return true;
    })().finally(() => {
      this.connectingPromise = null;
    });
    await this.connectingPromise;
  }

  /** One-shot connect to a specific machine. Throws on failure after rolling
   * the state machine back and tearing the half-open connection down. */
  private async connectOnce(bound: BoundDevice): Promise<boolean> {
    if (!this.reconnectTransportFactory) {
      throw new Error("no transport factory configured");
    }
    const endpoint = bound.relay_endpoint;
    if (!endpoint) {
      throw new Error("machine record is missing a relay endpoint; re-scan required");
    }
    try {
      const ws = await this.reconnectTransportFactory(endpoint);
      await this.establishBoundConnection(ws, bound);
      const generation = ++this.connectionGeneration;
      this.stopLoop = false;
      this.loopPromise = this.runLoop(generation).catch(() => {});
      this.startHeartbeat();
      if (this.control) {
        await this.getState();
      }
      this.connState.transition("connected");
      this.reconnectAttempt = 0;
      diagnostics.log("services", `connected to machine ${bound.peer_device_id.slice(0, 8)}…`);
      return true;
    } catch (e) {
      diagnostics.log(
        "services",
        `connect to machine failed: ${e instanceof Error ? e.message : String(e)}`,
      );
      this.stopHeartbeat();
      this.stopLoop = true;
      try {
        await this.conn?.close();
      } catch {
        // ignore
      }
      this.conn = null;
      this.control = null;
      this.connState.transition("disconnected");
      throw e;
    }
  }

  /**
   * Reconnect to the previously bound PC without scanning a fresh QR. The
   * persisted `BoundDevice` supplies the pairing token and PC static public
   * key; this runs DeviceAuth + a fresh E2E resume handshake, installs the
   * derived key, and restarts the receive loop.
   */
  private async establishBoundConnection(
    transport: RelayTransport,
    bound: BoundDevice,
  ): Promise<void> {
    const identity = await this.ensureIdentity();
    this.connState.transition("connecting");
    this.conn = new RelayConnection(transport);

    this.connState.transition("authenticating");
    const auth = buildDeviceAuthArgs(identity);
    await this.conn.authenticate(auth.deviceId, auth.devicePubkey, auth.signature, auth.timestampMs);

    this.connState.transition("paired/e2e");
    const result = await runPairingResume(this.conn, bound);
    this.conn.installSessionKey(result.sessionKey, result.pcDeviceId);
    this.lastSessionKey = result.sessionKey;
    this.lastPeerDeviceId = result.pcDeviceId;
    // The key is now authoritative and in-process: transient drops may use
    // the cached fast path until this process ends.
    this.sessionKeyNeedsResume = false;
    await persistSession(this.secretStore, {
      key: result.sessionKey,
      peer_device_id: result.pcDeviceId,
    });

    this.control = new ControlClient(this.conn);
    this.permissions.setControlClient(this.control);
  }

  /**
   * Reconnect with the same E2E key already established in this process.
   * This keeps a transient relay/network drop seamless without asking the PC
   * to re-run a pairing handshake. Only used when both sides still share the
   * in-memory key from the original pairing.
   */
  private async establishCachedConnection(
    transport: RelayTransport,
    key: SessionKey,
    peerDeviceId: string,
  ): Promise<void> {
    const identity = await this.ensureIdentity();
    this.connState.transition("connecting");
    this.conn = new RelayConnection(transport);

    this.connState.transition("authenticating");
    const auth = buildDeviceAuthArgs(identity);
    await this.conn.authenticate(auth.deviceId, auth.devicePubkey, auth.signature, auth.timestampMs);

    this.conn.installSessionKey(key, peerDeviceId);
    this.control = new ControlClient(this.conn);
    this.permissions.setControlClient(this.control);
  }

  /**
   * Reconnect using a persisted pairing token + peer static key, dialing a
   * fresh transport when only the endpoint is known. Used for live-drop
   * recovery of the ACTIVE machine (never picks a machine on its own).
   */
  async resumeFromBoundTransport(transport?: RelayTransport): Promise<void> {
    const bound = this.activeBound;
    if (!bound) {
      throw new Error("no active machine to resume");
    }
    const endpoint = bound.relay_endpoint;
    if (!transport && !endpoint) {
      throw new Error("persisted pairing is missing a relay endpoint");
    }

    // Restore the persisted E2E session key so a fresh process COULD attempt
    // a cached-key reconnect. We deliberately do not trust it for the fast
    // path (see `sessionKeyNeedsResume`): the PC only keeps its copy in
    // memory, so after any PC restart the restored key is stale and every
    // encrypted frame would be dropped silently. Keep it around only as a
    // diagnostic fallback; the resume handshake below always re-establishes
    // the key authoritatively. The persisted key is per-peer — a key saved
    // for a different machine must be discarded, not reused.
    if (!this.lastSessionKey || !this.lastPeerDeviceId) {
      const persisted = await loadSession(this.secretStore);
      if (persisted && persisted.peer_device_id === bound.peer_device_id) {
        this.lastSessionKey = persisted.key;
        this.lastPeerDeviceId = persisted.peer_device_id;
        this.sessionKeyNeedsResume = true;
        diagnostics.log("services", "restored persisted session key; will re-resume via pairing handshake");
      }
    }

    if (transport) {
      if (
        this.lastSessionKey &&
        this.lastPeerDeviceId &&
        !this.sessionKeyNeedsResume
      ) {
        await this.establishCachedConnection(
          transport,
          this.lastSessionKey,
          this.lastPeerDeviceId,
        );
      } else {
        await this.establishBoundConnection(transport, bound);
      }
    } else {
      if (!this.reconnectTransportFactory) {
        throw new Error("no transport factory configured for resume");
      }
      const ws = await this.reconnectTransportFactory(endpoint!);
      if (
        this.lastSessionKey &&
        this.lastPeerDeviceId &&
        !this.sessionKeyNeedsResume
      ) {
        await this.establishCachedConnection(
          ws,
          this.lastSessionKey,
          this.lastPeerDeviceId,
        );
      } else {
        await this.establishBoundConnection(ws, bound);
      }
    }

    const generation = ++this.connectionGeneration;
    this.stopLoop = false;
    this.loopPromise = this.runLoop(generation).catch(() => {});
    this.startHeartbeat();
  }

  /** Attempt the persisted pairing resume without input (startup recovery). */
  async tryAutoResume(): Promise<boolean> {
    if (this.connectingPromise) {
      return this.connectingPromise;
    }
    this.connectingPromise = this.doAutoResume().finally(() => {
      this.connectingPromise = null;
    });
    return this.connectingPromise;
  }

  private async doAutoResume(): Promise<boolean> {
    if (!this.activeBound) {
      // No machine selected (fresh start or the binding was removed): the
      // machines list is the landing state, there is nothing to auto-resume.
      this.connState.transition("disconnected");
      return false;
    }
    while (!this.stopLoop) {
      try {
        diagnostics.log("services", `resume attempt ${this.reconnectAttempt + 1}`);
        await this.resumeFromBoundTransport();
        // The user may have hit the kill switch (or picked another machine)
        // while the resume handshake was in flight: do not yank the UI back
        // to connected after a deliberate disconnect.
        if (this.stopLoop) return false;
        if (this.control) {
          diagnostics.log("services", "resume connected; requesting initial state");
          await this.getState();
          diagnostics.log("services", "initial state received");
        }
        this.connState.transition("connected");
        this.reconnectAttempt = 0;
        diagnostics.log("services", "resume succeeded");
        return true;
      } catch (e) {
        diagnostics.log("services", `resume failed: ${e instanceof Error ? e.message : String(e)}`);
        if (this.lastSessionKey && this.lastPeerDeviceId) {
          this.lastSessionKey = null;
          this.lastPeerDeviceId = null;
          await clearSession(this.secretStore);
        }
        this.reconnectAttempt += 1;
        this.connState.transition("connecting");
        if (this.reconnectAttempt >= 8) {
          // Repeated resume failures usually mean the persisted binding is
          // stale (e.g. the PC or phone identity rotated). Drop ONLY the
          // failing machine so the other bindings on the machines list stay
          // usable; the user is prompted to re-scan that machine's QR.
          if (this.activeBound) {
            const stale = this.activeBound.peer_device_id;
            this.activeBound = null;
            await removeBoundDeviceRecord(this.secretStore, stale);
            diagnostics.log(
              "services",
              "auto resume exhausted; removed stale machine binding; re-scan required",
            );
          }
          this.connState.transition("disconnected");
          return false;
        }
        const delay = Math.min(500 * 2 ** (this.reconnectAttempt - 1), 8_000);
        await this.sleep(delay);
      }
    }
    return false;
  }

  /** App startup: load the device identity and the persisted machine list.
   * Deliberately does NOT auto-connect: the machines list screen is the
   * landing state, and the user picks which machine to connect to (a phone
   * may be bound to several PCs). Migration of the legacy single-record
   * binding happens inside `loadBoundDevices`. */
  async boot(): Promise<boolean> {
    await this.ensureIdentity();
    // Alert settings load before any session can complete a turn; failures
    // fall back to defaults (alerting must never block boot).
    try {
      this.alertSettingsState = await loadAlertSettings(this.secretStore);
    } catch {
      this.alertSettingsState = { ...DEFAULT_ALERT_SETTINGS };
    }
    this.emitAlertSettings();
    const devices = await loadBoundDevices(this.secretStore);
    this.connState.transition("disconnected");
    diagnostics.log("services", `boot: ${devices.length} bound machine(s); awaiting selection`);
    return false;
  }

  // --- Turn-completion alerts ---

  /** Install (or clear) the presenter that renders turn-completion alerts.
   * Called by the app shell once native adapters exist. */
  setAlertPresenter(presenter: AlertPresenter | null): void {
    this.alertPresenter = presenter;
  }

  get alertSettings(): AlertSettings {
    return this.alertSettingsState;
  }

  subscribeAlertSettings(listener: (settings: AlertSettings) => void): () => void {
    this.alertSettingsListeners.add(listener);
    listener(this.alertSettingsState);
    return () => this.alertSettingsListeners.delete(listener);
  }

  async setAlertSettings(next: AlertSettings): Promise<void> {
    this.alertSettingsState = next;
    this.emitAlertSettings();
    try {
      await saveAlertSettings(this.secretStore, next);
    } catch (e) {
      diagnostics.log(
        "services",
        `alert settings persist failed: ${e instanceof Error ? e.message : String(e)}`,
      );
    }
  }

  private emitAlertSettings(): void {
    for (const l of this.alertSettingsListeners) l(this.alertSettingsState);
  }

  /** Forget every machine and the persisted session (Settings kill path). */
  async unbindAndClear(): Promise<void> {
    await clearAllBoundDevices(this.secretStore);
    await clearSession(this.secretStore);
    this.lastSessionKey = null;
    this.lastPeerDeviceId = null;
    this.activeBound = null;
  }

  setSubscriptionFromStatus(status: SubscriptionStatus): void {
    this.subscription = subscriptionStateFromStatus(status);
    this.onSubscriptionChange?.(this.subscription);
  }

  /** Surface pending permissions from the snapshot (the snapshot is the
   * source of the tool call_id, which is the permission_request_id on the
   * wire, and of the approval's `permission_options`). */
  private rescanPendingPermissions(): void {
    const snap = this.sessionStore.state;
    const pending = this.permissions.snapshot();
    const pendingIds = new Set(pending.map((p) => p.permissionRequestId));
    // A tool is awaiting a decision when it has no `permission_decision` and
    // offers something to decide: `permission_options` for a plain allow/deny
    // approval (Codex's run/apply_patch prompt) or `permission_input` questions
    // for a harness `user_question`. Filtering on `permission_input` alone hid
    // every Codex approval from the phone.
    const awaitingDecision = (snap?.tools ?? []).filter(isPendingPermissionTool);
    for (const tool of awaitingDecision) {
      if (pendingIds.has(tool.call_id)) continue;
      this.permissions.surface(
        tool.call_id,
        { name: tool.name, kind: tool.kind, id: tool.id, call_id: tool.call_id },
        tool.permission_input,
      );
    }
    // Dismiss approvals whose tool is no longer pending (resolved/denied).
    const stillPending = new Set(awaitingDecision.map((tool) => tool.call_id));
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
    this.sessionStore.beginSession(res.session_id);
    return res.session_id;
  }

  async switchSession(sessionId: string, workspaceRoot?: string | null) {
    // Wipe the held snapshot only when actually switching away: re-entering
    // the session that is already live must not clear it (that forced a full
    // re-fetch on every back-and-forth). If the PC's active session differs
    // (desktop user switched locally), the switch request still goes out and
    // the pushed Full snapshot replaces the stale view.
    if (this.sessionStore.state?.session.id !== sessionId) {
      this.sessionStore.beginSession(sessionId);
    }
    return this.controlClient().switchSession(sessionId, workspaceRoot ?? null);
  }

  /** Fetch and install the active session's full snapshot. Call after
   * switching, not on every pairing/list refresh.
   *
   * Incremental resume: when the caller did not force a full sync
   * (`force` — post-switch entry always wipes the store, so the held state is
   * empty there anyway) the held (session, revision) is offered to the PC. A
   * matching PC answers `up_to_date` and the snapshot transfer is skipped —
   * reconnects after a transient drop no longer re-pull the whole session. */
  async getState(expectedSessionId?: string): Promise<void> {
    const held = this.sessionStore.state;
    const known =
      !expectedSessionId && held !== null
        ? { sessionId: held.session.id, revision: held.revision }
        : undefined;
    const response = await this.controlClient().getState(known);
    if (response.up_to_date) {
      return;
    }
    if (!response.snapshot) {
      throw new Error("get_state response carried neither snapshot nor up_to_date");
    }
    if (expectedSessionId && response.snapshot.session.id !== expectedSessionId) {
      throw new Error("switched session state mismatch");
    }
    this.sessionStore.setSnapshot(response.snapshot);
  }

  async sendPrompt(text: string) {
    return this.controlClient().sendPrompt([
      { type: "text", text } as never,
    ]);
  }

  async cancel() {
    // The user cancelled on purpose: suppress the resulting interruption
    // alert (one-shot, short window — a later unrelated interruption still
    // alerts).
    this.turnWatcher.suppressNextInterruption();
    return this.controlClient().cancel();
  }

  async stopTool(toolCallId: string) {
    return this.controlClient().stopTool(toolCallId);
  }

  /** Fetch the full file change for a turn-change row (on-demand diff). */
  async getFileDiff(messageId: string, path: string) {
    const response = await this.controlClient().getFileDiff(messageId, path);
    return response.change ?? null;
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
    const generation = ++this.connectionGeneration;
    this.stopLoop = false;
    this.loopPromise = this.runLoop(generation).catch(() => {});
    return this.loopPromise;
  }

  /** Disconnect: stop the loop, clear the session, drop the session key. */
  async disconnect(): Promise<void> {
    this.stopLoop = true;
    this.stopHeartbeat();
    this.activeBound = null;
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
