## 1. Capability Model

- [x] 1.1 Add `session_steer` to Rust `PromptInputCapabilities` with a default false value and serde compatibility.
- [x] 1.2 Add `session_steer` to the frontend TypeScript prompt capability type and update snapshot/test fixtures.
- [x] 1.3 Mark managed Codex ACP and Claude ACP sessions as steer-capable while leaving unknown agents unsupported.

## 2. App-Core Prompt Flow

- [x] 2.1 Add an active-turn steer path for prompt submissions received while `in_flight_prompt` exists and `session_steer` is enabled.
- [x] 2.2 Persist and append the steering user message to the current timeline without replacing the existing prompt owner.
- [x] 2.3 Preserve current turn state during steering, including status, active plan, tool ownership, permission ownership, and file-change tracking.
- [x] 2.4 Return a clear unsupported-steering error when active input is submitted for a session that does not advertise steering support.
- [x] 2.5 Add app-core tests for accepted steering, unsupported steering, and state preservation.

## 3. ACP Runtime Steering

- [x] 3.1 Update the active prompt loop to handle `RuntimeCommand::SendPrompt` instead of ignoring it while a prompt is active.
- [x] 3.2 Forward active prompt content as a concurrent ACP prompt request to the same session.
- [x] 3.3 Add generation or handoff ownership so stale prompt completions cannot mark the session idle after a later steer request is accepted.
- [x] 3.4 Add non-blocking client feedback for immediate steer rejection or transport failure.
- [x] 3.5 Add acp-core tests for active `SendPrompt`, stale completion handling, and rejection feedback.

## 4. Managed Agent Support

- [x] 4.1 Update Codex ACP tests and implementation so a second prompt during a regular active turn reaches Codex core as steering input.
- [x] 4.2 Ensure Codex ACP rejects non-steerable active turn types with a clear reason.
- [x] 4.3 Update Claude ACP tests and implementation so prompt-running input uses the pending-message handoff path and keeps completion ownership correct.
- [x] 4.4 Confirm both managed agents advertise steering support through Kodex capability mapping.

## 5. Composer UX

- [x] 5.1 Keep the Composer text input enabled during active steer-capable sessions.
- [x] 5.2 Change the active-session primary action to send additional instructions when text is non-empty.
- [x] 5.3 Keep the active-turn primary action available as stop when composer text is empty.
- [x] 5.4 Keep provider, model, mode controls, and unsupported active-turn attachments disabled while active.
- [x] 5.5 Add Composer tests for idle send, active primary-action steer/stop switching, unsupported active session, and disabled controls.

## 6. Verification

- [x] 6.1 Run targeted Rust tests for workspace-model, app-core, acp-core, codex-acp, and kodex-claude changes.
- [x] 6.2 Run frontend tests covering Composer and affected snapshot/type fixtures.
- [x] 6.3 Run `cargo fmt`, relevant frontend formatting/build checks, and `git diff --check`.
