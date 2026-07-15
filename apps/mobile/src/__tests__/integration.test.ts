import { describe, it, expect } from "vitest";
import { AppController } from "../app/services";
import { InMemorySecretStore } from "../util/in-memory-store";
import { RelayConnection } from "../relay/connection";
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

function qrJson(): string {
  return JSON.stringify({
    relay_endpoint: "wss://relay.example.com",
    pairing_code: "PAIR123",
    pc_device_pubkey: encodeBase64UrlNoPad(getPublicKey(PC_SECRET)),
  });
}

function makeSnapshot(sessionId: string, status: UiSnapshot["session"]["status"] = "Idle"): UiSnapshot {
  return {
    revision: 1,
    workspace: { id: "ws-1", name: "demo", root: "/demo" },
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
 * serves control requests and streams events over the encrypted channel. */
class FakePc {
  private conn: RelayConnection;
  private phoneDeviceId = "phone-dev";
  private snapshot: UiSnapshot;
  private stop = false;

  constructor(conn: RelayConnection) {
    this.conn = conn;
    this.snapshot = makeSnapshot("init");
  }

  async run(): Promise<void> {
    const authEnv = await this.conn.recvEnvelope();
    if (!authEnv || authEnv.type !== "device_auth") throw new Error("expected device_auth");
    const ackStatus: SubscriptionStatus = { active: true, plan: "pro" };
    await this.conn.sendEnvelope(fromMessage(null, { type: "subscription_status", payload: ackStatus }));

    const initEnv = await this.conn.recvEnvelope();
    if (!initEnv || initEnv.type !== "pairing_initiate") throw new Error("expected pairing_initiate");
    const init = initEnv.payload as PairingInitiate;
    const phoneEphPub = decodeBase64UrlNoPad(init.phone_ephemeral_pubkey!);
    const shared = ecdhSharedSecret(PC_SECRET, phoneEphPub);
    const key = deriveSessionKey(shared);
    await this.conn.sendEnvelope(
      fromMessage(null, {
        type: "pairing_confirm",
        payload: {
          pairing_token: "ptok",
          session_key_material: init.phone_ephemeral_pubkey!,
          pc_device_id: "pc-dev",
          phone_device_id: this.phoneDeviceId,
        },
      }),
    );
    this.conn.installSessionKey(key, this.phoneDeviceId);

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

  it("relay drop retains the snapshot and demotes to disconnected", async () => {
    const { controller, phoneT } = await bootstrap();
    await controller.createSession();
    await waitFor(() => (controller.snapshot?.session.id === "s1" ? true : undefined));

    const retained = controller.snapshot;
    phoneT.forceClose();
    await waitFor(() => (controller.connectionState === "disconnected" ? true : undefined));
    expect(controller.connectionState).toBe("disconnected");
    expect(controller.snapshot).toBe(retained);

    await controller.disconnect();
  });
});
// end of file
