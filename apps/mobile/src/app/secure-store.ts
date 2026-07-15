import * as SecureStore from "expo-secure-store";
import type { SecretStore } from "../crypto/identity";
import { encodeBase64UrlNoPad, decodeBase64UrlNoPad } from "../util/base64url";

// expo-secure-store-backed SecretStore for the device identity + bound device.
// Stores binary secrets as base64url-no-pad strings under the given key.
// Keychain (iOS) / Keystore (Android) provide hardware-backed isolation; app
// uninstall clears the entries. This is separate from the E2E SessionKey,
// which is never persisted. Mirrors the SecretStore interface in
// `crypto/identity.ts` (InMemorySecretStore is the test impl).
export class SecureSecretStore implements SecretStore {
  async get(key: string): Promise<Uint8Array | null> {
    const value = await SecureStore.getItemAsync(key);
    if (value === null) return null;
    return decodeBase64UrlNoPad(value);
  }

  async set(key: string, value: Uint8Array): Promise<void> {
    await SecureStore.setItemAsync(key, encodeBase64UrlNoPad(value), {
      keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK,
    });
  }

  async delete(key: string): Promise<void> {
    await SecureStore.deleteItemAsync(key);
  }
}
// end of file
