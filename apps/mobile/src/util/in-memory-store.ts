import type { SecretStore } from "../crypto/identity";

// In-memory SecretStore for tests and ephemeral use. The app wires the
// Keychain/Keystore-backed implementation (expo-secure-store) at runtime.
export class InMemorySecretStore implements SecretStore {
  private map = new Map<string, Uint8Array>();

  async get(key: string): Promise<Uint8Array | null> {
  return this.map.get(key) ?? null;
  }
  async set(key: string, value: Uint8Array): Promise<void> {
  this.map.set(key, Uint8Array.from(value));
  }
  async delete(key: string): Promise<void> {
  this.map.delete(key);
  }
}
// end of file
