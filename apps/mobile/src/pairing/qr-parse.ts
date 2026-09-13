import type { PairingQrPayload } from "../types/relay-protocol";
import { decodeBase64UrlNoPad } from "../util/base64url";

export const PC_PUBKEY_LEN = 32;

/**
 * True when a plain `ws://` endpoint targets something that has no TLS
 * identity to protect anyway: a bare IP literal, `localhost`, or a `.local`
 * hostname. These are the dev-window shapes the PC emits before the relay
 * gains a domain + real certificate; production domains keep requiring
 * `wss://`. This replaces the old build-time-only env gate, which never
 * reached raw `gradlew` bundles and left pairing broken after reinstalls.
 */
export function isInsecureDevEndpoint(url: string): boolean {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "ws:") return false;
    const host = parsed.hostname;
    return (
      host === "localhost" ||
      host.endsWith(".local") ||
      /^\d{1,3}(\.\d{1,3}){3}$/.test(host)
    );
  } catch {
    return false;
  }
}

/**
 * Parse a scanned QR string into a `PairingQrPayload`, enforcing the
 * security rules: relay endpoint must be `wss://` (plain `ws://` allowed for
 * dev-shaped targets — see {@link isInsecureDevEndpoint} — or when
 * `allowInsecureDebug` is set), and the PC device public key must be a
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
  const wsAllowed =
    allowInsecureDebug ||
    isInsecureDevEndpoint(payload.relay_endpoint);
  if (!(wsAllowed && payload.relay_endpoint.startsWith("ws://"))) {
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
  // Normalize the optional identity fields: a non-string or blank value must
  // not become a garbage label/address on the machines list.
  const clean = (value: unknown): string | undefined => {
    if (typeof value !== "string") return undefined;
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : undefined;
  };
  return {
    ...payload,
    pc_name: clean(payload.pc_name),
    pc_ip: clean(payload.pc_ip),
  };
}

/** Decode the PC static public key from a parsed QR payload (32 bytes). */
export function pcStaticPublicKey(payload: PairingQrPayload): Uint8Array {
  return decodeBase64UrlNoPad(payload.pc_device_pubkey);
}
// end of file
