## Context

`add-usage-reporting` shipped the full usage pipeline: ACP `SessionUpdate::UsageUpdate` → `ClientEvent::UsageUpdated` → reducer `apply_usage_update` → SQLite `usage_events` → live dock + historical summaries. The DTO layer (`UsageTokenBreakdown`, `UsageEventScope`, `UsageEvent`) already supports input/output/cache_read/cache_write/reasoning/total and three scopes. So the contract is fine; the **data feeding it** is wrong.

The single source for `codex-acp` is `EventMsg::TokenCount(TokenCountEvent { info })` in `codex-acp/src/thread/prompt_state.rs`. Codex core hands over a `TokenUsageInfo` with:

- `total_token_usage: TokenUsage` — cumulative session consumption (input, cached_input, output, reasoning_output, total)
- `last_token_usage: TokenUsage` — the most recent request delta
- `model_context_window: Option<i64>` ��� context window size

The current handler extracts only `last_token_usage.tokens_in_context_window()` (== `last_token_usage.total_tokens`, i.e. context occupancy) and passes that single `u64` to `kodex_usage_meta`. `kodex_usage_meta` then builds `json!({"scope":"context_snapshot","total_tokens":used})`, dropping every other field. Downstream, `reducer.rs` has a compensating hack that reinterprets this `context_snapshot` as `session_total`. Net effect: input/output/cache/reasoning are always `None`, "session total" tracks context pressure, and the per-turn delta never exists.

`claude-acp` (`kodex-claude`) already emits a richer `kodex.ai/usage` meta via task 6.1 of `add-usage-reporting`, so its breakdowns land correctly. This change brings `codex-acp` to parity and removes the reducer workaround.

## Goals / Non-Goals

**Goals:**

- Surface real input, output, cache read, and reasoning token counts for `codex-acp` sessions, sourced from Codex core `TokenUsage`.
- Distinguish per-turn delta (`last_token_usage`) from session cumulative (`total_token_usage`) so the dock's "本轮" and "会话" figures are both meaningful.
- Keep context-window occupancy (`used`/`size`) accurate and decoupled from consumption totals.
- Remove the `context_snapshot`→`session_total` reassignment hack in reducer and session-store so the data model is self-consistent.
- Stay backward compatible: older in-flight events and other agents that still send `context_snapshot` must not crash or produce negative numbers.

**Non-Goals:**

- No new DTOs. `UsageTokenBreakdown` / `UsageEventScope` already cover this.
- No UI changes; existing dock and settings components already render the breakdown fields and will populate once data arrives.
- No `codex_api_proxy` rewrite. That proxy normalizes chat-completions usage for the responses-API shim, which is a separate ingestion concern; aligning it is tracked separately.
- No cache-write token reporting for `codex-acp`. Codex/OpenAI models fold cache creation into `input_tokens` and do not expose a separate cache-write count, so `cache_write_tokens` stays `None` — this is correct, not a gap.
- No backfill of historical sessions; only new token-count events are enriched.

## Decisions

### Decision: Emit two scopes from one token-count event

`kodex_usage_meta` will accept `&TokenUsage` for both last and total, plus `model_context_window`, and produce a single `kodex.ai/usage` meta object carrying:

- top-level token fields populated from `total_token_usage` with `scope: "session_total"`
- a nested `turn_delta` object carrying the same fields from `last_token_usage`

`emit_usage_update` in `mapping.rs` reads the meta, emits one `UsageEvent` with `UsageEventScope::SessionTotal` (top-level fields), and a second `UsageEvent` with `UsageEventScope::TurnDelta` (from the nested `turn_delta`). Both reuse the same `context.used_tokens`/`window_tokens` from ACP `used`/`size`.

Rationale: Codex delivers last+total in one event. Splitting into two `UsageEvent`s lets the existing reducer branches (`SessionTotal` overwrites, `TurnDelta` accumulates) work unchanged, and the session-store aggregation already handles both scopes correctly. One meta object keeps the ACP payload compact and atomic.

Alternative considered: Add a fourth `UsageEventScope::TurnAndTotal` carrying both. Rejected because it would force every consumer to split a hybrid scope, and the reducer already has clean per-scope branches.

### Decision: Field mapping Codex → Kodex

| Codex `TokenUsage` field | Kodex `UsageTokenBreakdown` field | Notes |
|---|---|---|
| `input_tokens` | `input_tokens` | Includes cached input per Codex semantics |
| `cached_input_tokens` | `cache_read_tokens` | Cache hit reads |
| `output_tokens` | `output_tokens` | Includes reasoning output per Codex semantics |
| `reasoning_output_tokens` | `reasoning_tokens` | Subset of output, reported separately for display |
| `total_tokens` | `total_tokens` | Codex-provided total |
| — | `cache_write_tokens` | Stays `None`; Codex does not separate cache creation |

`total_tokens` fallback arithmetic in `usage_tokens_from_meta` stays as-is: when the meta lacks `total_tokens`, sum the present parts. Because `codex-acp` now always provides `total_tokens`, the fallback rarely triggers for it but still protects other agents.

Rationale: Mirrors how `claude-acp` already maps its fields, so summaries are comparable across agents. Codex's `input_tokens` already includes cached input, so we must NOT add `cache_read_tokens` into a naive `input + output` total — `usage_total_tokens` already prefers the agent-provided `total_tokens` when present, which is correct.

Alternative considered: Derive a "non-cached input" field. Rejected; `UsageTokenBreakdown` has no slot for it and the agent-provided `total_tokens` is authoritative.

