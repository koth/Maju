# Maju Mobile Companion

A React Native (Expo) + TypeScript phone app that pairs with a running Maju
desktop (PC) over a relay and remotely controls it: scan-to-pair, end-to-end
encrypted session control, tool-call/timeline rendering, and remote permission
approval. See the requirements at
`docs/mobile-companion-app-requirements.md` and the OpenSpec change
`add-mobile-companion-app`.

## Stack

- React Native 0.76 + Expo SDK 52 + TypeScript
- `@noble/curves`/`@noble/hashes`/`@noble/ciphers` for X25519 + HKDF-SHA256 +
  ChaCha20-Poly1305 AEAD, byte-aligned with `crates/relay-client::crypto`
- `expo-secure-store` (Keychain/Keystore) for the device identity + binding
- `expo-camera` for QR scanning; `@react-navigation/native-stack` for nav
- Vitest for the pure-logic + protocol + integration tests

The crypto, framing, relay connection, reducer, pairing, permission, and
account logic are all framework-agnostic (no React) and unit-tested; the RN
UI layer in `src/features/` is thin and type-checked.

## Layout

| Path | Role |
|---|---|
| `src/types/` | Vendored mirrors of `relay-protocol` + `workspace-model` DTOs |
| `src/crypto/` | ECDH, HKDF `SessionKey`, AEAD, device identity (HMAC auth) |
| `src/relay/` | TLS WebSocket transport, `RelayConnection` (E2E), state machine, backoff, receive loop |
| `src/pairing/` | QR parse + E2E pairing handshake |
| `src/session/` | `ControlClient` (request/response matching), snapshot reducer, `SessionStore`, permission store |
| `src/account/` | Device binding, subscription state, login interface |
| `src/app/` | `AppController` service + React context/hooks + navigation root + secure-store adapter |
| `src/features/` | Screens: pairing, session-list, conversation (timeline/markdown/tool card), composer, permission, settings |
| `src/__tests__/` | Crypto conformance, framing, connection, pairing, reducer, binding, permission, end-to-end integration |

## Develop

```bash
cd apps/mobile
npx tsc --noEmit      # typecheck
npx vitest run        # all tests
npx vitest run src/__tests__/integration.test.ts   # one file
npx expo start       # Metro dev server (then press i/a for iOS/Android)
```

Build a dev client (requires Xcode/Android Studio toolchains):

```bash
npx expo prebuild           # generate native ios/ android/ projects
npx expo run:ios            # or run:android
```

## Relay endpoint

The relay endpoint is supplied by the PC's pairing QR (`relay_endpoint`), which
must be `wss://`. The `WsTransport` rejects `ws://` unless an explicit debug
flag is passed. To allow insecure `ws://` for local development, set the
env var `KODEX_RELAY_ALLOW_INSECURE_WS=1` before launching Expo, or pass the
debug flag through `pairFromTransport(transport, qrJson, true)`.

## Security notes

- The X25519 static device secret is stored in the OS secure store
  (`expo-secure-store`: iOS Keychain / Android Keystore). App uninstall clears
  it.
- The E2E `SessionKey` is session-scoped: derived at pairing, held in memory,
  discarded on disconnect. It is never persisted.
- The one-time `pairing_code` is used and discarded; never stored.
- `auth_token` (account) and the E2E `SessionKey` are stored separately.
- Permission approval is default-deny: destructive remote operations are never
  auto-approved (PC `remote_mode` gates this) and require an explicit second
  confirmation on the phone.

## React Native polyfills

`App.tsx` imports `react-native-get-random-values` before any `@noble/*` crypto
so `crypto.getRandomValues` is polyfilled on Hermes (no `Math.random` fallback).
Keep that import first.

## Conformance

The phone crypto must be byte-identical with `crates/relay-client`. The
`src/__tests__/crypto-conformance.test.ts` vectors are generated from the Rust
crate (`SessionKey::derive` salt `kodex-relay-salt`, info
`kodex-relay-e2e-v1`; AEAD AAD = `to_device_id`). If the Rust crypto changes,
regenerate the KAT vectors and update this suite.
// end of file
