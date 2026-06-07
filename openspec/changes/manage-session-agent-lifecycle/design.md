## Context

Kodex currently keeps one `SessionHandle` in `Application`. Bootstrap, session switch, session create, and reconnect all replace that one handle. Local session history is durable in SQLite and `acp_session_id` lets ACP `session/load` restore an agent-side session when a live process no longer exists.

That was enough when only the visible session mattered. The clearer model is now: one local conversation maps to one ACP agent runtime while it is active. Switching the visible conversation should not imply cancelling another session's running prompt. The app needs to retain live runtimes per local session, poll them until work finishes, and reclaim idle background agents after a grace period.

## Goals / Non-Goals

**Goals:**

- Maintain a per-local-session registry of ACP agent runtimes.
- Ensure the visible session has a live runtime, either reused from the registry or restored through ACP `session/load`.
- Let switched-away in-flight prompts continue until they naturally complete.
- Show lightweight session-list feedback for background sessions that are still running or have finished while unviewed.
- Reclaim idle background runtimes after 10 minutes without being viewed.
- Persist background events to the correct local session and keep visible UI consistent.
- Shut down all live runtimes when the workspace/application closes.

**Non-Goals:**

- Do not introduce cross-workspace agent sharing.
- Do not persist runtime registry state across app restarts; only `acp_session_id` and SQLite history remain durable.
- Do not add a user-facing multi-agent process manager in this change; session-list indicators are status feedback, not process controls.
- Do not change ACP protocol behavior; use existing `session/new` and `session/load`.
- Do not support simultaneous prompts into the same local session.

## Decisions

### Decision: Introduce a per-session runtime registry inside `Application`

Replace the single `session: SessionHandle` field with a registry keyed by local session UUID. A runtime entry owns:

- `SessionHandle`
- agent command/env metadata
- local session id
- in-flight prompt task, if any
- last viewed timestamp
- idle-since timestamp
- runtime status such as Active, BackgroundRunning, BackgroundIdle, Retiring
- session-list attention state such as None, RunningInBackground, CompletedUnviewed, NeedsAttention

The visible `ui.session.id` remains the selected local session. Helper methods resolve the active runtime from that id.

Rationale: This makes ownership explicit. A conversation that is still working keeps its agent process even when the UI switches away.

Alternative considered: Keep one handle and serialize switching until the current prompt finishes. Rejected because it prevents users from inspecting other sessions while a long-running prompt is active.

### Decision: Switching away backgrounds the runtime instead of replacing it

When the user switches sessions:

1. Mark the outgoing runtime as last viewed now.
2. If it has an in-flight prompt, keep polling it in the background.
3. If it is idle, mark it background-idle and eligible for timed shutdown.
4. Load the target session's local SQLite snapshot into the visible UI.
5. If the target runtime exists and is alive, attach the visible UI to it without ACP restore.
6. If it does not exist, start a runtime using the stored agent label/command and `acp_session_id` when available.

Rationale: Reusing live runtimes avoids unnecessary `session/load`, preserves current prompt state, and matches user expectations that opening a conversation returns to its agent.

### Decision: Background polling persists events before UI projection

All runtime events must carry or be processed with their owning local session id. Background prompt events update SQLite and runtime metadata even when they are not immediately rendered in `ui.messages` or `ui.tools`. When a background session becomes visible again, the UI reloads its snapshot from SQLite and overlays any live runtime state that is still in memory.

Rationale: Existing persistence assumes events apply to the visible `ui.session.id`. That must change before multiple runtimes can be safe.

Alternative considered: Keep only active-session polling and allow background process output to queue. Rejected because long-running agents could block, fill buffers, or lose timely persistence.

### Decision: Session list is the lightweight background work surface

Session rows expose enough runtime metadata for users to understand background work without opening a process manager:

- A switched-away session with an in-flight prompt shows an animated circular progress indicator in the session list.
- When that background prompt completes before the session is viewed, the spinner is replaced by a small completed/unviewed dot.
- Opening the session clears the completed/unviewed dot after pending events are drained and the visible snapshot is rebuilt.
- If a background runtime reaches a permission request or another state requiring user attention, it is marked as needing attention and waits for the user to open that session before resolving the request.

The active visible session continues to use the existing timeline/composer status. Its row may show normal selection state, but it does not need the background spinner or completed-unviewed dot.

Rationale: This matches the mental model that background sessions can keep working, while giving a clear affordance when work is still running or has produced unseen results.

### Decision: Idle shutdown is timer-driven and only for background idle runtimes

The idle grace period defaults to 10 minutes. A runtime becomes eligible only when:

- it is not the visible session
- it has no in-flight prompt
- all pending ready events have been drained and persisted
- the session has not been viewed during the grace period

Opening the session before the deadline cancels the pending retirement. When shutdown happens, the stored `acp_session_id` remains available for a future `session/load`.

Rationale: This balances resource usage against fast switching. Ten minutes is long enough for normal comparison/review workflows without keeping every agent indefinitely.

### Decision: Reconnect targets only the visible session runtime

Manual reconnect should restart or restore the visible session's runtime. It should not disturb other live background runtimes unless the app is shutting down.

Rationale: Reconnect is currently a user command for the active conversation. Changing all runtimes would be surprising and risky.

### Decision: Cancellation remains session-scoped

Cancel from the composer should affect the visible session's in-flight prompt. Background sessions continue unless the user switches to that session and cancels, or a future UI exposes explicit background cancellation.

Rationale: This avoids accidental cancellation of work the user is not looking at.

## Risks / Trade-offs

- Background events could be persisted under the wrong local session. -> Add an event envelope or runtime context that always includes owner session id; tests must cover background completion while another session is visible.
- Session-list indicators could become stale if they are derived only in the UI. -> Keep background runtime/completion metadata in app-core session summaries and emit updated session lists after background progress changes.
- Background permission requests could interrupt the visible session if routed globally. -> Treat them as owner-session events, mark the background session as needing attention, and resolve only after the user opens that session or a future explicit background control exists.
- Multiple live agents increase CPU/memory usage. -> Apply the 10-minute background idle shutdown and shut down all runtimes on workspace close.
- Switching back while a background prompt is still running can show stale UI if SQLite reload lags. -> Drain pending events for the target runtime before projecting the visible snapshot.
- Existing tests assume one `SessionHandle`. -> Refactor tests around runtime registry helpers and add deterministic clock controls for idle timeout.
- Provider/profile settings may change while a background agent is alive. -> Existing per-session agent label/provider checks should remain session scoped; live runtimes continue with the config they were started with.

## Migration Plan

1. Introduce runtime registry types and compatibility helpers while keeping current single-runtime behavior.
2. Move current `session`, `in_flight_prompt`, and runtime config state into the active runtime entry.
3. Update session switching to reuse or restore per-session runtimes.
4. Add background polling and event persistence by owner session id.
5. Add idle shutdown checks with a test-controlled clock.
6. Remove obsolete single-runtime assumptions after tests cover switch/reconnect/create/delete.

## Open Questions

- Should the 10-minute idle grace period be configurable in settings immediately, or a constant in v1?
- Should the completed/unviewed dot clear immediately on opening the session, or only after the user scrolls/lands on the newest result?
- Should the background attention state use the same visual dot with a different color, or a distinct icon when the blocked state is permission-related?
- On app quit, should Kodex wait briefly for background in-flight prompts, or shut them down immediately like today?
