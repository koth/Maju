import { hmac } from "@noble/hashes/hmac";
import { sha256 } from "@noble/hashes/sha256";
import { encodeBase64UrlNoPad } from "../util/base64url";
import {
  generatePrivateKey,
  getPublicKey,
  SECRET_KEY_LEN,
} from "./ecdh";

// Device identity, byte-aligned with relay_client::identity::DeviceIdentity.
// - device_id = base64url-no-pad(SHA-256(public_key))   [URL_SAFE_NO_PAD]
// - auth_signature = base64url-no-pad(HMAC-SHA256(secret, "{device_id}:{ts_ms}"))
// The X25519 static keypair is the DEVICE identity (persistent, secure store);
// it is distinct from the per-pairing ephemeral key used for E2E derivation.

export interface DeviceIdentity {
  /** 32-byte static X25519 private key (never transmitted in plaintext). */
  readonly secret: Uint8Array;
 /** 32-byte static public key. */
  readonly publicKey: Uint8Array;
}

/**
 * Pluggable secret persistence so the identity core stays unit-testable without
 * the platform secure-storage (expo-secure-store) dependency. The app wires the
 * Keychain/Keystore-backed implementation; tests use an in-memory store.
 */
export interface SecretStore {
  get(key: string): Promise<Uint8Array | null>;
  set(key: string, value: Uint8Array): Promise<void>;
  delete(key: string): Promise<void>;
}

export const DEVICE_SECRET_KEY = "kodex.device-secret";

/** Generate a fresh device identity (OS RNG). */
export function generateDeviceIdentity(): DeviceIdentity {
  const secret = generatePrivateKey();
  const publicKey = getPublicKey(secret);
  return { secret, publicKey };
}

/** Reconstruct an identity from a stored 32-byte secret (derives the pubkey). */
export function deviceIdentityFromSecret(secret: Uint8Array): DeviceIdentity {
  if (secret.length !== SECRET_KEY_LEN) {
  throw new Error(`device secret is ${secret.length} bytes, expected ${SECRET_KEY_LEN}`);
  }
  return { secret, publicKey: getPublicKey(secret) };
}

/** Stable device id: base64url-no-pad(SHA-256(public_key)). */
export function deviceId(identity: DeviceIdentity): string {
  const hash = sha256(identity.publicKey);
  return encodeBase64UrlNoPad(hash);
}

/** Public key, base64url-no-pad (for the QR pairing payload). */
export function publicKeyB64(identity: DeviceIdentity): string {
  return encodeBase64UrlNoPad(identity.publicKey);
}

/** HMAC-SHA256(secret, "{device_id}:{timestamp_ms}"), base64url-no-pad. */
export function authSignature(identity: DeviceIdentity, timestampMs: number): string {
  const message = `${deviceId(identity)}:${timestampMs}`;
  const mac = hmac.create(sha256, identity.secret);
  mac.update(new TextEncoder().encode(message));
  return encodeBase64UrlNoPad(mac.digest());
}

/**
 * Load the device identity from `store`, generating + persisting a fresh one
 * if absent. Mirrors relay_client::DeviceIdentity::load_or_create.
 */
export async function loadOrCreateIdentity(
  store: SecretStore,
): Promise<DeviceIdentity> {
  const existing = await store.get(DEVICE_SECRET_KEY);
  if (existing) {
    return deviceIdentityFromSecret(existing);
  }
  const identity = generateDeviceIdentity();
  await store.set(DEVICE_SECRET_KEY, identity.secret);
  return identity;
}
