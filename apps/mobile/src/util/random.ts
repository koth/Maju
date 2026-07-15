// CSPRNG bridge. Uses the platform Web Crypto `getRandomValues`.
// - Node 22+: `globalThis.crypto.getRandomValues` is built in (tests).
// - React Native (Hermes): requires `react-native-get-random-values` to be
//   imported once at app entry (App.tsx) to polyfill `crypto.getRandomValues`.
// Never falls back to `Math.random`.

function getCrypto(): Crypto {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  if (!c || typeof c.getRandomValues !== "function") {
    throw new Error(
      "secure RNG unavailable: import 'react-native-get-random-values' at app entry (RN) or run on a modern runtime (node)",
    );
  }
  return c;
}

export function randomBytes(length: number): Uint8Array {
  const out = new Uint8Array(length);
  getCrypto().getRandomValues(out);
  return out;
}