### Decision: Drop the `context_snapshot`-as-session-total workaround

`reducer.rs` currently special-cases `UsageEventScope::ContextSnapshot` to overwrite `session_total` (with a comment admitting it is a hack). Once `codex-acp` sends a real `session_total`, this hack is wrong: it would let any genuine context snapshot clobber the cumulative total. Remove that branch so `ContextSnapshot` only updates `context` occupancy (its true meaning), and `SessionTotal`/`TurnDelta` own the token totals. Apply the same change to `session_usage_snapshot_from_events` and `usage_overview_from_events` / `usage_timeseries_from_events` in session-store.

Rationale: The workaround existed solely to compensate for `codex-acp` mislabeling. Fixing the source makes the workaround harmful rather than helpful.

Alternative considered: Keep accepting `context_snapshot` as a fallback total when no `session_total` has arrived. Rejected because it reintroduces the exact semantic confusion we are removing; occupancy is not consumption.

### Decision: Keep `codex-acp` `scope` as `session_total`, not `context_snapshot`

The top-level meta `scope` becomes `"session_total"` because the fields now describe cumulative consumption. Context occupancy continues to flow through the standard ACP `UsageUpdate.used`/`size` fields, which `emit_usage_update` already maps to `UsageContextSnapshot` regardless of `scope`. So no `context_snapshot` scope is emitted by `codex-acp` at all.

Rationale: `scope` should describe what the token fields mean, not what the ACP `used`/`size` mean.

### Decision: Submodule commit ordering

`codex-acp` is a git submodule (origin `github.com/koth/kodex-acp.git`, a fork of `zed-industries/codex-acp`). The agent-side changes (`thread.rs`, `prompt_state.rs`, `tests.rs`) are committed and pushed inside the submodule first, then the parent repo records the new submodule SHA. The Kodex-side `mapping.rs`/`reducer.rs`/`session-store`/tests land in the parent repo in the same logical change but can be committed before or after the pointer bump as long as CI builds against the bumped SHA.

Rationale: Keeps the fork's history self-contained and reviewable; the parent repo only tracks a SHA, not submodule file contents.

Alternative considered: Vendor `codex-acp` into the parent repo. Rejected — it would diverge from upstream and complicate future syncs.

## Risks / Trade-offs

- [Double-counting turn deltas] `last_token_usage` from Codex is per-request and `total_token_usage` is cumulative, so emitting both is correct. If a future Codex version makes `last_token_usage` also cumulative, `TurnDelta` accumulation would inflate. → Mitigation: the `SessionTotal` branch overwrites (not adds), so even if `TurnDelta` drifts, the session total stays correct; add a test asserting `last ≤ total` to catch regressions.
- [Reducer change breaks `context_snapshot`-only agents] Removing the workaround means an agent that only ever sends `context_snapshot` (no `session_total`) would show zero session total. → Mitigation: `claude-acp` already sends proper scopes; `codex-acp` is fixed by this change; for unknown agents, showing "unavailable" is more honest than showing occupancy-as-total. Document this in the spec.
- [Stale historical rows] Existing `usage_events` rows from before this change have only `total_tokens` and `scope=context_snapshot`. After the reducer change, those rows no longer feed `session_total` on reload. → Mitigation: `session_usage_snapshot_from_events` is updated so legacy `context_snapshot` rows still contribute their `total_tokens` to a best-effort session total (read-only compatibility), while new rows use correct scopes. No migration/backfill needed.
- [Submodule push dependency] Parent-repo CI fails if it references a submodule SHA not yet pushed to origin. → Mitigation: push the submodule commit before updating the parent pointer; the tasks.md enforces this order.
- [Field naming confusion] Codex `cached_input_tokens` vs Kodex `cache_read_tokens` vs Anthropic `cache_read_input_tokens`. → Mitigation: `usage_u64_field` already accepts many aliases; the mapping table above is the single source of truth and is documented in the spec.

## Migration Plan

1. In the `codex-acp` submodule: rewrite `kodex_usage_meta` signature, update the `TokenCount` handler in `prompt_state.rs`, update `thread/tests.rs`. Commit and push to the fork's origin.
2. In the parent repo: extend `emit_usage_update` in `mapping.rs` to parse `turn_delta` and emit two events; add mapping tests.
3. In the parent repo: remove the `context_snapshot`→`session_total` reassignment in `reducer.rs`; add a read-only compatibility path in `session_usage_snapshot_from_events` for legacy rows.
4. In the parent repo: align `session-store` summary/timeseries/overview aggregation with the corrected scopes.
5. Update the parent repo's `codex-acp` submodule pointer to the pushed SHA.
6. Run `cargo test` for affected crates and `codex-acp` tests; run UI build to confirm no type regressions.
7. Rollback: revert the parent repo commit (which reverts both Kodex code and the submodule pointer). The submodule change itself is backward compatible because richer meta is additive — older Kodex consumers ignore `turn_delta` and still read top-level fields.

## Open Questions

- Should `emit_usage_update` skip the `TurnDelta` event when `last_token_usage` is all-zero (e.g. first event of a turn)? Lean yes, to avoid a zero-row in SQLite; confirm during implementation.
- Should the legacy `context_snapshot` compatibility path in session-store be time-boxed (removed after N months) or permanent? Lean permanent-until-proven-unnecessary since it is a few lines.
- Does `kodex-claude` also need the `turn_delta` nesting for consistency, or is its current flat meta sufficient? Out of scope here, but worth a follow-up if cross-agent summary parity matters.
