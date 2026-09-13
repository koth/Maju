import { describe, it, expect } from "vitest";
import { AppController } from "../app/services";
import { InMemorySecretStore } from "../util/in-memory-store";
import { RelayConnection } from "../relay/connection";
import type { RelayTransport } from "../relay/transport";
import { linkedPair } from "./mock-relay";
import { fromMessage } from "../relay/framing";
import { getPublicKey, ecdhSharedSecret, deriveSessionKey } from "../crypto";
import { encodeBase64UrlNoPad, decodeBase64UrlNoPad } from "../util/base64url";
import type {
  PairingInitiate,
  ControlRequest,
  ControlResponse,
  EventFrame,
  SubscriptionStatus,
} from "../types/relay-protocol";
import type { UiSnapshot, ToolInvocation, WorkspaceSessionList } from "../types";

// End-to-end loopback harness: a fake PC (RelayConnection on the peer end of a
// linked channel transport) + the real AppController on the phone. Proves the
// full journey: scan -> pair -> E2E -> CreateSession -> SnapshotFull ->
// SendPrompt -> ToolUpdated stream -> SessionStatusChanged{Idle}, plus the
// permission round-trip, Cancel, ListSessions/SwitchSession, and reconnect
// resync. Mirrors the requirements-doc acceptance criteria.

const PC_SECRET = Uint8Array.from({ length: 32 }, (_, i) => 200 + i);
/** A second machine's static X25519 secret (multi-machine switch test). */
const PC2_SECRET = Uint8Array.from({ length: 32 }, (_, i) => 100 + i);

function qrJsonFor(secret: Uint8Array, pcName?: string, pcIp?: string): string {
  return JSON.stringify({
    relay_endpoint: "wss://relay.example.com",
    pairing_code: "PAIR123",
    pc_device_pubkey: encodeBase64UrlNoPad(getPublicKey(secret)),
    ...(pcName ? { pc_name: pcName } : {}),
    ...(pcIp ? { pc_ip: pcIp } : {}),
  });
}

function qrJson(): string {
  return qrJsonFor(PC_SECRET, "Studio-Mac", "192.168.1.24");
}

function makeSnapshot(
  sessionId: string,
  workspaceName = "demo",
  status: UiSnapshot["session"]["status"] = "Idle",
): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: workspaceName, root: "/demo" },
    workspace_connected: true,
    session: { id: sessionId, workspace_id: "ws-1", title: "Session", model: "m", mode: null, agent_cli: null, status },
    session_config: { hydrated: false, controls: [] },
    prompt_capabilities: { image: false, embedded_context: false, session_steer: false },
    available_commands: [],
    agent_plan: [],
    messages: [],
    timeline: [],
    tools: [],
    repository: { branch: "main", head: "abc", changed_files: [] },
    inspector_tab: "Activity",
    inspector_sections: [],
    session_changes: [],
    review_changes: [],
    turn_changes: [],
    thinking_status: null,
  };
}

function tool(callId: string, over: Partial<ToolInvocation> = {}): ToolInvocation {
  return {
    id: callId,
    call_id: callId,
    parent_call_id: null,
    name: "edit_file",
    kind: "edit",
    summary: "editing",
    status: "Running",
    is_subagent: false,
    detail_text: "",
    logs: [],
    diff_paths: [],
    diff_previews: [],
    raw_input: null,
    raw_output: null,
    terminal_output: null,
    error: null,
    permission_options: [],
    permission_input: null,
    permission_decision: null,
    can_stop: true,
    stop_kind: null,
    stop_status: null,
    ...over,
  };
}

async function tick(ms = 10): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFor<T>(fn: () => T | undefined | null, timeoutMs = 1500): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = fn();
    if (value !== undefined && value !== null) return value;
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await tick(5);
  }
}

/** Fake PC: acks DeviceAuth, completes the E2E handshake from PC_SECRET, then
 * serves control requests and streams events over the encrypted channel.
 * Options parametrize a second machine (own static key / device id / token). */
