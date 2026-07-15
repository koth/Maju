import { describe, it, expect } from "vitest";
import { RelayConnection } from "../relay/connection";
import { linkedPair } from "./mock-relay";
import { ConnectionStateMachine } from "../relay/state-machine";
import { nextBackoffDelay, BACKOFF_MAX_MS } from "../relay/backoff";
import { fromMessage } from "../relay/framing";
import { deriveSessionKey } from "../crypto";
import { PROTO_VERSION, type Envelope } from "../types/relay-protocol";

describe("connection state machine", () => {
  it("surfaces transitions to subscribers", () => {
  const sm = new ConnectionStateMachine();
  const seen: string[] = [];
  sm.subscribe((s) => seen.push(s));
  sm.transition("connecting");
  sm.transition("authenticating");
  sm.transition("connected");
  expect(seen).toEqual(["disconnected", "connecting", "authenticating", "connected"]);
  });
});

describe("backoff", () => {
  it("escalates and caps at the max", () => {
  const seq = Array.from({ length: 12 }, (_, i) => nextBackoffDelay(i, 2_000, 60_000, () => 0));
  expect(seq[0]).toBeLessThanOrEqual(2_000);
 expect(seq[6]).toBeGreaterThan(seq[0]);
  for (const d of seq) expect(d).toBeLessThanOrEqual(BACKOFF_MAX_MS);
  });
});

describe("relay connection E2E + auth", () => {
  it("e2e envelope round-trips through the passthrough pair", async () => {
  const [phoneT, pcT] = linkedPair();
  const phone = new RelayConnection(phoneT, 30_000);
  const pc = new RelayConnection(pcT, 30_000);
  const key = deriveSessionKey(new Uint8Array(32).fill(9));
  phone.installSessionKey(key, "pc-device-id");
  pc.installSessionKey(key, "phone-device-id");
  const rid = "22222222-3333-4444-8555-666666666666";
  const env = fromMessage(rid, {
  type: "control_request",
  payload: { op: "cancel", request_id: rid },
  });
  await phone.sendEnvelope(env);
  const got = await pc.recvEnvelope();
  expect(got).toEqual(env);
  });

  it("auth handshake succeeds against a mock relay", async () => {
  const [phoneT, pcT] = linkedPair();
  const phone = new RelayConnection(phoneT, 30_000);
  const pc = new RelayConnection(pcT, 30_000);
  const authPromise = phone.authenticate("dev-phone", "sig-b64", 1_700_000_000_000);
  const got = (await pc.recvEnvelope())!;
  expect(got.type).toBe("device_auth");
  const ack: Envelope = {
  proto_version: PROTO_VERSION,
  id: null,
  type: "device_auth",
  payload: { device_id: "relay-ack", signature: "", timestamp_ms: 0 },
  };
  await pc.sendEnvelope(ack);
  await expect(authPromise).resolves.toBeUndefined();
  });

  it("clean close yields null on recv", async () => {
  const [phoneT, pcT] = linkedPair();
  const phone = new RelayConnection(phoneT, 30_000);
  const pc = new RelayConnection(pcT, 30_000);
  // Simulate the relay dropping the phone's connection: the phone's transport
  // observes its side as closed, so recv resolves null (no inbound frame).
  phoneT.forceClose();
  const got = await phone.recvEnvelope();
  expect(got).toBeNull();
  });
});
// end of file
