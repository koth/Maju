## 1. Usage DTOs and ACP Mapping

- [x] 1.1 Add usage structs to `workspace-model` for live context usage, token breakdowns, model summaries, and historical summary rows.
- [x] 1.2 Add matching TypeScript types in `apps/desktop/ui/src/types`.
- [x] 1.3 Add `ClientEvent::UsageUpdated` in `crates/acp-core/src/events.rs`.
- [x] 1.4 Map ACP `SessionUpdate::UsageUpdate` in `crates/acp-core/src/mapping.rs`, including `used`, `size`, and optional `kodex.ai/usage` metadata.
- [x] 1.5 Add acp-core tests covering standard `used/size`, ignored `cost`, valid metadata fields, and malformed metadata fields.

## 2. Application State and Persistence

- [x] 2.1 Add `usage` state to `UiSnapshot` and initialize it for new, loaded, and empty sessions.
- [x] 2.2 Update `crates/app-core/src/reducer.rs` to apply usage updates to the visible session snapshot without affecting messages or timeline.
- [x] 2.3 Add a `usage_events` table and indexes in `crates/session-store` migrations with cascade delete by session.
- [x] 2.4 Add session-store APIs to append usage events, load latest session usage, and query summaries by model, agent, workspace, session, and date range.
- [x] 2.5 Persist usage events from `crates/app-core/src/application/events.rs` under the owning local session.
- [x] 2.6 Restore latest usage snapshot when loading a session from SQLite.
- [ ] 2.7 Add app-core/session-store tests for active session persistence, session reload, delete cascade, and background-session ownership.

## 3. Summary Commands

- [x] 3.1 Add Tauri command DTOs for usage summary filters and grouped summary results.
- [x] 3.2 Implement commands to query usage summaries for current workspace, all workspaces, specific sessions, and date ranges.
- [x] 3.3 Add command bridge tests or Rust command tests for grouping by model and filtering by date range.

## 4. Workbench Live Usage UI

- [x] 4.1 Add a usage section to the environment/progress dock between environment information and progress.
- [x] 4.2 Show context used/window text and occupancy bar when context usage is available.
- [x] 4.3 Show current turn and current session token totals using available breakdown fields without inventing unavailable values.
- [x] 4.4 Add a compact composer usage pill near the model selector.
- [x] 4.5 Make the composer usage pill open the dock and reveal the usage section when space allows.
- [x] 4.6 Add UI tests for available usage, unavailable usage, composer pill behavior, and no pricing display.

## 5. Historical Usage UI

- [x] 5.1 Add a Settings usage page or section for historical summaries.
- [x] 5.2 Add controls for date range and grouping by model, agent, workspace, and session.
- [x] 5.3 Render totals for input, output, cache read, cache write, reasoning, and overall tokens when available.
- [x] 5.4 Show context peak per session or group when context usage events are available.
- [x] 5.5 Ensure archived sessions are excluded by default unless the UI explicitly includes them.
- [x] 5.6 Add UI tests for model grouping, date range filtering, unavailable breakdowns, and no pricing display.

## 6. Agent Metadata Enrichment

- [x] 6.1 Update `kodex-claude` usage updates to attach `kodex.ai/usage` metadata with model, provider, agent, scope, and token breakdowns when available.
- [x] 6.2 Update `codex-acp` usage updates to attach `kodex.ai/usage` metadata where Codex token count events expose detailed fields.
- [x] 6.3 Add agent-side tests verifying metadata is emitted and cost fields are not required by Kodex.

## 7. Validation

- [x] 7.1 Run targeted Rust tests for `acp-core`, `app-core`, and `session-store`.
- [x] 7.2 Run targeted `kodex-claude` tests for usage update metadata.
- [x] 7.3 Run UI unit tests for workbench and settings usage components.
- [x] 7.4 Run `npm run build` in `apps/desktop/ui`.
- [x] 7.5 Run `cargo check -p kodex-desktop` after Rust integration changes.
