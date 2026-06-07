## 1. Runtime Ownership Model

- [x] 1.1 Introduce `SessionRuntime` and `SessionRuntimeRegistry` types in `app-core` keyed by local session id.
- [x] 1.2 Move `SessionHandle`, in-flight prompt task, agent command, pending model restore, runtime timestamps, and session-list attention state into the runtime entry.
- [x] 1.3 Add helper methods for resolving the visible session runtime and for starting a runtime from stored session metadata.
- [x] 1.4 Preserve current single-visible-session behavior through compatibility helpers before changing switch semantics.

## 2. Session Switch And Restore Semantics

- [x] 2.1 Update bootstrap to create the first visible session runtime through the registry.
- [x] 2.2 Update session create to create a new local session and immediately attach a new runtime with `session/new`.
- [x] 2.3 Update session switch to background the outgoing runtime, load the target SQLite snapshot, and reuse an existing live target runtime when present.
- [x] 2.4 Update session switch to start a new target runtime with `session/load` when no live runtime exists and `acp_session_id` is available.
- [x] 2.5 Update session delete to shut down and remove the deleted session's runtime before deleting local persistence.

## 3. Prompt And Event Routing

- [x] 3.1 Route prompt send, cancel, permission resolution, mode/model/config changes, and reconnect through the visible session runtime.
- [x] 3.2 Make event persistence accept an owning local session id instead of assuming `ui.session.id`.
- [x] 3.3 Poll background runtimes so switched-away in-flight prompts continue and persist completion events.
- [x] 3.4 When switching back to a background session, drain its pending runtime events and rebuild visible UI from persisted state plus live runtime state.
- [x] 3.5 Keep background permission requests owner-scoped: mark that session as needing attention and do not surface the prompt as belonging to the visible session.

## 4. Idle Retirement

- [x] 4.1 Add a testable clock or time provider for runtime last-viewed and idle-since timestamps.
- [x] 4.2 Mark background runtimes idle only after in-flight work completes and pending events are drained.
- [x] 4.3 Shut down background idle runtimes after the 10-minute grace period while retaining `acp_session_id` for future `session/load`.
- [x] 4.4 Cancel pending retirement when a background runtime becomes visible before the timeout.

## 5. Tauri State And Shutdown

- [x] 5.1 Ensure `AppState` polling advances active and background runtimes without emitting non-visible UI as the active snapshot.
- [x] 5.2 Shut down every live runtime when the workspace or application exits.
- [x] 5.3 Emit updated session-list summaries when background prompts start, finish, need attention, or have their runtimes retired.

## 6. Session List Indicators

- [x] 6.1 Add lightweight session-list DTO fields in `workspace-model` for background progress, completed-unviewed state, and attention-needed state.
- [x] 6.2 Maintain those fields in `app-core` from runtime lifecycle events and clear completed-unviewed state when the session is opened.
- [x] 6.3 Update `SessionList` to show an animated circular progress indicator for switched-away sessions with in-flight prompts.
- [x] 6.4 Update `SessionList` to replace the progress indicator with a small completed/unviewed dot after background completion and keep it visible across runtime retirement.
- [x] 6.5 Add focused frontend tests for spinner display, completed-dot display, and clearing the dot on session open.

## 7. Tests And Verification

- [x] 7.1 Add app-core tests proving live runtime reuse avoids `session/load` when switching back quickly.
- [x] 7.2 Add app-core tests proving switched-away in-flight prompts complete and persist under their original local session.
- [x] 7.3 Add app-core tests proving background idle runtimes retire after 10 minutes and restore through `session/load` on reopen.
- [x] 7.4 Add tests proving cancel/reconnect affect only the visible session runtime.
- [x] 7.5 Add tests proving session-list metadata transitions from background-running to completed-unviewed and clears when opened.
- [x] 7.6 Add tests proving background permission requests mark only their owning session as needing attention.
- [x] 7.7 Run `cargo test -p app-core --lib -- --test-threads=1`.
- [x] 7.8 Run `cargo test -- --test-threads=1` if the registry refactor touches shared workspace behavior.
