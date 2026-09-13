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

## App icon

The mobile icon is derived from the desktop brand mark
(`apps/desktop/src-tauri/icons/maju.png`) so both platforms show the same
icon. After the brand mark changes, regenerate the Expo source icon and the
Android mipmaps from the repo root:

```bash
scripts/generate-mobile-icons.sh   # needs macOS sips + libwebp's cwebp
```

Then rebuild the app. The `android/` and `ios/` projects are gitignored, so the
tracked artifact is `assets/icon.png` — keep it in sync with the script rather
than editing it by hand.

## Design system

`src/features/theme.ts` is the single source of truth for the visual language.
The rules it encodes (and that screen work must follow):

1. **One accent.** Blue is only for interactive/active state — active tab,
   focused input, running indicator, primary action. Everything else neutral.
2. **No chrome by default.** Lists are separated by `styles.hairline` /
   `hairlineInset` and whitespace, never by a bordered card per row. A bordered
   box is for genuinely floating surfaces (sheets, banners) only.
3. **Status is text or a dot**, not a tinted pill: pass a semantic color on a
   `<Text>` (or a small `View` dot) instead of building a colored chip.
4. **Depth is luminance**, not shadow: step `bg → surface → surfaceAlt →
   surfaceRaised`. `shadows` is empty for rows/cards on purpose.

Use `typeScale` for type sizes and the `styles` primitives (`row`, `card`,
`input`, `button`, `buttonGhost`, `sheet`, `hairlineInset`) before inventing a
one-off `StyleSheet` — per-screen one-offs are how the UI drifted apart before.

## Relay endpoint

The relay endpoint is supplied by the PC's pairing QR (`relay_endpoint`). It
must be `wss://` except for dev-shaped plain-`ws://` targets (bare IP
literals, `localhost`, `*.local`) which are accepted without any flag — these
are what the PC emits during the no-domain development window and they have no
TLS identity to protect anyway. Real hostnames keep requiring `wss://` unless
the env var `EXPO_PUBLIC_RELAY_ALLOW_INSECURE_WS=1` is set (Expo inlines
`EXPO_PUBLIC_*` vars at bundle time; raw `gradlew` builds do NOT read
`.env*` files, so set it in the shell if you need it there).

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
