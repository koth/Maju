import { describe, it, expect } from "vitest";
import { RelayConnection } from "../relay/connection";
import { linkedPair } from "./mock-relay";
import { parsePairingQr, pcStaticPublicKey } from "../pairing/qr-parse";
import { runPairingHandshake } from "../pairing/pairing-flow";
import { deviceIdentityFromSecret, getPublicKey, ecdhSharedSecret, deriveSessionKey } from "../crypto";
import { encodeBase64UrlNoPad, decodeBase64UrlNoPad, bytesToHex } from "../util/base64url";
import { fromMessage } from "../relay/framing";
import { encrypt, decrypt } from "../crypto/aead";
import type { PairingInitiate, PairingQrPayload } from "../types/relay-protocol";

// PC static secret [200..231]; its public key is the KAT PC_PUBLIC_HEX.
const PC_SECRET = Uint8Array.from({ length: 32 }, (_, i) => 200 + i);
const PC_PUBLIC_HEX = "4d5bab89b0733d9d8dcecf04f321c90b761b7765a6bdb2bddbfad3e7abdf1f66";

function makeQr(allowWs = false): PairingQrPayload {
  const pcPublic = getPublicKey(PC_SECRET);
  return parsePairingQr(
  JSON.stringify({
  relay_endpoint: "wss://relay.example.com",
  pairing_code: "ABC123XY",
  pc_device_pubkey: encodeBase64UrlNoPad(pcPublic),
  }),
  allowWs,
  );
}

describe("qr parsing", () => {
  it("parses a valid QR and validates the PC pubkey length", () => {
  const qr = makeQr();
  expect(qr.relay_endpoint).toBe("wss://relay.example.com");
  expect(qr.pairing_code).toBe("ABC123XY");
  expect(pcStaticPublicKey(qr)).toHaveLength(32);
  });

  it("rejects a non-TLS endpoint unless debug is set", () => {
  const raw = JSON.stringify({
  relay_endpoint: "ws://relay.example.com",
  pairing_code: "ABC123XY",
  pc_device_pubkey: encodeBase64UrlNoPad(getPublicKey(PC_SECRET)),
  });
  expect(() => parsePairingQr(raw)).toThrow();
  expect(() => parsePairingQr(raw, true)).not.toThrow();
  });
});

describe("pairing handshake", () => {
  it("phone and PC derive identical session keys (E2E parity)", async () => {
  const [phoneT, pcT] = linkedPair();
  const phoneConn = new RelayConnection(phoneT, 30_000);
  const pcConn = new RelayConnection(pcT, 30_000);
  const qr = makeQr();
  const pcStaticPublic = pcStaticPublicKey(qr);
  // sanity: the PC public key derived from the fixed secret matches the KAT
  expect(bytesToHex(getPublicKey(PC_SECRET))).toBe(PC_PUBLIC_HEX);

  const phoneIdentity = deviceIdentityFromSecret(
  Uint8Array.from({ length: 32 }, (_, i) => i),
  );

  // Phone starts the handshake (sends PairingInitiate, awaits PairingConfirm).
  const phonePromise = runPairingHandshake(phoneConn, phoneIdentity, qr);

  // Fake PC: receive PairingInitiate, derive the shared secret from its own
  // static secret and the phone's ephemeral public key, then reply.
  const initEnv = (await pcConn.recvEnvelope())!;
  expect(initEnv.type).toBe("pairing_initiate");
  const init = initEnv.payload as PairingInitiate;
  const phoneEphPub = decodeBase64UrlNoPad(init.phone_ephemeral_pubkey!);
  const pcShared = ecdhSharedSecret(PC_SECRET, phoneEphPub);
  const pcSessionKey = deriveSessionKey(pcShared);

  await pcConn.sendEnvelope(
  fromMessage(null, {
  type: "pairing_confirm",
  payload: {
  pairing_token: "ptok",
  session_key_material: init.phone_ephemeral_pubkey!,
  pc_device_id: "pc-dev",
  phone_device_id: "phone-dev",
  },
  }),
  );

  const result = await phonePromise;
  // Phone derives X25519(ephemeral_secret, pc_static_public) + HKDF == PC's.
  expect(bytesToHex(result.sessionKey.bytes)).toBe(
  bytesToHex(pcSessionKey.bytes),
  );
  expect(result.pcDeviceId).toBe("pc-dev");
  expect(result.pairingToken).toBe("ptok");

  // Install on both sides and prove E2E works post-pairing.
  phoneConn.installSessionKey(result.sessionKey, "pc-dev");
  pcConn.installSessionKey(pcSessionKey, "phone-dev");
  const env = fromMessage("rid-1", {
  type: "control_request",
  payload: { op: "cancel", request_id: "rid-1" },
  });
  await phoneConn.sendEnvelope(env);
  const got = await pcConn.recvEnvelope();
  expect(got).toEqual(env);
  // and the reverse direction decrypts too
  const enc = encrypt(result.sessionKey, "phone-dev", env);
  expect(decrypt(pcSessionKey, enc)).toEqual(env);
  });
});
// end of file
