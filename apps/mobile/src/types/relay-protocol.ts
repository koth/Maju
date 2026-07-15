// TypeScript mirror of `crates/relay-protocol` wire types. Field names and
// discriminator strings match the Rust serde output exactly
// (`#[serde(rename="type")]`, `#[serde(tag="op"/"kind", rename_all="snake_case")]`,
// `#[serde(tag="type", content="payload", rename_all="snake_case")]` for Message).

import type {
  PermissionInputRequest,
  PermissionInputResponse,
  SessionStatus,
  ToolInvocation,
  UiSnapshot,
  UiSnapshotPatch,
  UserPromptContent,
  WorkspaceSessionList,
  AgentCliId,
} from "./index";

export const PROTO_VERSION = 1 as const;

/** The raw wire frame exchanged between PC, relay, and phone. */
export interface Envelope {
  proto_version: number;
  id?: string | null;
  type: string;
  payload: unknown;
}

/** Outer relay-routing shape wrapping a serialized, encrypted `Envelope`. */
export interface EncryptedEnvelope {
  to_device_id: string;
  nonce: number[];
  ciphertext: number[];
}

// --- Control requests (internally tagged by `op`, snake_case) ---

export type ControlRequest =
  | { op: "list_sessions"; request_id: string }
  | {
      op: "create_session";
      request_id: string;
      workspace_root?: string | null;
      agent?: AgentCliId | null;
    }
  | {
      op: "switch_session";
      request_id: string;
      session_id: string;
      workspace_root?: string | null;
    }
  | { op: "send_prompt"; request_id: string; prompt: UserPromptContent[] }
  | { op: "get_state"; request_id: string }
  | {
      op: "resolve_permission";
      request_id: string;
      permission_request_id: string;
      option_id?: string | null;
      guidance?: string | null;
      input_response?: PermissionInputResponse | null;
    }
  | { op: "cancel"; request_id: string }
  | { op: "stop_tool"; request_id: string; tool_call_id: string };

export type ControlResponse =
  | { op: "list_sessions"; request_id: string; sessions: WorkspaceSessionList[] }
  | { op: "create_session"; request_id: string; session_id: string }
  | { op: "switch_session"; request_id: string }
  | { op: "send_prompt"; request_id: string }
  | { op: "get_state"; request_id: string; snapshot: UiSnapshot }
  | { op: "resolve_permission"; request_id: string }
  | { op: "cancel"; request_id: string }
  | { op: "stop_tool"; request_id: string }
  | { op: "error"; request_id: string; message: string };

// --- Event frames (internally tagged by `kind`, snake_case; no request_id) ---

export type EventFrame =
  | { kind: "snapshot_full"; snapshot: UiSnapshot }
  | { kind: "snapshot_patch"; patch: UiSnapshotPatch }
  | { kind: "permission_request"; request: PermissionInputRequest }
  | { kind: "tool_updated"; tool: ToolInvocation }
  | { kind: "session_status_changed"; session_id: string; status: SessionStatus };

// --- Pairing / auth / binding / subscription messages ---

export interface PairingQrPayload {
  relay_endpoint: string;
  pairing_code: string;
  pc_device_pubkey: string;
}
export interface PairingInitiate {
  pairing_code: string;
  pc_device_pubkey: string;
  relay_endpoint: string;
  phone_ephemeral_pubkey?: string | null;
}
export interface PairingConfirm {
  pairing_token: string;
  session_key_material: string;
  pc_device_id: string;
  phone_device_id: string;
}
export interface PairingRegister {
  pairing_code: string;
}
export interface DeviceAuth {
  device_id: string;
  signature: string;
  timestamp_ms: number;
}
export interface BindDeviceRequest {
  auth_token: string;
}
export interface BindDeviceResponse {
  ok: boolean;
  bound_device_id: string;
  message?: string | null;
}
export interface SubscriptionStatus {
  active: boolean;
  plan?: string | null;
  expires_at?: number | null;
}

/** Typed view of an `Envelope` payload, reached via `intoMessage`. */
export type Message =
  | { type: "control_request"; payload: ControlRequest }
  | { type: "control_response"; payload: ControlResponse }
  | { type: "event"; payload: EventFrame }
  | { type: "pairing_initiate"; payload: PairingInitiate }
  | { type: "pairing_confirm"; payload: PairingConfirm }
  | { type: "pairing_register"; payload: PairingRegister }
  | { type: "device_auth"; payload: DeviceAuth }
  | { type: "bind_device_request"; payload: BindDeviceRequest }
  | { type: "bind_device_response"; payload: BindDeviceResponse }
  | { type: "subscription_status"; payload: SubscriptionStatus }
  | { type: string; payload: unknown };

/** Helper: extract the discriminator + payload from a Message Value. */
export function splitMessage(value: unknown): { type: string; payload: unknown } {
  if (typeof value === "object" && value !== null) {
    const obj = value as Record<string, unknown>;
    const type = typeof obj.type === "string" ? obj.type : "unknown";
    const payload = "payload" in obj ? obj.payload : null;
    return { type, payload };
  }
  return { type: "unknown", payload: value };
}
