import type { RelayConnection } from "../relay/connection";
import { fromMessage } from "../relay/framing";
import { uuidV4 } from "../util/uuid";
import type { ControlRequest, ControlResponse } from "../types/relay-protocol";
import type {
  UserPromptContent,
  PermissionInputResponse,
  AgentCliId,
  WorkspaceSessionList,
  UiSnapshot,
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
    private readonly requestTimeoutMs: number = 30_000,
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
  }): Promise<{ op: "create_session"; request_id: string; session_id: string }> {
    return this.send({
      op: "create_session",
      request_id: uuidV4(),
      workspace_root: opts?.workspace_root ?? null,
      agent: opts?.agent ?? null,
    }) as Promise<{ op: "create_session"; request_id: string; session_id: string }>;
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

  getState(): Promise<{ op: "get_state"; request_id: string; snapshot: UiSnapshot }> {
    return this.send({ op: "get_state", request_id: uuidV4() }) as Promise<{
      op: "get_state";
      request_id: string;
      snapshot: UiSnapshot;
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
}
// end of file
