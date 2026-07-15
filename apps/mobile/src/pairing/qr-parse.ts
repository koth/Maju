import type { PairingQrPayload } from "../types/relay-protocol";
import { decodeBase64UrlNoPad } from "../util/base64url";

export const PC_PUBKEY_LEN = 32;

/**
 * Parse a scanned QR string into a `PairingQrPayload`, enforcing the
 * security rules: relay endpoint must be `wss://` (plain `ws://` only allowed
 * when `allowInsecureDebug` is set), and the PC device public key must be a
 * 32-byte X25519 key (base64url-no-pad). The pairing code is used-then-discarded
 * by the caller; it is never persisted here.
 */
export function parsePairingQr(
  raw: string,
  allowInsecureDebug = false,
): PairingQrPayload {
  const payload = JSON.parse(raw) as PairingQrPayload;
  if (!payload || typeof payload.relay_endpoint !== "string") {
  throw new Error("invalid pairing QR: missing relay_endpoint");
  }
  if (!payload.relay_endpoint.startsWith("wss://")) {
  if (!(allowInsecureDebug && payload.relay_endpoint.startsWith("ws://"))) {
  throw new Error(`refusing non-TLS relay endpoint: ${payload.relay_endpoint}`);
  }
  }
  if (typeof payload.pairing_code !== "string" || !payload.pairing_code) {
  throw new Error("invalid pairing QR: missing pairing_code");
  }
  if (typeof payload.pc_device_pubkey !== "string" || !payload.pc_device_pubkey) {
  throw new Error("invalid pairing QR: missing pc_device_pubkey");
  }
  const pub = decodeBase64UrlNoPad(payload.pc_device_pubkey);
  if (pub.length !== PC_PUBKEY_LEN) {
  throw new Error(`pc_device_pubkey is ${pub.length} bytes, expected ${PC_PUBKEY_LEN}`);
  }
  return payload;
}

/** Decode the PC static public key from a parsed QR payload (32 bytes). */
export function pcStaticPublicKey(payload: PairingQrPayload): Uint8Array {
  return decodeBase64UrlNoPad(payload.pc_device_pubkey);
}
// end of file
