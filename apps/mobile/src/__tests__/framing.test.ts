import { describe, it, expect } from "vitest";
import { fromMessage, intoMessage, serializeEnvelope, parseEnvelope } from "../relay/framing";
import { PROTO_VERSION, type EncryptedEnvelope } from "../types/relay-protocol";

describe("envelope framing", () => {
  const requestId = "11111111-2222-4333-8444-555555555555";

  it("round-trip preserves all fields", () => {
    const env = fromMessage(requestId, {
      type: "control_request",
      payload: { op: "cancel", request_id: requestId },
    });
    expect(env.proto_version).toBe(PROTO_VERSION);
    expect(env.id).toBe(requestId);
    expect(env.type).toBe("control_request");
    const json = serializeEnvelope(env);
    const back = parseEnvelope(json);
    expect(back).toEqual(env);
  });

  it("control request echoes request_id", () => {
    const env = fromMessage(requestId, {
      type: "control_request",
      payload: { op: "cancel", request_id: requestId },
    });
    expect(env.id).toBe(requestId);
    const msg = intoMessage(env);
    expect(msg.type).toBe("control_request");
    const req = msg.payload as { op: string; request_id: string };
    expect(req.op).toBe("cancel");
    expect(req.request_id).toBe(requestId);
  });

  it("unknown type maps to the catch-all", () => {
    const env = parseEnvelope(
      JSON.stringify({
        proto_version: PROTO_VERSION,
        id: null,
        type: "some_future_message",
        payload: { anything: 42 },
      }),
    );
    const msg = intoMessage(env);
    expect(msg.type).toBe("some_future_message");
    expect(msg.payload).toEqual({ anything: 42 });
  });

  it("encrypted envelope exposes only routing fields", () => {
    const enc: EncryptedEnvelope = {
      to_device_id: "dev-1",
      nonce: [1, 2, 3],
      ciphertext: [4, 5, 6],
    };
    const json = JSON.parse(JSON.stringify(enc)) as Record<string, unknown>;
    expect(Object.keys(json).sort()).toEqual(["ciphertext", "nonce", "to_device_id"]);
  });
});
