import type { RelayConnection } from "../relay/connection";
import type { PairingQrPayload, PairingConfirm } from "../types/relay-protocol";
import { fromMessage } from "../relay/framing";
import { PROTO_VERSION } from "../types/relay-protocol";
import type { DeviceIdentity } from "../crypto/identity";
import { deviceId, authSignature, publicKeyB64 } from "../crypto/identity";
import { generatePrivateKey, getPublicKey, ecdhSharedSecret } from "../crypto/ecdh";
import { encodeBase64UrlNoPad } from "../util/base64url";
import { deriveSessionKey, type SessionKey } from "../crypto/session-key";
import { pcStaticPublicKey } from "./qr-parse";

export interface PairingResult {
  sessionKey: SessionKey;
  pcDeviceId: string;
  phoneDeviceId: string;
  pairingToken: string;
}

/**
 * Run the phone side of the E2E pairing handshake over an already-dialed,
 * authenticated connection (DeviceAuth happens first, in the connection layer).
 * Generates a fresh ephemeral X25519 keypair, sends `PairingInitiate` with the
 * ephemeral public key, awaits `PairingConfirm`, and derives the SessionKey
 * from X25519(ephemeral_secret, pc_static_public) + HKDF. The ephemeral secret
 * and pairing code are discarded after derivation (security rule).
 */
export async function runPairingHandshake(
  conn: RelayConnection,
  identity: DeviceIdentity,
  qr: PairingQrPayload,
): Promise<PairingResult> {
  // Fresh ephemeral keypair for this pairing (discarded after derivation).
  const ephemeralSecret = generatePrivateKey();
  const ephemeralPublic = getPublicKey(ephemeralSecret);
  const pcStaticPublic = pcStaticPublicKey(qr);

  const initiateEnv = fromMessage(null, {
  type: "pairing_initiate",
  payload: {
  pairing_code: qr.pairing_code,
  pc_device_pubkey: qr.pc_device_pubkey,
  relay_endpoint: qr.relay_endpoint,
  phone_ephemeral_pubkey: encodeBase64UrlNoPad(ephemeralPublic),
  },
  });
  await conn.sendEnvelope(initiateEnv);

  const confirmEnv = await conn.recvEnvelope();
  if (confirmEnv === null) {
  throw new Error("relay closed during pairing handshake");
  }
  if (confirmEnv.type !== "pairing_confirm") {
  throw new Error(`unexpected pairing response: ${confirmEnv.type}`);
  }
  const confirm = confirmEnv.payload as PairingConfirm;

  const shared = ecdhSharedSecret(ephemeralSecret, pcStaticPublic);
  const sessionKey = deriveSessionKey(shared);

  return {
  sessionKey,
  pcDeviceId: confirm.pc_device_id,
  phoneDeviceId: confirm.phone_device_id,
  pairingToken: confirm.pairing_token,
  };
}

/** Build a DeviceAuth envelope for the connection-layer authenticate step. */
export function buildDeviceAuthArgs(identity: DeviceIdentity): {
  deviceId: string;
  signature: string;
  timestampMs: number;
} {
  const ts = Date.now();
  return {
  deviceId: deviceId(identity),
  signature: authSignature(identity, ts),
  timestampMs: ts,
  };
}

/** Re-export the public-key b64 helper for tests/parity checks. */
export { publicKeyB64 };

/** A pairing code is single-use: this helper signals it must not be reused. */
export const PAIRING_CODE_SINGLE_USE = true;

// Re-export PROTO_VERSION for callers building raw envelopes.
export { PROTO_VERSION };
// end of file
