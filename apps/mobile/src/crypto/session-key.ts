import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";

// Session key derivation, byte-aligned with relay-client::crypto::SessionKey.
// Rust: Hkdf::<Sha256>::new(Some(salt), ikm).expand(b"kodex-relay-e2e-v1", &mut [0u8;32]).
// noble: hkdf(sha256, ikm, salt, info, dkLen) — same RFC 5869 HKDF-SHA256.

export const RELAY_SALT = "kodex-relay-salt";
export const RELAY_INFO = "kodex-relay-e2e-v1";
export const SESSION_KEY_LEN = 32;

export interface SessionKey {
  readonly bytes: Uint8Array;
}

export function deriveSessionKey(
  ikm: Uint8Array,
  salt: Uint8Array = new TextEncoder().encode(RELAY_SALT),
): SessionKey {
  const info = new TextEncoder().encode(RELAY_INFO);
  const okm = hkdf(sha256, ikm, salt, info, SESSION_KEY_LEN);
  return { bytes: okm };
}