class FakePc {
  private conn: RelayConnection;
  private phoneDeviceId = "phone-dev";
  private readonly pcDeviceId: string;
  private readonly pairingToken: string;
  private readonly secret: Uint8Array;
  private readonly workspaceName: string;
  private snapshot: UiSnapshot;
  private stop = false;
  /** Handshake message types received from the phone, in order. */
  readonly handshakeTypes: string[] = [];
  /** GetState requests answered with the up_to_date short-circuit. */
  private getStateShortCircuits = 0;
  /** The last ResolvePermission request the phone sent (option id + request
   * id), so tests can assert what was actually approved. */
  lastResolve: { permission_request_id: string; option_id?: string | null } | null = null;

  get shortCircuitedGetStates(): number {
    return this.getStateShortCircuits;
  }

  constructor(
    conn: RelayConnection,
    opts: {
      pcDeviceId?: string;
      pairingToken?: string;
      secret?: Uint8Array;
      workspaceName?: string;
    } = {},
  ) {
    this.conn = conn;
    this.pcDeviceId = opts.pcDeviceId ?? "pc-dev";
    this.pairingToken = opts.pairingToken ?? "ptok";
    this.secret = opts.secret ?? PC_SECRET;
    this.workspaceName = opts.workspaceName ?? "demo";
    this.snapshot = makeSnapshot("init", this.workspaceName);
  }

  async run(): Promise<void> {
    const authEnv = await this.conn.recvEnvelope();
    if (!authEnv || authEnv.type !== "device_auth") throw new Error("expected device_auth");
    const ackStatus: SubscriptionStatus = { active: true, plan: "pro" };
    await this.conn.sendEnvelope(fromMessage(null, { type: "subscription_status", payload: ackStatus }));

    const initEnv = await this.conn.recvEnvelope();
    this.handshakeTypes.push(initEnv?.type ?? "null");
    // Accept either a fresh pairing or a resume (post-restart reconnect).
    if (initEnv && initEnv.type === "pairing_resume") {
      console.log("[fake-pc] got pairing_resume");
      const resume = initEnv.payload as { pairing_token: string; phone_ephemeral_pubkey: string };
      const phoneEphPub = decodeBase64UrlNoPad(resume.phone_ephemeral_pubkey);
      const shared = ecdhSharedSecret(this.secret, phoneEphPub);
      const key = deriveSessionKey(shared);
      await this.conn.sendEnvelope(
        fromMessage(null, {
          type: "pairing_confirm",
          payload: {
            pairing_token: resume.pairing_token,
            session_key_material: encodeBase64UrlNoPad(getPublicKey(this.secret)),
            pc_device_id: this.pcDeviceId,
            phone_device_id: this.phoneDeviceId,
          },
        }),
      );
      this.conn.installSessionKey(key, this.phoneDeviceId);
    } else {
    console.log("[fake-pc] got", initEnv?.type);
    if (!initEnv || initEnv.type !== "pairing_initiate") throw new Error("expected pairing_initiate");
    const init = initEnv.payload as PairingInitiate;
    const phoneEphPub = decodeBase64UrlNoPad(init.phone_ephemeral_pubkey!);
    const shared = ecdhSharedSecret(this.secret, phoneEphPub);
    const key = deriveSessionKey(shared);
    await this.conn.sendEnvelope(
      fromMessage(null, {
        type: "pairing_confirm",
        payload: {
          pairing_token: this.pairingToken,
          session_key_material: init.phone_ephemeral_pubkey!,
          pc_device_id: this.pcDeviceId,
          phone_device_id: this.phoneDeviceId,
        },
      }),
    );
    this.conn.installSessionKey(key, this.phoneDeviceId);
    }

    while (!this.stop) {
      const env = await this.conn.recvEnvelope();
      if (!env) return;
      if (env.type !== "control_request") continue;
      await this.handle(env.payload as ControlRequest);
    }
  }

  private async sendResponse(response: ControlResponse): Promise<void> {
    await this.conn.sendEnvelope(fromMessage(response.request_id, { type: "control_response", payload: response }));
  }

  private async pushEvent(frame: EventFrame): Promise<void> {
    await this.conn.sendEnvelope(fromMessage(null, { type: "event", payload: frame }));
  }

