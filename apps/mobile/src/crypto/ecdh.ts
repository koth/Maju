import { x25519 } from "@noble/curves/ed25519";
import { randomBytes } from "../util/random";

// X25519 key agreement, byte-aligned with relay-client::pairing
// (`x25519_dalek` `StaticSecret::diffie_hellman`). X25519 is deterministic, so
// identical private/public byte inputs produce identical shared secrets.

export const PUBLIC_KEY_LEN = 32;
export const SECRET_KEY_LEN = 32;

/** Generate a fresh 32-byte X25519 private key (OS RNG). */
export function generatePrivateKey(): Uint8Array {
  // x25519.utils.randomPrivateKey uses the noble RNG; ensure it uses ours.
  return randomBytes(SECRET_KEY_LEN);
}

/** Derive the 32-byte public key from a 32-byte private key. */
export function getPublicKey(privateKey: Uint8Array): Uint8Array {
  return x25519.getPublicKey(privateKey);
}

/**
 * Compute the 32-byte X25519 shared secret from a private key and the peer's
 * 32-byte public key. Mirrors relay-client::pairing::ecdh_shared_secret.
 */
export function ecdhSharedSecret(
  privateKey: Uint8Array,
  peerPublicKey: Uint8Array,
): Uint8Array {
  return x25519.getSharedSecret(privateKey, peerPublicKey);
}
