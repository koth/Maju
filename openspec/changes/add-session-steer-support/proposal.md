## Why

Kodex sessions currently treat every composer submission as a new turn, so users cannot add instructions to an agent while it is already thinking or waiting on tool activity. This makes both Codex ACP and Claude ACP feel stuck when the user needs to correct direction, add constraints, or answer an implicit follow-up without cancelling the turn.

## What Changes

- Add session steering as a first-class prompt flow for active turns.
- Allow the Composer to submit an additional user message while a steer-capable session is running.
- Preserve the active turn lifecycle when steering: do not create a second local prompt task, reset plan state, or incorrectly mark the session idle.
- Route active-turn prompts through Codex ACP and Claude ACP as steer/handoff input.
- Surface steering failures as non-blocking feedback while keeping the existing turn alive when possible.
- Keep mode, model, and provider controls disabled while a turn is active.

## Capabilities

### New Capabilities

- `session-steering`: Active sessions can accept additional user prompt content during an in-flight turn and deliver it to steer-capable ACP agents.

### Modified Capabilities

- None.

## Impact

- `crates/workspace-model`: expose whether a session supports steering in prompt input capabilities.
- `crates/app-core`: add a steer path that appends user input to the current timeline without replacing the in-flight prompt.
- `crates/acp-core`: handle `SendPrompt` commands during an active prompt loop and coordinate prompt completion ownership.
- `codex-acp`: map concurrent prompt requests to Codex core turn steering and report non-steerable states cleanly.
- `kodex-claude`: validate the existing pending-message handoff path and advertise/support steering behavior.
- `apps/desktop/ui`: update the Composer primary action so active sessions send "追加指令" when text is present and stop the turn when text is empty.
- Tests across backend, ACP agents, and Composer UI.
