import type { BindDeviceResponse } from "../types/relay-protocol";
import type { SecretStore } from "../crypto/identity";

// Persisted binding records. Stored in secure storage when a pairing succeeds
// so a restart can reconnect to ANY of the bound machines without re-scanning.
// `auth_token` is the account token; `pairing_token` is the per-pairing device
// token from the relay (one fresh token per scanned QR — the relay keeps one
// pairing row per scan, so a phone may hold several). Neither is the E2E
// SessionKey (that is re-derived per resume). Mirrors relay_client::binding.
export interface BoundDevice {
  device_id: string;
  auth_token: string;
  pairing_token: string;
  peer_device_id: string;
  /** PC static X25519 public key (base64url-no-pad) for resume derivation.
   * Optional for legacy bound records created before this field existed. */
  peer_static_pubkey_b64?: string;
  /** Relay endpoint from the original scan, used to redial this machine. */
  relay_endpoint?: string;
  /** The PC's OWN network address as reported in its QR (never the relay
   * host, which is identical for every machine). Optional: records bound
   * before this field existed, and PCs that do not report one. */
  peer_ip?: string;
  /** Friendly machine name shown in the machines list (optional). */
  label?: string;
  /** Epoch ms when this machine was bound (optional, for the list). */
  bound_at?: number;
}

/** Legacy single-record key (pre-multi-machine). Migrated on first load. */
export const BOUND_DEVICE_KEY = "kodex.bound-device";
/** Current storage: one JSON array of BoundDevice under a single key. */
export const BOUND_DEVICES_KEY = "kodex.bound-devices";

function toJson(devices: BoundDevice[]): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(devices));
}

function fromJson(bytes: Uint8Array): BoundDevice[] {
  const parsed = JSON.parse(new TextDecoder().decode(bytes));
  return Array.isArray(parsed) ? (parsed as BoundDevice[]) : [];
}

function isBoundDevice(value: unknown): value is BoundDevice {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.pairing_token === "string" &&
    typeof v.peer_device_id === "string" &&
    v.pairing_token.length > 0 &&
    v.peer_device_id.length > 0
  );
}

/**
 * Load every bound machine. Migrates the legacy single-record storage
 * (`kodex.bound-device`) into the list on first read, then removes the
 * legacy key so both stores never disagree.
 */
export async function loadBoundDevices(
  store: SecretStore,
): Promise<BoundDevice[]> {
  const bytes = await store.get(BOUND_DEVICES_KEY);
  if (bytes) {
    try {
      const devices = fromJson(bytes).filter(isBoundDevice);
      // Dedupe by peer_device_id (last write wins) to stay idempotent.
      const byPeer = new Map<string, BoundDevice>();
      for (const d of devices) byPeer.set(d.peer_device_id, d);
      return [...byPeer.values()];
    } catch {
      // Corrupt list: fall through to legacy migration / empty.
    }
  }
  const legacyBytes = await store.get(BOUND_DEVICE_KEY);
  if (!legacyBytes) return [];
  try {
    const legacy = JSON.parse(
      new TextDecoder().decode(legacyBytes),
    ) as BoundDevice;
    if (!isBoundDevice(legacy)) {
      await store.delete(BOUND_DEVICE_KEY);
      return [];
    }
    const migrated: BoundDevice[] = [legacy];
    await store.set(BOUND_DEVICES_KEY, toJson(migrated));
    await store.delete(BOUND_DEVICE_KEY);
    return migrated;
  } catch {
    await store.delete(BOUND_DEVICE_KEY);
    return [];
  }
}

export async function saveBoundDevices(
  store: SecretStore,
  devices: BoundDevice[],
): Promise<void> {
  await store.set(BOUND_DEVICES_KEY, toJson(devices));
}

/** Insert or update a machine (keyed by `peer_device_id`), newest position last. */
export async function upsertBoundDevice(
  store: SecretStore,
  bound: BoundDevice,
): Promise<BoundDevice[]> {
  const devices = await loadBoundDevices(store);
  const byPeer = new Map(devices.map((d) => [d.peer_device_id, d] as const));
  byPeer.set(bound.peer_device_id, bound);
  const next = [...byPeer.values()];
  await saveBoundDevices(store, next);
  return next;
}

/** Remove one machine by its PC device id. Returns the remaining list. */
export async function removeBoundDevice(
  store: SecretStore,
  peerDeviceId: string,
): Promise<BoundDevice[]> {
  const devices = await loadBoundDevices(store);
  const next = devices.filter((d) => d.peer_device_id !== peerDeviceId);
  await saveBoundDevices(store, next);
  return next;
}

/** Forget every machine (legacy key included). */
export async function clearAllBoundDevices(store: SecretStore): Promise<void> {
  await store.delete(BOUND_DEVICES_KEY);
  await store.delete(BOUND_DEVICE_KEY);
}

export type BindOutcome =
  | { kind: "bound"; bound: BoundDevice }
  | { kind: "subscription_required" }
  | { kind: "failed"; message: string };

/** Map a BindDeviceResponse to a BindOutcome. The relay rejects binds without
 * an active subscription; the client surfaces this so the UI can prompt to
 * subscribe. Mirrors relay_client::binding::BindOutcome::from_response. */
export function bindOutcomeFromResponse(
  response: BindDeviceResponse,
  authToken: string,
  pairingToken: string,
  peerDeviceId: string,
  peerStaticPubkeyB64?: string,
): BindOutcome {
  if (response.ok) {
    return {
      kind: "bound",
      bound: {
        device_id: response.bound_device_id,
        auth_token: authToken,
        pairing_token: pairingToken,
        peer_device_id: peerDeviceId,
        peer_static_pubkey_b64: peerStaticPubkeyB64,
      },
    };
  }
  const message = response.message ?? "bind rejected";
  if (message.toLowerCase().includes("subscription")) {
    return { kind: "subscription_required" };
  }
  return { kind: "failed", message };
}

/**
 * Whether reconnect can use stored credentials. Bound + active subscription
 * => reconnect without re-scan. Free tier (no binding) or expired => re-scan.
 * Mirrors SubscriptionState::can_reconnect_without_rescan.
 */
export function canReconnectWithoutRescan(
  subscriptionActive: boolean,
  bound: BoundDevice | null,
): boolean {
  return subscriptionActive && bound !== null;
}
// end of file