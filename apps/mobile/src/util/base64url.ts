// Base64url WITHOUT padding (RFC 4648 §5), matching the Rust
// `base64::engine::general_purpose::URL_SAFE_NO_PAD` used by relay-client's
// device id / auth signature / pubkey encodings. Pure JS over Uint8Array so
// it works in node (tests) and React Native (Hermes) without a Buffer polyfill.

const ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

export function encodeBase64UrlNoPad(bytes: Uint8Array): string {
  let out = "";
  const len = bytes.length;
  for (let i = 0; i < len; i += 3) {
    const b0 = bytes[i];
    const b1 = i + 1 < len ? bytes[i + 1] : 0;
    const b2 = i + 2 < len ? bytes[i + 2] : 0;
    const triple = (b0 << 16) | (b1 << 8) | b2;
    out += ALPHABET[(triple >> 18) & 0x3f];
    out += ALPHABET[(triple >> 12) & 0x3f];
    if (i + 1 < len) out += ALPHABET[(triple >> 6) & 0x3f];
    if (i + 2 < len) out += ALPHABET[triple & 0x3f];
  }
  return out;
}

export function decodeBase64UrlNoPad(input: string): Uint8Array {
  const cleaned = input.replace(/=+$/g, "");
  const len = cleaned.length;
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (let i = 0; i < len; i++) {
    const ch = cleaned[i];
    const value = ALPHABET.indexOf(ch);
    if (value === -1) {
      throw new Error(`invalid base64url character: ${ch}`);
    }
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return new Uint8Array(out);
}

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i].toString(16).padStart(2, "0");
  }
  return out;
}

export function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) {
    throw new Error("hex string must have even length");
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
