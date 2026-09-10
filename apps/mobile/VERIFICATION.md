# Verification — Mobile Companion App

Maps `docs/mobile-companion-app-requirements.md` §9 acceptance criteria to the
implementation, with automated vs. needs-native-device status.

## Automated (green here)

- `npx tsc --noEmit` — clean
- `npx vitest run` — 166 tests across 19 files
- `src/__tests__/integration.test.ts` — end-to-end over an in-memory relay
- `src/__tests__/permission.test.ts` — approval store, default-deny timeout, and
  the two approval shapes (question form vs. options-only)
- `src/__tests__/turn-completion.test.ts` — turn-completion watcher transitions/dedupe/suppression
- `src/__tests__/alert-presenter.test.ts` — context-aware alert policy table

## Remote approvals

Two shapes arrive over the relay, and both are answerable from the phone:

| Shape | Example | Phone behavior |
|---|---|---|
| `permission_input` question form | DeepSeek harness `ask_user_question` | Full question UI (radio/checkbox/free text) + submit/cancel |
| `permission_options` only (no input) | Codex `run_command` / `apply_patch`, Codebuddy bash | Option list from the tool; destructive ones require picking an option, then a second confirm |

Both are derived from the snapshot tool (`isPendingPermissionTool`), which is why
the PC must push the snapshot update for an input-less approval as well — a
`PermissionRequested` signal alone cannot carry its options
(`crates/app-core/src/application/events.rs`).

## Turn-completion alerts (add-mobile-turn-completion-alerts)

Logic is covered by the two test files above. The following need a real
device (`expo run:android`, dev build — Expo Go cannot run the native
modules):

| # | Scenario | Expected |
|---|---|---|
| A1 | 发送 prompt 后停留在该会话页等轮次结束 | 仅轻震动；无铃声、无横幅、无系统通知 |
| A2 | 发送 prompt 后切到会话列表等轮次结束 | 提示音 + 成功震动 + 应用内横幅；点横幅跳转会话页 |
| A3 | 发送 prompt 后切出 App（Home 键）等轮次结束 | 系统通知（标题"任务已完成"，带声音+震动，渠道"任务完成提醒"） |
| A4 | 轮次被 PC 端中断 | 中断提示音 + Warning 震动 + 横幅/通知文案为"已中断" |
| A5 | 手机端点取消 | 无提醒（5 秒抑制窗口）；此后 PC 侧中断仍提醒 |
| A6 | 设置里关闭"提示音"/"震动" | 前台提醒对应通道静默；后台通知落到对应无声/无震动渠道 |
| A7 | 开启"仅后台时提醒" | 前台完全静默；后台仍弹系统通知 |
| A8 | 关闭总开关 | 任何场景均无提醒 |
| A9 | 系统通知权限被拒绝 | 后台无通知；设置页显示"通知权限未授予"提示 |
| A10 | 关声音 → 杀进程 → 重启 → 完成一轮 | 不响铃（设置持久化生效） |
| A11 | 重连/打开已完成会话 | 不触发提醒（无前态基线规则） |

Known limit (by design, disclosed in the settings screen): alerts require the
app process + relay connection to be alive; a killed app cannot alert until
server-side push (FCM/APNs) is introduced.

## §9 acceptance criteria

| # | Criterion | Where | Status |
|---|---|---|---|
| 1 | Scan QR → pair → both derive the same SessionKey (encrypt/decrypt round-trip) | `pairing.test.ts`, `integration.test.ts` bootstrap | automated |
| 2 | `CreateSession` → `session_id` + `SnapshotFull` | `integration.test.ts` (creates a session) | automated |
| 3 | `SendPrompt` → `ToolUpdated` stream → `SessionStatusChanged{Idle}` | `integration.test.ts` (streams tool updates to Idle) | automated |
| 4 | Destructive tool → `PermissionRequest` → approve executes; deny aborts | `permission.test.ts`, `integration.test.ts` (approve, incl. the options-only Codex shape) | automated (approve); deny covered by `permission.test.ts` deny path |
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
