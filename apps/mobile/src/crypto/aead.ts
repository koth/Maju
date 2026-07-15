import { chacha20poly1305 } from "@noble/ciphers/chacha";
import type { EncryptedEnvelope, Envelope } from "../types/relay-protocol";
import { PROTO_VERSION } from "../types/relay-protocol";
import { randomBytes } from "../util/random";

// ChaCha20-Poly1305 AEAD framing, byte-aligned with relay-client::crypto.
// Rust: fresh 12-byte OsRng nonce, AAD = to_device_id UTF-8 bytes, ciphertext =
// AEAD(plaintext = serde_json::to_vec(envelope)) + 16-byte Poly1305 tag.
// noble: chacha20poly1305(key, nonce, aad).encrypt(plaintext) appends the tag.

export const NONCE_LEN = 12;
export const TAG_LEN = 16;

function toUtf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

/**
 * Encrypt a typed `Envelope` into a relay-routable `EncryptedEnvelope`.
 * `toDeviceId` is the routing target and is bound as AEAD associated data.
 */
export function encrypt(
  key: { bytes: Uint8Array },
  toDeviceId: string,
  envelope: Envelope,
): EncryptedEnvelope {
  const nonce = randomBytes(NONCE_LEN);
  const plaintext = toUtf8(JSON.stringify(envelope));
  const aad = toUtf8(toDeviceId);
  const cipher = chacha20poly1305(key.bytes, nonce, aad);
  const ciphertext = cipher.encrypt(plaintext);
  return {
    to_device_id: toDeviceId,
    nonce: Array.from(nonce),
    ciphertext: Array.from(ciphertext),
  };
}

/**
 * Decrypt an `EncryptedEnvelope` back into a typed `Envelope`. Verifies the
 * AEAD tag and checks `proto_version` matches the current `PROTO_VERSION`.
 */
export function decrypt(
  key: { bytes: Uint8Array },
  encrypted: EncryptedEnvelope,
): Envelope {
  if (encrypted.nonce.length !== NONCE_LEN) {
    throw new Error(`invalid nonce length: ${encrypted.nonce.length}`);
  }
  const nonce = new Uint8Array(encrypted.nonce);
  const aad = toUtf8(encrypted.to_device_id);
  const cipher = chacha20poly1305(key.bytes, nonce, aad);
  const plaintext = cipher.decrypt(new Uint8Array(encrypted.ciphertext));
  const envelope: Envelope = JSON.parse(new TextDecoder().decode(plaintext));
  if (envelope.proto_version !== PROTO_VERSION) {
    throw new Error(
      `proto version mismatch: got ${envelope.proto_version} expected ${PROTO_VERSION}`,
    );
  }
  return envelope;
}