  private async handle(request: ControlRequest): Promise<void> {
    const requestId = request.request_id;
    if (request.op === "get_state") {
      // Mirror the real PC: when the phone reports the held (session,
      // revision) as still current, skip the snapshot transfer entirely.
      if (
        request.known_session_id === this.snapshot.session.id &&
        request.known_revision === this.snapshot.revision
      ) {
        this.getStateShortCircuits += 1;
        await this.sendResponse({ op: "get_state", request_id: requestId, up_to_date: true });
        return;
      }
      await this.sendResponse({ op: "get_state", request_id: requestId, snapshot: this.snapshot });
      return;
    }
    if (request.op === "create_session") {
      this.snapshot = makeSnapshot("s1", "Idle");
      await this.sendResponse({ op: "create_session", request_id: requestId, session_id: "s1" });
      await this.pushEvent({ kind: "snapshot_full", snapshot: this.snapshot });
      return;
    }
    if (request.op === "send_prompt") {
      await this.sendResponse({ op: "send_prompt", request_id: requestId });
      await this.pushEvent({ kind: "tool_updated", tool: tool("call-1", { status: "Running" }) });
      await this.pushEvent({ kind: "tool_updated", tool: tool("call-1", { status: "Succeeded", summary: "done" }) });
      await this.pushEvent({ kind: "session_status_changed", session_id: "s1", status: "Idle" });
      return;
    }
    if (request.op === "cancel") {
      await this.sendResponse({ op: "cancel", request_id: requestId });
      this.snapshot = { ...this.snapshot, session: { ...this.snapshot.session, status: "Idle" } };
      await this.pushEvent({ kind: "session_status_changed", session_id: "s1", status: "Idle" });
      return;
    }
    if (request.op === "stop_tool") {
      await this.sendResponse({ op: "stop_tool", request_id: requestId });
      await this.pushEvent({ kind: "tool_updated", tool: tool(request.tool_call_id, { status: "Interrupted" }) });
      return;
    }
    if (request.op === "list_sessions") {
      const group: WorkspaceSessionList = {
        workspace: { id: "ws-1", name: "demo", root: "/demo" },
        sessions: [{ id: "s1", title: "Session", status: "Idle", created_at: "", updated_at: "", message_count: 1 }],
        active_session_id: "s1",
        is_active: true,
        connected: true,
      };
      await this.sendResponse({ op: "list_sessions", request_id: requestId, sessions: [group] });
      return;
    }
    if (request.op === "switch_session") {
      this.snapshot = makeSnapshot("s1", "Idle");
      await this.sendResponse({ op: "switch_session", request_id: requestId });
      await this.pushEvent({ kind: "snapshot_full", snapshot: this.snapshot });
      return;
    }
    if (request.op === "resolve_permission") {
      this.lastResolve = {
        permission_request_id: request.permission_request_id,
        option_id: request.option_id ?? null,
      };
      await this.sendResponse({ op: "resolve_permission", request_id: requestId });
      await this.pushEvent({
        kind: "tool_updated",
        tool: tool(request.permission_request_id, { status: "Succeeded", permission_input: null, permission_decision: "allowed", permission_options: [], summary: "allowed" }),
      });
      return;
    }
  }

  /** Push a destructive permission request: a tool awaiting approval. */
  async requestPermission(callId: string): Promise<void> {
    const pending = tool(callId, {
      status: "Running",
      summary: "waiting for approval",
      permission_input: { questions: [{ id: "q1", header: "Allow", question: "Allow write?", is_other: false, is_secret: false, multi_select: false, options: [] }] },
      permission_options: [
        { id: "allow", label: "Allow once", kind: "allow" },
        { id: "deny", label: "Deny", kind: "deny" },
      ],
      permission_decision: null,
    });
    await this.pushEvent({ kind: "tool_updated", tool: pending });
  }

