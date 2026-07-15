import type { SubscriptionStatus } from "../types/relay-protocol";

// Client-side subscription state, updated by relay-pushed SubscriptionStatus.
// Drives the UI and the reconnect strategy (bound + active => reconnect with
// credentials; inactive or free => require re-scan). Mirrors relay_client.

export interface SubscriptionState {
  active: boolean;
  plan: string | null;
  expiresAt: number | null;
}

export function subscriptionStateFromStatus(status: SubscriptionStatus): SubscriptionState {
  return {
  active: status.active,
  plan: status.plan ?? null,
  expiresAt: status.expires_at ?? null,
  };
}

export const NO_SUBSCRIPTION: SubscriptionState = {
  active: false,
  plan: null,
  expiresAt: null,
};

/**
 * On expiry (active=false), demote to free-tier re-scan semantics WITHOUT
 * killing an in-progress session. Returns the demoted state and a flag the
 * UI uses to prompt the user to re-scan on the next pairing.
 */
export function demoteOnExpiry(
  current: SubscriptionState,
  pushed: SubscriptionStatus,
): { state: SubscriptionState; mustRescan: boolean } {
  const next = subscriptionStateFromStatus(pushed);
  const mustRescan = current.active && !next.active;
  return { state: next, mustRescan };
}
// end of file
