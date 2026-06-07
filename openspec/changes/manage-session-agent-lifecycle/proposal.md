## Why

Kodex currently models the active workspace as one `Application` with one `SessionHandle`, so switching sessions tears down the mental model of "this conversation owns this live ACP agent." The clearer product model is that each active local conversation maps to its own agent process: the opened conversation should already have a live agent, background conversations should be allowed to finish, and idle background agents should be reclaimed after a short grace period.

## What Changes

- Replace the single active `SessionHandle` ownership model with per-session agent runtime ownership inside `app-core`.
- Treat the live ACP agent as a reclaimable runtime cache for a durable local session, not as the session record itself.
- Keep the currently opened conversation attached to its corresponding live ACP agent whenever possible.
- When switching away from a session with an in-flight prompt, keep that agent running until the prompt finishes instead of cancelling or replacing it.
- Surface background progress in the session list: show an animated progress indicator while a switched-away session is still working, then show a small completed/unread dot when it finishes before the user views it.
- After a background session becomes idle, shut down its agent if it has not been viewed again for a configurable idle grace period, initially 10 minutes.
- When opening a session, reuse its existing live agent if present; otherwise start a new agent and restore via ACP `session/load` when the session has a persisted `acp_session_id`.
- Preserve local SQLite timeline/message/tool reconstruction independently from live agent process lifetime.
- Update session switching, reconnect, cancellation, prompt polling, and shutdown behavior to route commands/events to the correct per-session runtime.

## Capabilities

### New Capabilities
- `session-agent-lifecycle`: Local Kodex sessions own independent ACP agent runtimes that can stay alive while backgrounded, be reclaimed after idle, and be restored through ACP `session/load`.

### Modified Capabilities

## Impact

- `crates/app-core/src/application.rs`: `Application` state changes from a single `SessionHandle` to active-session metadata plus a per-session runtime registry.
- `crates/app-core/src/application/sessions.rs`: session switch/create/delete/reconnect semantics change to reuse, background, retire, or restore per-session agents.
- `crates/app-core/src/application/prompting.rs`: prompt polling and event persistence must target the owning session, including background prompt completion.
- `crates/app-core/src/application/events.rs`: persisted events need to be associated with the event's owning local session, not only the currently visible session.
- `apps/desktop/src-tauri/src/state.rs`: workspace/app polling may need to continue advancing background runtimes while emitting the active UI snapshot.
- `apps/desktop/ui/src/features/session/SessionList.tsx`: session rows need runtime/completion indicators for background running and finished-unviewed sessions.
- `crates/workspace-model`: session list DTOs may need lightweight runtime status and finished-unviewed metadata, without exposing raw ACP types.
- `session-store`: existing `acp_session_id`, messages, tools, and timelines remain the durable source; schema changes are optional unless runtime metadata is persisted.
- Tests: add coverage for live agent reuse on switch, background prompt completion, idle shutdown, session list indicators, and `session/load` restoration after agent retirement.