  /** Push a plain allow/deny approval the way the PC forwards Codex's
   * run/apply_patch prompt: a tool carrying `permission_options` and NO
   * `permission_input` (the PC sends no `permission_request` frame for it —
   * only the snapshot patch). */
  async requestApprovalWithoutInput(callId: string): Promise<void> {
    const pending = tool(callId, {
      name: "run_command",
      kind: "execute",
      status: "Running",
      summary: "waiting for approval",
      permission_input: null,
      permission_options: [
        { id: "approved", label: "Yes", kind: "allow_once" },
        { id: "abort", label: "No, provide feedback", kind: "reject_once" },
      ],
      permission_decision: null,
    });
    await this.pushEvent({ kind: "tool_updated", tool: pending });
  }

  stopLoop(): void {
    this.stop = true;
    void this.conn.close().catch(() => {});
  }
}

async function bootstrap() {
  const [phoneT, pcT] = linkedPair();
  const controller = new AppController(new InMemorySecretStore());
  const pc = new FakePc(new RelayConnection(pcT));
  const pcRun = pc.run();
  await controller.pairFromTransport(phoneT, qrJson(), false);
  return { controller, pc, pcRun, phoneT };
}

describe("integration: phone <-> fake PC over relay", () => {
  it("pairs E2E, creates a session, streams tool updates to Idle", async () => {
    const { controller, pc, pcRun } = await bootstrap();

    expect(controller.connectionState).toBe("connected");
    await controller.getState();
    expect(controller.snapshot?.session.id).toBe("init");

    const sessionId = await controller.createSession();
    expect(sessionId).toBe("s1");
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    await controller.sendPrompt("hello");
    await waitFor(() => {
      const tools = controller.snapshot?.tools ?? [];
      const status = controller.snapshot?.session.status;
      if (tools.some((t) => t.call_id === "call-1" && t.status === "Succeeded") && status === "Idle") return true;
      return undefined;
    });
    expect(controller.snapshot?.session.status).toBe("Idle");
    expect(controller.snapshot?.tools.find((t) => t.call_id === "call-1")?.status).toBe("Succeeded");

    pc.stopLoop();
    await controller.disconnect();
    await pcRun;
  });

  it("getState short-circuits when the held (session, revision) is still current", async () => {
    const { controller, pc, pcRun } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    // Reconnect path: the phone offers its held state; the PC agrees it is
    // current and the snapshot transfer is skipped entirely.
    const held = controller.snapshot;
    await controller.getState();
    expect(pc.shortCircuitedGetStates).toBe(1);
    expect(controller.snapshot).toBe(held);
    expect(controller.snapshot?.session.id).toBe("s1");

    pc.stopLoop();
    await controller.disconnect();
    await pcRun;
  });

  it("destructive permission: approve executes", async () => {
    const { controller, pc, pcRun } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    await pc.requestPermission("perm-1");
    await waitFor(() => controller.pendingApprovals.find((a) => a.permissionRequestId === "perm-1") ?? undefined);
    expect(controller.pendingApprovals.some((a) => a.permissionRequestId === "perm-1")).toBe(true);

    await controller.approvePermission("perm-1", "allow");
    await waitFor(() => {
      const found = controller.snapshot?.tools.find((t) => t.call_id === "perm-1");
      return found?.permission_decision === "allowed" ? true : undefined;
    });
    expect(controller.pendingApprovals).toHaveLength(0);

    pc.stopLoop();
    await controller.disconnect();
    await pcRun;
  });

  it("Codex-style approval without a question form surfaces and approves its option", async () => {
    // Regression: the phone only surfaced approvals that carried a
    // `permission_input` question form, so Codex's run/apply_patch prompts —
    // plain allow/deny options pushed as a tool update, with no
    // `permission_request` frame at all — were invisible and unanswerable.
    const { controller, pc, pcRun } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    await pc.requestApprovalWithoutInput("perm-codex");
    await waitFor(() =>
      controller.pendingApprovals.find((a) => a.permissionRequestId === "perm-codex") ?? undefined,
    );
    expect(controller.pendingApprovals.some((a) => a.permissionRequestId === "perm-codex")).toBe(true);

    await controller.approvePermission("perm-codex", "approved");
    expect(pc.lastResolve).toMatchObject({
      permission_request_id: "perm-codex",
      option_id: "approved",
    });

    pc.stopLoop();
    await controller.disconnect();
    await pcRun;
  });

  it("cancel returns to Idle; list + switch shows history", async () => {
    const { controller, pc, pcRun } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    await controller.cancel();
    await waitFor(() => (controller.snapshot?.session.status === "Idle" ? true : undefined));
    expect(controller.snapshot?.session.status).toBe("Idle");

    const res = await controller.listSessions();
    expect(res.sessions[0].sessions[0].id).toBe("s1");

    await controller.switchSession("s1");
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));
    expect(controller.snapshot?.session.id).toBe("s1");

    pc.stopLoop();
    await controller.disconnect();
    await pcRun;
  });

  it("relay drop retains the snapshot while reconnect keeps retrying", async () => {
    const { controller, phoneT } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    const retained = controller.snapshot;
    phoneT.forceClose();
    await waitFor(() => (controller.connectionState === "connecting" ? true : undefined));
    expect(controller.connectionState).toBe("connecting");
    expect(controller.snapshot).toBe(retained);

    await controller.disconnect();
  });

  it("cold start lands on the machines list; picking a machine resumes via pairing_resume", async () => {
    // First boot: pair.
    const store = new InMemorySecretStore();
    const [phoneT1, pcT1] = linkedPair();
    const controller1 = new AppController(store);
    const pc1 = new FakePc(new RelayConnection(pcT1));
    const pcRun1 = pc1.run();
    await controller1.pairFromTransport(phoneT1, qrJson(), false);
    expect(controller1.connectionState).toBe("connected");
    await controller1.getState();
    // NOTE: the persisted session key is deliberately KEPT. A restored key
    // must never be trusted for the cached fast path (the PC holds its copy
    // in memory only; any PC restart would make every fast-path frame
    // undecryptable and hang the phone until timeout), so the reconnect below
    // must run the explicit pairing_resume handshake.
    // Simulate app kill: tear down the transport + loop without clearing
    // the persisted store (identity, BoundDevice list, session key survive).
    await controller1.disconnect();
    pc1.stopLoop();
    await pcRun1;

    // Second boot with the same store: boot() must NOT auto-connect — the
    // machines list is shown and the user picks a machine to connect to.
    const [phoneT2, pcT2] = linkedPair();
    const controller2 = new AppController(store, async () => phoneT2);
    const pc2 = new FakePc(new RelayConnection(pcT2));
    const pcRun2 = pc2.run();

    const machines = await controller2.listMachines();
    expect(machines).toHaveLength(1);
    expect(machines[0].peer_device_id).toBe("pc-dev");

    const booted = await controller2.boot();
    expect(booted).toBe(false);
    expect(controller2.connectionState).toBe("disconnected");

    // The user taps the machine: DeviceAuth + pairing_resume with the
    // machine's own pairing token, then a GetState resync.
    await controller2.connectToBoundDevice("pc-dev");
    expect(controller2.connectionState).toBe("connected");
    // A cold start with a restored session key goes straight to
    // pairing_resume, never the stale-key fast path the PC silently drops.
    expect(pc2.handshakeTypes).toEqual(["pairing_resume"]);
    await waitFor(() => (controller2.snapshot?.session.id === "init" ? true : undefined));
    expect(controller2.snapshot?.session.id).toBe("init");

    await controller2.disconnect();
    pc2.stopLoop();
    await pcRun2;
  });

  it("binds several machines, connects to each, and unbinds one", async () => {
    const store = new InMemorySecretStore();
    let nextTransport: RelayTransport | null = null;
    const controller = new AppController(store, async () => {
      if (!nextTransport) throw new Error("no transport wired for this step");
      const t = nextTransport;
      nextTransport = null;
      return t;
    });

    // Scan machine A's QR (fresh pairing, direct transport).
    const [phoneA1, pcTA1] = linkedPair();
    const pcA1 = new FakePc(new RelayConnection(pcTA1));
    const runA1 = pcA1.run();
    await controller.pairFromTransport(phoneA1, qrJson(), false);
    expect(controller.connectionState).toBe("connected");
    // The active machine drives the switcher's "which PC" chip + green dot.
    expect(controller.activePeerDeviceId).toBe("pc-dev");
    // The PC names itself in its QR; that name must reach the stored record
    // (it is the only human-readable identifier the machines list has).
    const stored = (await controller.listMachines())[0];
    expect(stored.label).toBe("Studio-Mac");
    // …together with the PC's OWN address, never the relay host (which is the
    // same for every machine and made two rows indistinguishable).
    expect(stored.peer_ip).toBe("192.168.1.24");
    expect(stored.peer_ip).not.toBe("relay.example.com");
    await controller.disconnect();
    pcA1.stopLoop();
    await runA1;

    // Scan machine B's QR — a SECOND binding, not a replacement.
    const [phoneB1, pcTB1] = linkedPair();
    const pcB1 = new FakePc(new RelayConnection(pcTB1), {
      pcDeviceId: "pc-b",
      pairingToken: "ptok-b",
      secret: PC2_SECRET,
      workspaceName: "pc-b-ws",
    });
    const runB1 = pcB1.run();
    await controller.pairFromTransport(phoneB1, qrJsonFor(PC2_SECRET), false);
    expect(controller.connectionState).toBe("connected");
    await controller.disconnect();
    pcB1.stopLoop();
    await runB1;

    const machines = await controller.listMachines();
    expect(machines.map((m) => m.peer_device_id).sort()).toEqual(["pc-b", "pc-dev"]);
    // Each machine carries its own pairing token + static key.
    const a = machines.find((m) => m.peer_device_id === "pc-dev")!;
    const b = machines.find((m) => m.peer_device_id === "pc-b")!;
    expect(a.pairing_token).toBe("ptok");
    expect(b.pairing_token).toBe("ptok-b");
    expect(a.peer_static_pubkey_b64).not.toBe(b.peer_static_pubkey_b64);

    // Connect to A: resume handshake with A's token, A's snapshot arrives.
    const [phoneA2, pcTA2] = linkedPair();
    const pcA2 = new FakePc(new RelayConnection(pcTA2));
    const runA2 = pcA2.run();
    nextTransport = phoneA2;
    await controller.connectToBoundDevice("pc-dev");
    expect(controller.connectionState).toBe("connected");
    expect(controller.activePeerDeviceId).toBe("pc-dev");
    expect(pcA2.handshakeTypes).toEqual(["pairing_resume"]);
    await waitFor(() => (controller.snapshot?.session.id === "init" ? true : undefined));

    // Switch to B: A's cached key must not leak across machines — B runs its
    // own resume handshake and serves its own workspace snapshot.
    const [phoneB2, pcTB2] = linkedPair();
    const pcB2 = new FakePc(new RelayConnection(pcTB2), {
      pcDeviceId: "pc-b",
      pairingToken: "ptok-b",
      secret: PC2_SECRET,
      workspaceName: "pc-b-ws",
    });
    const runB2 = pcB2.run();
    nextTransport = phoneB2;
    await controller.connectToBoundDevice("pc-b");
    expect(controller.connectionState).toBe("connected");
    expect(controller.activePeerDeviceId).toBe("pc-b");
    expect(pcB2.handshakeTypes).toEqual(["pairing_resume"]);
    await waitFor(() => (controller.snapshot?.workspace.name === "pc-b-ws" ? true : undefined));
    expect(controller.snapshot?.workspace.name).toBe("pc-b-ws");

    // Unbind B while it is the active machine: list shrinks, connecting to it
    // now fails as unbound.
    const remaining = await controller.removeMachine("pc-b");
    expect(remaining.map((m) => m.peer_device_id)).toEqual(["pc-dev"]);
    expect(controller.connectionState).toBe("disconnected");
    // Unbinding the active machine clears the selection: no chip/green dot may
    // keep pointing at a machine this phone no longer holds.
    expect(controller.activePeerDeviceId).toBeNull();
    await expect(controller.connectToBoundDevice("pc-b")).rejects.toThrow(/not bound/);
    expect((await controller.listMachines()).map((m) => m.peer_device_id)).toEqual(["pc-dev"]);

    await controller.disconnect();
    pcA2.stopLoop();
    pcB2.stopLoop();
    await runA2;
    await runB2;
  });
});
// end of file
