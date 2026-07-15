# Verification — Mobile Companion App

Maps `docs/mobile-companion-app-requirements.md` §9 acceptance criteria to the
implementation, with automated vs. needs-native-device status.

## Automated (green here)

- `npx tsc --noEmit` — clean
- `npx vitest run` — 63 tests across 8 files
- `src/__tests__/integration.test.ts` — end-to-end over an in-memory relay

## §9 acceptance criteria

| # | Criterion | Where | Status |
|---|---|---|---|
| 1 | Scan QR → pair → both derive the same SessionKey (encrypt/decrypt round-trip) | `pairing.test.ts`, `integration.test.ts` bootstrap | automated |
| 2 | `CreateSession` → `session_id` + `SnapshotFull` | `integration.test.ts` (creates a session) | automated |
| 3 | `SendPrompt` → `ToolUpdated` stream → `SessionStatusChanged{Idle}` | `integration.test.ts` (streams tool updates to Idle) | automated |
| 4 | Destructive tool → `PermissionRequest` → approve executes; deny aborts | `permission.test.ts`, `integration.test.ts` (approve) | automated (approve); deny covered by `permission.test.ts` deny path |
| 5 | `Cancel` → session back to Idle | `integration.test.ts` (cancel returns to Idle) | automated |
| 6 | `ListSessions` + `SwitchSession` → history snapshot | `integration.test.ts` (list + switch) | automated |
| 7 | Login + valid subscription → `BindDeviceResponse.ok=true`, restart免扫码 | `binding.test.ts`, `account/*` | logic automated; live relay login TBD per relay contract |
| 8 | No subscription → prompt to subscribe | `binding.test.ts` (subscription_required) | automated |
| 9 | Subscription expiry → demote, prompt re-scan, don't kill session | `subscription.test` (in `binding.test.ts`) | automated |
| 10 | Relay drop → reconnecting + retained snapshot; PC offline → "PC offline" | `integration.test.ts` (relay drop retains snapshot) | reconnect-resync automated; PC-offline is a UI state |
| 11 | Device keys in secure store; uninstall clears | `secure-store.ts` (expo-secure-store Keychain/Keystore) | code present; needs a device to verify Keychain semantics |

## Needs a native device / toolchain

These require Xcode/Android Studio and a real relay/PC; not automatable here:

- `npx expo prebuild` / `run:ios` / `run:android` build sanity
- Live camera QR scan against a PC's pairing QR
- Keychain/Keystore persistence + app-uninstall clearing (criterion 11)
- A live relay + bound-account reconnect (best-effort re-key; falls back to
  re-scan if the relay rejects the stored token — see `AppController`)
// end of file
