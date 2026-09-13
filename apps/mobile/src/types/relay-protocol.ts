// TypeScript mirror of `crates/relay-protocol` wire types. Field names and
// discriminator strings match the Rust serde output exactly
// (`#[serde(rename="type")]`, `#[serde(tag="op"/"kind", rename_all="snake_case")]`,
// `#[serde(tag="type", content="payload", rename_all="snake_case")]` for Message).

import type {
  PermissionInputRequest,
  PermissionInputResponse,
  SessionFileChange,
  SessionStatus,
  ToolInvocation,
  UiSnapshot,
  UiSnapshotPatch,
  UserPromptContent,
  WorkspaceSessionList,
  AgentCliId,
} from "./index";

export const PROTO_VERSION = 1 as const;

/** Wire capability advertised at pairing time and echoed via PairingConfirm.
 * Mirrors relay_protocol::pairing::CAPABILITY_CIPHERTEXT_B64: a peer that
 * advertises it may receive base64url-no-pad `ciphertext_b64` payloads
 * (~1.33 chars/byte) instead of the legacy number-array `ciphertext`
 * (~4 chars/byte). */
export const CAPABILITY_CIPHERTEXT_B64 = "ciphertext_b64" as const;

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
  /** Legacy payload encoding: the ciphertext bytes as a JSON number array
   * (~4 chars/byte). Kept for old-peer compatibility on the emit path. */
  ciphertext?: number[];
  /** Preferred payload encoding: the ciphertext bytes as base64url-no-pad
   * (~1.33 chars/byte). PCs emit this when the phone advertises the
   * `ciphertext_b64` capability at pairing time. */
  ciphertext_b64?: string;
  /** Present when this frame is one fragment of a larger encrypted payload. */
  chunk_id?: string;
  chunk_index?: number;
  chunk_total?: number;
  /** Payload encoding applied BEFORE encryption (e.g. `"gzip"`). Absent for
   * small payloads sent as raw serialized JSON. Chunks inherit the value
   * from the original envelope so reassembly can restore it. */
  encoding?: string;
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
  | {
      op: "get_state";
      request_id: string;
      /** Held (active session id, revision). When both still match the PC's
       * active state the PC answers `up_to_date` instead of re-sending the
       * full snapshot. Omit for a first sync (always Full). */
      known_session_id?: string;
      known_revision?: number;
    }
  | {
      op: "resolve_permission";
      request_id: string;
      permission_request_id: string;
      option_id?: string | null;
      guidance?: string | null;
      input_response?: PermissionInputResponse | null;
    }
  | { op: "cancel"; request_id: string }
  | { op: "stop_tool"; request_id: string; tool_call_id: string }
  | { op: "get_file_diff"; request_id: string; message_id: string; path: string };

export type ControlResponse =
  | { op: "list_sessions"; request_id: string; sessions: WorkspaceSessionList[] }
  | { op: "create_session"; request_id: string; session_id: string }
  | { op: "switch_session"; request_id: string }
  | { op: "send_prompt"; request_id: string }
  | {
      op: "get_state";
      request_id: string;
      /** Absent when `up_to_date` is true — the held snapshot is still
       * current and nothing needed to cross the relay. */
      snapshot?: UiSnapshot;
      up_to_date?: boolean;
    }
  | { op: "resolve_permission"; request_id: string }
  | { op: "cancel"; request_id: string }
  | { op: "stop_tool"; request_id: string }
  | { op: "file_diff"; request_id: string; change?: SessionFileChange | null }
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
  /** Friendly name of the PC (its hostname), when the PC sent one. PCs that
   * predate this field omit it, so treat it as optional. */
  pc_name?: string;
}
export interface PairingInitiate {
  pairing_code: string;
  pc_device_pubkey: string;
  relay_endpoint: string;
  phone_ephemeral_pubkey?: string | null;
  /** Sender wire capabilities forwarded to the peer via PairingConfirm. */
  capabilities?: string[];
}
export interface PairingConfirm {
  /** Present when the relay rejected the pairing/resume (e.g. invalid or
   * expired code, PC offline). Success path omits this field. */
  error?: string;
  pairing_token: string;
  session_key_material: string;
  pc_device_id: string;
  phone_device_id: string;
  /** Wire capabilities the peers agreed on (echoed from the phone's
   * PairingResume/PairingInitiate capabilities). */
  capabilities?: string[];
}
export interface PairingResume {
  pairing_token: string;
  phone_ephemeral_pubkey: string;
  /** Sender wire capabilities forwarded to the peer via PairingConfirm. */
  capabilities?: string[];
}
export interface PairingRegister {
  pairing_code: string;
}
export interface DeviceAuth {
  device_id: string;
  /** Ed25519 verifying key (base64url-no-pad). Optional for legacy peers. */
  device_pubkey?: string;
  /** Ed25519 signature over `{device_id}:{timestamp_ms}`, base64url-no-pad. */
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
/** Device -> relay -> peer: the sender cannot decrypt the peer's traffic, so
 * the peer should drop its session key and re-run its resume handshake. */
export interface PeerSessionReset {}

/** Typed view of an `Envelope` payload, reached via `intoMessage`. */
export type Message =
  | { type: "control_request"; payload: ControlRequest }
  | { type: "control_response"; payload: ControlResponse }
  | { type: "event"; payload: EventFrame }
  | { type: "pairing_initiate"; payload: PairingInitiate }
  | { type: "pairing_confirm"; payload: PairingConfirm }
  | { type: "pairing_resume"; payload: PairingResume }
  | { type: "pairing_register"; payload: PairingRegister }
  | { type: "peer_session_reset"; payload: PeerSessionReset }
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
