import type { BindDeviceResponse } from "../types/relay-protocol";
import type { SecretStore } from "../crypto/identity";

// Persisted binding record. Stored in secure storage when bind succeeds so a
// restart can reconnect without re-scanning. `auth_token` is the account token;
// `pairing_token` is the device pairing token from the relay. Neither is the
// E2E SessionKey (that is re-derived per pairing). Mirrors relay_client::binding.
export interface BoundDevice {
  device_id: string;
  auth_token: string;
  pairing_token: string;
  peer_device_id: string;
}

export const BOUND_DEVICE_KEY = "kodex.bound-device";

function toBytes(value: BoundDevice): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

function fromBytes(bytes: Uint8Array): BoundDevice {
  return JSON.parse(new TextDecoder().decode(bytes)) as BoundDevice;
}

export async function persistBoundDevice(
  store: SecretStore,
  bound: BoundDevice,
): Promise<void> {
  await store.set(BOUND_DEVICE_KEY, toBytes(bound));
}

export async function loadBoundDevice(
  store: SecretStore,
): Promise<BoundDevice | null> {
  const bytes = await store.get(BOUND_DEVICE_KEY);
  if (!bytes) return null;
  return fromBytes(bytes);
}

export async function clearBoundDevice(store: SecretStore): Promise<void> {
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
): BindOutcome {
  if (response.ok) {
  return {
  kind: "bound",
  bound: {
  device_id: response.bound_device_id,
  auth_token: authToken,
  pairing_token: pairingToken,
  peer_device_id: peerDeviceId,
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
