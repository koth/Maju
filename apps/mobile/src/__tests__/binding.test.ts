import { describe, it, expect } from "vitest";
import {
  BOUND_DEVICE_KEY,
  persistBoundDevice,
  loadBoundDevice,
  clearBoundDevice,
  bindOutcomeFromResponse,
  canReconnectWithoutRescan,
  type BoundDevice,
} from "../account/binding";
import { InMemorySecretStore } from "../util/in-memory-store";
import {
  subscriptionStateFromStatus,
  NO_SUBSCRIPTION,
  demoteOnExpiry,
  type SubscriptionState,
} from "../account/subscription";
import type { BindDeviceResponse, SubscriptionStatus } from "../types/relay-protocol";

// Mirrors the relay_client binding/subscription tests: bind success persists
// the BoundDevice; a subscription-rejection surfaces SubscriptionRequired; an
// active+bound state can reconnect without re-scan; expiry demotes to free
// tier without killing the session.

function makeBound(): BoundDevice {
  return {
    device_id: "phone-dev",
    auth_token: "tok-abc",
    pairing_token: "ptok",
    peer_device_id: "pc-dev",
  };
}

describe("BoundDevice secure-storage persistence", () => {
  it("persists and reloads a bound device from the SecretStore", async () => {
    const store = new InMemorySecretStore();
    await persistBoundDevice(store, makeBound());
    const loaded = await loadBoundDevice(store);
    expect(loaded).not.toBeNull();
    expect(loaded).toEqual(makeBound());
  });

  it("returns null when no binding is stored", async () => {
    const store = new InMemorySecretStore();
    expect(await loadBoundDevice(store)).toBeNull();
  });

  it("clears a stored binding so a later load returns null", async () => {
    const store = new InMemorySecretStore();
    await persistBoundDevice(store, makeBound());
    expect(await loadBoundDevice(store)).not.toBeNull();
    await clearBoundDevice(store);
    expect(await loadBoundDevice(store)).toBeNull();
  });

  it("stores the binding under the constant key (separate from SessionKey)", async () => {
    const store = new InMemorySecretStore();
    await persistBoundDevice(store, makeBound());
    const raw = await store.get(BOUND_DEVICE_KEY);
    expect(raw).not.toBeNull();
    const decoded = JSON.parse(new TextDecoder().decode(raw!)) as BoundDevice;
    expect(decoded.auth_token).toBe("tok-abc");
    // SessionKey is never persisted here: the binding holds only the account
    // + pairing tokens (the E2E key is re-derived per pairing).
    expect("session_key" in decoded).toBe(false);
  });
});

describe("bindOutcomeFromResponse", () => {
  const bound = makeBound();

  it("maps an ok=true response to a bound outcome", () => {
    const response: BindDeviceResponse = {
      ok: true,
      bound_device_id: "phone-dev",
    };
    const outcome = bindOutcomeFromResponse(
      response,
      bound.auth_token,
      bound.pairing_token,
      bound.peer_device_id,
    );
    expect(outcome.kind).toBe("bound");
    if (outcome.kind === "bound") {
      expect(outcome.bound).toEqual(bound);
    }
  });

  it("maps a subscription-rejection to subscription_required", () => {
    const response: BindDeviceResponse = {
      ok: false,
      bound_device_id: "",
      message: "no active subscription",
    };
    const outcome = bindOutcomeFromResponse(
      response,
      bound.auth_token,
      bound.pairing_token,
      bound.peer_device_id,
    );
    expect(outcome.kind).toBe("subscription_required");
  });

  it("maps any other rejection to a failed outcome with the message", () => {
    const response: BindDeviceResponse = {
      ok: false,
      bound_device_id: "",
      message: "device limit reached",
    };
    const outcome = bindOutcomeFromResponse(
      response,
      bound.auth_token,
      bound.pairing_token,
      bound.peer_device_id,
    );
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.message).toBe("device limit reached");
    }
  });

  it("falls back to a generic message when the relay omits one", () => {
    const response: BindDeviceResponse = {
      ok: false,
      bound_device_id: "",
    };
    const outcome = bindOutcomeFromResponse(
      response,
      bound.auth_token,
      bound.pairing_token,
      bound.peer_device_id,
    );
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.message.length).toBeGreaterThan(0);
    }
  });
});

describe("canReconnectWithoutRescan", () => {
  const bound = makeBound();

  it("is true only when the subscription is active AND a binding exists", () => {
    expect(canReconnectWithoutRescan(true, bound)).toBe(true);
  });

  it("is false on the free tier (no binding) even if active", () => {
    expect(canReconnectWithoutRescan(true, null)).toBe(false);
  });

  it("is false when the subscription is inactive even if bound", () => {
    expect(canReconnectWithoutRescan(false, bound)).toBe(false);
  });

  it("is false for a free-tier unbound device", () => {
    expect(canReconnectWithoutRescan(false, null)).toBe(false);
  });
});

describe("subscription state", () => {
  it("maps a SubscriptionStatus to a client-side state", () => {
    const status: SubscriptionStatus = {
      active: true,
      plan: "pro",
      expires_at: 1_700_000_000_000,
    };
    expect(subscriptionStateFromStatus(status)).toEqual<SubscriptionState>({
      active: true,
      plan: "pro",
      expiresAt: 1_700_000_000_000,
    });
  });

  it("NO_SUBSCRIPTION is the inactive free-tier default", () => {
    expect(NO_SUBSCRIPTION).toEqual<SubscriptionState>({
      active: false,
      plan: null,
      expiresAt: null,
    });
  });

  it("demoteOnExpiry flags re-scan when an active subscription lapses", () => {
    const current: SubscriptionState = {
      active: true,
      plan: "pro",
      expiresAt: 1_700_000_000_000,
    };
    const pushed: SubscriptionStatus = { active: false };
    const { state, mustRescan } = demoteOnExpiry(current, pushed);
    expect(state.active).toBe(false);
    expect(mustRescan).toBe(true);
  });

  it("demoteOnExpiry does not flag re-scan on an inactive->inactive transition", () => {
    const { mustRescan } = demoteOnExpiry(NO_SUBSCRIPTION, {
      active: false,
    });
    expect(mustRescan).toBe(false);
  });

  it("demoteOnExpiry does not flag re-scan when a subscription activates", () => {
    const { mustRescan } = demoteOnExpiry(NO_SUBSCRIPTION, {
      active: true,
      plan: "pro",
    });
    expect(mustRescan).toBe(false);
  });
});
// end of file
