## Why

The usage reporting pipeline introduced by `add-usage-reporting` is wired end-to-end (ACP mapping, reducer, SQLite persistence, live dock UI, historical summaries), but the **source** of the data in `codex-acp` discards almost all token detail. `kodex_usage_meta(total_tokens: u64)` only emits `total_tokens` and leaves input, output, cache read, and reasoning fields empty. On top of that, the value it sends is `last_token_usage.tokens_in_context_window()` — i.e. current context-window occupancy — which the Kodex reducer then treats as the session cumulative total. As a result, input/output/cache/reasoning columns are always empty, the "session total" mirrors context pressure instead of real consumption, and the per-turn delta is never reported at all. Users see a single occupancy-flavored number where they expect a real token breakdown.

## What Changes

- Expand `codex-acp` `kodex_usage_meta` to accept the full Codex `TokenUsage` (input, cached input, output, reasoning output, total) for both the **last turn delta** and the **session cumulative** usage, instead of a single `total_tokens: u64`.
- Emit two scopes per token-count event from `codex-acp`: `session_total` (from `total_token_usage`) and `turn_delta` (from `last_token_usage`), so the Kodex reducer no longer has to reinterpret a `context_snapshot` as a session total.
- Map Codex `cached_input_tokens` → Kodex `cache_read_tokens` and Codex `reasoning_output_tokens` → Kodex `reasoning_tokens`. Codex does not separate cache-creation tokens, so `cache_write_tokens` stays `None` for `codex-acp` (correct, not a gap).
- Update `crates/acp-core/src/mapping.rs` `emit_usage_update` to parse the new structured `turn_delta` sub-object and emit a `TurnDelta` `UsageEvent` alongside the `SessionTotal` event, so both reach the reducer without a new transport.
- Remove the `context_snapshot`-as-session-total workaround in `crates/app-core/src/reducer.rs` and `crates/session-store` aggregation, now that a real `session_total` scope arrives. Context occupancy continues to come from ACP `used`/`size` and is unaffected.
- Update agent-side and mapping tests to assert the full breakdown is emitted and consumed; update existing `kodex_usage_meta(123)` test signatures.

## Capabilities

### New Capabilities
- `usage-token-breakdown`: Accurate per-turn and per-session token consumption breakdown (input, output, cache read, reasoning) sourced from agent SDK token-count events, distinct from context-window occupancy.

### Modified Capabilities
<!-- No specs are archived yet (add-usage-reporting is still an active change), so there is no existing spec to modify. This change supersedes the flawed "Agent sends usage metadata" scenario in add-usage-reporting's usage-reporting spec by replacing it with the requirements in usage-token-breakdown. -->

## Impact

- `codex-acp/src/thread.rs`: rewrite `kodex_usage_meta` signature and body to carry full `TokenUsage` for both last and total usage plus a `turn_delta` sub-object.
- `codex-acp/src/thread/prompt_state.rs`: pass `info.last_token_usage` and `info.total_token_usage` into the new meta builder at the `TokenCount` event handler.
- `codex-acp/src/thread/tests.rs`: update the `kodex_usage_meta(123)` test to the new signature and assert all fields.
- `crates/acp-core/src/mapping.rs`: extend `emit_usage_update` to read `turn_delta` and emit a second `ClientEvent::UsageUpdated` with `UsageEventScope::TurnDelta`.
- `crates/acp-core/src/mapping/tests.rs`: add coverage for the structured meta with both scopes.
- `crates/app-core/src/reducer.rs`: drop the `context_snapshot`→`session_total` reassignment hack; rely on real `SessionTotal` events.
- `crates/session-store/src/session_store/mod.rs`: align `session_usage_snapshot_from_events` / summary aggregation with the corrected scope semantics (stop treating `context_snapshot` as cumulative).
- `crates/workspace-model`: no DTO changes required — `UsageTokenBreakdown` and `UsageEventScope` already support all needed fields.
- `apps/desktop/ui`: no type changes; existing UI already renders the breakdown fields, so it will populate automatically once data arrives. Verify the dock and settings pages show non-empty input/output/cache/reasoning.
- Submodule pointer bump: `codex-acp` is a git submodule (origin github.com/koth/kodex-acp.git); the agent-side commit must be pushed there, then the parent repo updates the submodule reference.
