import type { RelayConnection } from "../relay/connection";
import { fromMessage } from "../relay/framing";
import { uuidV4 } from "../util/uuid";
import type { ControlRequest, ControlResponse } from "../types/relay-protocol";
import type {
  UserPromptContent,
  PermissionInputResponse,
  AgentCliId,
  AgentOptionsList,
  SessionConfigState,
  WorkspaceSessionList,
  UiSnapshot,
  SessionFileChange,
} from "../types";

interface Pending {
  resolve: (r: ControlResponse) => void;
  reject: (e: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

// Drives ControlRequest/ControlResponse with request_id matching. Each outbound
// request carries a fresh request_id; the matching ControlResponse (which echoes
// it) resolves the pending promise. Unsolicited EventFrames are routed by the
// driver, not here. Mirrors the phone side of relay_client::driver.
export class ControlClient {
  private pending = new Map<string, Pending>();

  constructor(
    private readonly conn: RelayConnection,
    // Session switching can be slow when the PC has to load a cold session
    // (SQLite history, ACP/dsh runtime startup, remote bootstrap). 10s was
    // too short and the phone would give up with "control request timeout"
    // while the desktop was still working. Keep the same 60s convention as
    // the desktop SDK so slow-but-legitimate operations have time to finish.
    private readonly requestTimeoutMs: number = 60_000,
  ) {}

  /** Send a control request and await its matching response. */
  async send(request: ControlRequest): Promise<ControlResponse> {
    const requestId = request.request_id;
    const promise = new Promise<ControlResponse>((resolve, reject) => {
      const timer = setTimeout(
        () => {
          if (this.pending.delete(requestId)) {
            reject(new Error(`control request timeout: ${requestId}`));
          }
        },
        this.requestTimeoutMs,
      );
      this.pending.set(requestId, { resolve, reject, timer });
    });
    const env = fromMessage(requestId, {
      type: "control_request",
      payload: request,
    });
    await this.conn.sendEnvelope(env);
    return promise;
  }

  /** Route an inbound ControlResponse to its pending request. Returns true if
   * a pending request was resolved (false for stray/duplicate responses). */
  dispatchResponse(response: ControlResponse): boolean {
    const entry = this.pending.get(response.request_id);
    if (!entry) return false;
    clearTimeout(entry.timer);
    this.pending.delete(response.request_id);
    if (response.op === "error") {
      entry.reject(new Error(response.message));
    } else {
      entry.resolve(response);
    }
    return true;
  }

  /** Reject every pending request immediately (connection lost / session
   * reset). Without this, callers would wait out the full request timeout
   * against a dead socket before the reconnect ladder kicks in. */
  failAll(reason: string): void {
    for (const [, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(new Error(reason));
    }
    this.pending.clear();
  }

  // --- Op builders (task 6.2). Each returns the typed response. ---

  listSessions(): Promise<{ op: "list_sessions"; request_id: string; sessions: WorkspaceSessionList[] }> {
    return this.send({ op: "list_sessions", request_id: uuidV4() }) as Promise<{
      op: "list_sessions";
      request_id: string;
      sessions: WorkspaceSessionList[];
    }>;
  }

  createSession(opts?: {
    workspace_root?: string | null;
    agent?: AgentCliId | null;
    preset?: string | null;
  }): Promise<{ op: "create_session"; request_id: string; session_id: string }> {
    return this.send({
      op: "create_session",
      request_id: uuidV4(),
      workspace_root: opts?.workspace_root ?? null,
      agent: opts?.agent ?? null,
      preset: opts?.preset ?? null,
    }) as Promise<{ op: "create_session"; request_id: string; session_id: string }>;
  }

  /** Fetch the agent/preset choices for the new-session picker. The preset
   *  half is best-effort on the PC (it may spawn the dsh host), so expect
   *  this to take a moment and `dsh_presets` to come back empty when the
   *  harness is unavailable. */
  listAgentOptions(): Promise<{
    op: "agent_options";
    request_id: string;
    options: AgentOptionsList;
  }> {
    return this.send({ op: "list_agent_options", request_id: uuidV4() }) as Promise<{
      op: "agent_options";
      request_id: string;
      options: AgentOptionsList;
    }>;
  }

  /** Set a session config control (e.g. the model picker) on the active
   *  session. The PC answers with the refreshed config state; a snapshot
   *  patch carrying the same state follows on the event stream. */
  setConfigControl(
    controlId: string,
    valueId: string,
    provider?: string | null,
  ): Promise<{ op: "set_config_control"; request_id: string; config: SessionConfigState }> {
    return this.send({
      op: "set_config_control",
      request_id: uuidV4(),
      control_id: controlId,
      value_id: valueId,
      provider: provider ?? null,
    }) as Promise<{ op: "set_config_control"; request_id: string; config: SessionConfigState }>;
  }

  switchSession(
    sessionId: string,
    workspaceRoot?: string | null,
  ): Promise<{ op: "switch_session"; request_id: string }> {
    return this.send({
      op: "switch_session",
      request_id: uuidV4(),
      session_id: sessionId,
      workspace_root: workspaceRoot ?? null,
    }) as Promise<{ op: "switch_session"; request_id: string }>;
  }

  sendPrompt(prompt: UserPromptContent[]): Promise<{ op: "send_prompt"; request_id: string }> {
    return this.send({ op: "send_prompt", request_id: uuidV4(), prompt }) as Promise<{
      op: "send_prompt";
      request_id: string;
    }>;
  }

  /** Fetch the active session's state. `known` carries the held (session,
   * revision); when the PC still matches, it answers up_to_date and skips the
   * whole-snapshot transfer. */
  getState(known?: { sessionId: string; revision: number }): Promise<{
    op: "get_state";
    request_id: string;
    snapshot?: UiSnapshot;
    up_to_date?: boolean;
  }> {
    return this.send({
      op: "get_state",
      request_id: uuidV4(),
      ...(known
        ? {
            known_session_id: known.sessionId,
            known_revision: known.revision,
          }
        : {}),
    }) as Promise<{
      op: "get_state";
      request_id: string;
      snapshot?: UiSnapshot;
      up_to_date?: boolean;
    }>;
  }

  resolvePermission(opts: {
    permission_request_id: string;
    option_id?: string | null;
    guidance?: string | null;
    input_response?: PermissionInputResponse | null;
  }): Promise<{ op: "resolve_permission"; request_id: string }> {
    return this.send({
      op: "resolve_permission",
      request_id: uuidV4(),
      permission_request_id: opts.permission_request_id,
      option_id: opts.option_id ?? null,
      guidance: opts.guidance ?? null,
      input_response: opts.input_response ?? null,
    }) as Promise<{ op: "resolve_permission"; request_id: string }>;
  }

  cancel(): Promise<{ op: "cancel"; request_id: string }> {
    return this.send({ op: "cancel", request_id: uuidV4() }) as Promise<{
      op: "cancel";
      request_id: string;
    }>;
  }

  stopTool(toolCallId: string): Promise<{ op: "stop_tool"; request_id: string }> {
    return this.send({
      op: "stop_tool",
      request_id: uuidV4(),
      tool_call_id: toolCallId,
    }) as Promise<{ op: "stop_tool"; request_id: string }>;
  }

  /** Fetch the full file change (old/new text) for one file of one turn.
   * `change` is undefined when the desktop can no longer resolve the turn
   * (e.g. the turn predates the desktop process). */
  getFileDiff(
    messageId: string,
    path: string,
  ): Promise<{ op: "file_diff"; request_id: string; change?: SessionFileChange | null }> {
    return this.send({
      op: "get_file_diff",
      request_id: uuidV4(),
      message_id: messageId,
      path,
    }) as Promise<{ op: "file_diff"; request_id: string; change?: SessionFileChange | null }>;
  }
}
// end of file
