## Context

The current prompt flow assumes one submitted composer prompt owns one local in-flight turn. `app-core` rejects a second prompt while `in_flight_prompt` is set, `acp-core` ignores `SendPrompt` commands while an active prompt loop is draining events, and the Composer turns the primary send button into a stop control during streaming or tool waits. This prevents users from adding corrective instructions to an active Codex ACP or Claude ACP session.

Codex core already has turn steering semantics through active-turn user input, and `kodex-claude` already has a pending-message handoff path for prompts submitted while a prompt is running. Kodex should expose that behavior as a local capability and make the UI path explicit.

## Goals / Non-Goals

**Goals:**

- Let steer-capable sessions accept additional user prompt content while a turn is active.
- Keep a single local turn lifecycle for the active request, including status, plan, tool state, file-change tracking, and persistence.
- Ensure prompt completion is owned by the latest active prompt/steer chain so handoffs do not make the UI go idle too early.
- Provide clear non-blocking feedback when a steer request cannot be accepted.
- Update Composer controls so the primary action switches between steering and stopping based on whether active-turn text is present.
- Cover Codex ACP and Claude ACP with focused tests.

**Non-Goals:**

- Do not add multi-turn concurrency or parallel independent prompts within one session.
- Do not make unknown ACP agents steerable by default.
- Do not change provider/model/mode switching while a turn is active.
- Do not require changes to the public ACP protocol for the first implementation.

## Decisions

### Add a local session steering capability

Add `session_steer` to Kodex prompt input capabilities, defaulting to `false`. Managed Codex ACP and Claude ACP sessions advertise it as `true`; unknown ACP agents remain text/image/context-only unless they explicitly gain support later.

Alternatives considered:

- Infer support from all agents. This risks sending active-turn prompts to agents that interpret them as unsupported concurrent requests.
- Extend upstream ACP first. That is cleaner long term, but it blocks local support even though both managed agents can implement the behavior today.

### Treat steer as part of the current local turn

When `app-core` receives prompt content while `in_flight_prompt` exists and `session_steer` is enabled, it appends a user message to the current session timeline and forwards the content through the existing session handle. It does not create a second `PromptTask`, clear the agent plan, reset current turn ownership, or set a new `current_turn_user_message_id`.

Alternatives considered:

- Reuse the normal prompt path and create a second in-flight prompt. This would race the shared ACP event stream and corrupt status transitions.
- Reject active prompts in the UI only. This preserves current behavior but does not solve the user need.

### Let `acp-core` process active `SendPrompt` commands

The active prompt loop must no longer ignore `RuntimeCommand::SendPrompt`. Instead, it sends a concurrent ACP prompt request to the same session and tracks prompt ownership with a generation or handoff token. The latest accepted prompt/steer chain owns the final turn status; stale completions from earlier prompt calls are ignored or recorded without setting the UI idle.

Alternatives considered:

- Queue steer text until the first prompt finishes. This is too late for steering and does not match Codex core behavior.
- Cancel and restart the turn. This loses tool context and user intent.

### Surface steer rejection without interrupting the active turn

If a steer request is rejected immediately by the agent or runtime, Kodex records a system-level feedback item and keeps the existing turn running when possible. Rejections include no active steerable turn, non-steerable turn types such as review/compact, unsupported agent capability, and transport failure.

Alternatives considered:

- Treat steer rejection as turn failure. This would punish the original active turn for an optional correction.
- Silently drop rejected steer input. This makes users think the instruction was accepted.

### Composer uses one active-turn primary action

When the session is active and supports steering, the text input remains enabled. With non-empty input, the primary action sends an additional instruction; with empty input, the same primary action stops the current turn. Mode/model/provider controls and unsupported attachments remain disabled while active. The first implementation can restrict active steering to text and embedded context; attachment support can follow once the runtime path is proven.

Alternatives considered:

- Keep the existing stop-as-send-button behavior. This hides steering behind a control state that prevents sending.
- Add a second stop button. This makes the active Composer harder to understand because the same control row exposes two competing primary actions.
- Allow all controls while active. Mode/model changes during a turn still have separate consistency risks.

## Risks / Trade-offs

- [Risk] Prompt completion races between initial prompt and steer prompt can mark the UI idle early. -> Mitigation: add generation ownership tests in `acp-core` and treat stale completions as non-final.
- [Risk] Unknown agents may not support concurrent prompts. -> Mitigation: gate the UI and app-core steer path behind `session_steer`.
- [Risk] Codex core only accepts steering for specific active turn types. -> Mitigation: propagate non-steerable errors as non-blocking user feedback.
- [Risk] Claude handoff may briefly return `end_turn` from the first prompt while the second prompt continues. -> Mitigation: make prompt ownership explicit in the runtime.
- [Risk] Active-turn attachments can introduce extra edge cases. -> Mitigation: start with text/context steering and keep attachments disabled until follow-up support is designed.

## Migration Plan

This change is additive. Existing sessions and unknown agents keep their current behavior because `session_steer` defaults to `false`. Managed Codex ACP and Claude ACP sessions become steer-capable after the app and agents are updated. Rollback is possible by disabling the capability advertisement, which returns Composer and app-core to the current active-turn rejection behavior.

## Open Questions

- Should image/file attachments be accepted during active steering in the same release, or remain disabled until a follow-up?
- Should steer rejections be represented as a dedicated `ClientEvent` variant or as a system message built from existing message events?
- Should local steering capability eventually move into an ACP protocol extension once the behavior stabilizes?
