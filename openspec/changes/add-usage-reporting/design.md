## Context

Kodex has three relevant layers for usage data:

- ACP agents already emit usage notifications. `claude-acp` emits `usage_update` for streaming context occupancy and final result usage; `codex-acp` emits `UsageUpdate` from token count events.
- `acp-core` currently maps many `SessionUpdate` variants into `ClientEvent`, but does not map `UsageUpdate`, so usage data is dropped before it reaches `app-core`.
- The desktop UI already has a right-side environment/progress dock and a compact composer model area. Those are better fits for usage state than inserting usage messages into the conversation timeline.

The first version should avoid pricing. It should report token counts, context window occupancy, and aggregate usage by useful dimensions such as model, agent, workspace, session, and date range.

## Goals / Non-Goals

**Goals:**

- Capture standard ACP `usage_update` notifications without coupling the UI to raw ACP types.
- Show real-time context usage for the visible session.
- Preserve usage events in SQLite for session reload and historical summaries.
- Support model-level summaries and drilldowns without adding pricing.
- Accept richer token breakdowns from agent `_meta` when present, while keeping a useful experience when only `used` and `size` are available.
- Keep usage state session-scoped so background sessions and visible sessions do not contaminate each other.

**Non-Goals:**

- Do not calculate prices, budgets, or billing estimates.
- Do not require every agent to provide input/output/cache/reasoning breakdowns in v1.
- Do not expose raw ACP payloads to React components.
- Do not add a separate usage daemon or telemetry upload path.
- Do not block turn completion or message rendering if usage persistence fails.

## Decisions

### Decision: Treat ACP `usage_update` as the primary ingestion path

`acp-core` will map `SessionUpdate::UsageUpdate` to a new `ClientEvent::UsageUpdated`. The event should carry:

- `context_used_tokens`
- `context_window_tokens`
- optional token breakdown fields when available from metadata
- optional `model`, `provider`, and `agent_cli`
- optional event `scope`, such as `context_snapshot`, `turn_delta`, or `session_total`

Rationale: Both `codex-acp` and `claude-acp` already send this standard update, so the missing piece is client-side ingestion. This also keeps future ACP-compatible agents on the same path.

Alternative considered: Parse usage from agent text output or logs. Rejected because it is brittle and would mix presentation text with state.

### Decision: Use Kodex metadata for richer breakdowns

The standard ACP usage update is sufficient for context occupancy but not for detailed summaries. Agents that know more should attach a vendor-neutral metadata object:

```json
{
  "kodex.ai/usage": {
    "scope": "turn_delta",
    "model": "claude-opus-4-7",
    "provider": "anthropic",
    "agent_cli": "claude-acp",
    "input_tokens": 12000,
    "output_tokens": 2400,
    "cache_read_tokens": 8000,
    "cache_write_tokens": 400,
    "reasoning_tokens": 900,
    "total_tokens": 23700
  }
}
```

All fields except `scope` are optional. The receiver normalizes missing numbers to `NULL` for storage and `0` only for display arithmetic where appropriate.

Rationale: This keeps the app protocol-agnostic while allowing `claude-acp` and future agents to provide better data. It also avoids depending on Claude-specific field names in `workspace-model`.

Alternative considered: Add many fields directly to the ACP standard update. Rejected because Kodex does not control the ACP schema and can move faster with metadata.

### Decision: Separate live usage snapshot from persisted usage events

`workspace-model` will expose a live `usage` field in `UiSnapshot`. It should be shaped for rendering:

- current context usage: used/window/percent
- current turn totals when known
- current session totals when known
- a small `by_model` summary for the active session

`session-store` will persist append-only `usage_events` rows. Summaries are derived from SQL aggregation rather than written as precomputed totals.

Rationale: The UI needs simple render-ready state, while persistence needs enough detail to answer historical queries. Keeping events append-only also avoids losing details when aggregation requirements change.

Alternative considered: Persist only current session totals on the `sessions` row. Rejected because it cannot support model/date/workspace drilldowns and loses per-turn detail.

### Decision: Right-side dock is the primary live usage surface

The existing environment/progress dock should add a `用量` section between environment information and task progress. It should show:

- context usage, for example `128k / 1M`
- a progress bar based on context window occupancy
- current turn total when known
- current session total
- optional model breakdown summary for the active session

The composer should show only a compact pill near the model selector, such as `128k / 1M`. Clicking it should open the dock and focus the usage section.

Rationale: Usage is contextual operational metadata. The dock already appears during active work when space allows and is not part of the conversation transcript.

Alternative considered: Add usage messages to the timeline. Rejected because usage updates can be frequent and would pollute the conversation.

### Decision: Settings owns historical usage summaries

Add a Settings usage page or section for historical summaries. It should support:

- date ranges: today, 7 days, 30 days, all
- grouping by model, agent, workspace, and session
- totals for input/output/cache/reasoning/overall where available
- context peak per session where available

Rationale: Historical usage is account/workspace metadata rather than active conversation content.

Alternative considered: Put a full usage report in the right review panel. Rejected for v1 because the review panel is already code-change oriented.

### Decision: Cost fields are ignored at ingestion

If an agent includes `cost` in `usage_update`, `acp-core` or `app-core` should not persist or expose it for this change.

Rationale: The user explicitly requested usage without pricing. Avoiding cost fields also prevents confusing or provider-specific billing assumptions.

## Risks / Trade-offs

- Agents may emit duplicate cumulative usage updates -> Store a scope/source/update signature and deduplicate obvious repeats per session when the payload is identical, but do not overfit v1.
- `used` can mean context occupancy, not turn token delta -> Label it as context usage in the UI and require detailed metadata for token breakdown summaries.
- Some agents only provide `used/size` -> Show context usage and mark detailed summaries as unavailable rather than guessing.
- Background sessions may update usage while another session is visible -> Process usage with the owning runtime/session context and persist it under that session.
- High-frequency streaming updates can cause UI churn -> Update the live snapshot on changes but debounce persistence or store only changed values if necessary.
- Schema changes must not break existing session databases -> Add `CREATE TABLE IF NOT EXISTS usage_events` plus indexes in the existing migration flow.
- Model names can change during a session -> Use the model from usage metadata if present, otherwise fall back to the current session model at event time.

## Migration Plan

1. Add `usage_events` table and indexes in `session-store` migrations.
2. Add usage DTOs to `workspace-model` and TypeScript types.
3. Map ACP `UsageUpdate` to `ClientEvent::UsageUpdated`.
4. Update `app-core` reducer and persistence to maintain live usage state and append usage events.
5. Add Tauri commands for historical usage summaries.
6. Add the dock usage section and composer pill.
7. Add Settings usage summary UI.
8. Enrich `claude-acp` and `codex-acp` usage metadata where agent-side source data is available.
9. Backfill is not required; older sessions simply have no historical usage events.

## Open Questions

- Should summaries count cache read/write tokens in `total_tokens` or show them separately only? V1 should show both, with total matching the agent-provided total when available.
- Should the composer pill show context usage only, or session total when context window size is unknown?
- Should usage summaries include archived sessions by default, or only when an "include archived" toggle is enabled?
- How aggressive should persistence deduplication be for streaming context snapshots?
