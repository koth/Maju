import type { RelayTransport } from "./transport";
import type { Envelope, EncryptedEnvelope, Message } from "../types/relay-protocol";
import { PROTO_VERSION, DeviceAuth } from "../types/relay-protocol";
import { encrypt, decrypt } from "../crypto/aead";
import type { SessionKey } from "../crypto/session-key";
import { serializeEnvelope, parseEnvelope } from "./framing";

// A relay connection with optional E2E encryption. When no session key is
// installed (pre-pairing auth phase) it sends/receives plain `Envelope` JSON;
// after `installSessionKey` it encrypts each `Envelope` into an
// `EncryptedEnvelope` and decrypts inbound frames, so the relay routes
// ciphertext only. Mirrors relay_client::connection::RelayConnection.
export class RelayConnection {
  private sessionKey: { bytes: Uint8Array } | null = null;
  private peerDeviceId: string | null = null;

  constructor(
    private readonly transport: RelayTransport,
    readonly heartbeatMs: number = 30_000,
  ) {}

  /** Install the E2E session key (post-pairing). Subsequent send/recv is E2E. */
  installSessionKey(key: SessionKey, peerDeviceId: string): void {
    this.sessionKey = key;
    this.peerDeviceId = peerDeviceId;
  }

  hasSessionKey(): boolean {
    return this.sessionKey !== null;
  }

  /** Send an envelope: encrypt to EncryptedEnvelope when a key is installed,
   * otherwise send plain Envelope JSON (auth phase). */
  async sendEnvelope(envelope: Envelope): Promise<void> {
    const frame =
      this.sessionKey && this.peerDeviceId
        ? serializeEncrypted(
            encrypt(this.sessionKey, this.peerDeviceId, envelope),
          )
        : serializeEnvelope(envelope);
    await this.transport.sendText(frame);
  }

  /** Receive the next envelope: decrypt an EncryptedEnvelope when a key is
   * installed, otherwise parse a plain Envelope. Returns null on clean close. */
  async recvEnvelope(): Promise<Envelope | null> {
    const frame = await this.transport.recvText();
    if (frame === null) return null;
    if (this.sessionKey) {
      const enc = JSON.parse(frame) as EncryptedEnvelope;
      return decrypt(this.sessionKey, enc);
    }
    return parseEnvelope(frame);
  }

  /** Pre-pairing auth: send a DeviceAuth envelope (plain) and await an ack.
   * Must be called before installSessionKey. */
  async authenticate(
    deviceId: string,
    signature: string,
    timestampMs: number,
  ): Promise<void> {
    const auth: DeviceAuth = { device_id: deviceId, signature, timestamp_ms: timestampMs };
    const env: Envelope = {
      proto_version: PROTO_VERSION,
      id: null,
      type: "device_auth",
      payload: auth,
    };
    await this.sendEnvelope(env);
    const ack = await this.recvEnvelope();
    if (ack === null) {
      throw new Error("relay closed during auth handshake");
    }
    const msg = ack;
    if (msg.type !== "device_auth" && msg.type !== "subscription_status") {
      throw new Error(`unexpected auth response: ${msg.type}`);
    }
  }

  async close(): Promise<void> {
    await this.transport.close();
  }
}

function serializeEncrypted(enc: EncryptedEnvelope): string {
  return JSON.stringify(enc);
}
// end of file
