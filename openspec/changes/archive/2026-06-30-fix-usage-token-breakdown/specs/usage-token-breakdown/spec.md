## ADDED Requirements

### Requirement: Codex ACP emits full token breakdown per token-count event
The `codex-acp` agent SHALL attach a `kodex.ai/usage` metadata object to each ACP `UsageUpdate` that carries input, cache read, output, reasoning, and total token counts for both the session cumulative usage and the most recent turn delta, sourced from the Codex core `TokenUsageInfo`.

#### Scenario: Token count event with full usage info
- **WHEN** Codex core emits a `TokenCountEvent` whose `info` contains `total_token_usage` and `last_token_usage`
- **THEN** `codex-acp` sends a `UsageUpdate` whose `kodex.ai/usage` meta top-level fields equal `total_token_usage` with `scope` set to `session_total`
- **AND** the meta includes a nested `turn_delta` object whose fields equal `last_token_usage`
- **AND** `cached_input_tokens` is mapped to `cache_read_tokens`
- **AND** `reasoning_output_tokens` is mapped to `reasoning_tokens`
- **AND** `cache_write_tokens` is omitted or null because Codex does not report cache creation separately

#### Scenario: Token count event with no usage info
- **WHEN** Codex core emits a `TokenCountEvent` whose `info` is `None`
- **THEN** `codex-acp` does not send a `UsageUpdate`
- **AND** no usage event is recorded for that event

#### Scenario: Context window reported alongside tokens
- **WHEN** the `TokenCountEvent` info includes `model_context_window`
- **THEN** the `UsageUpdate` `size` equals `model_context_window`
- **AND** the `UsageUpdate` `used` equals `last_token_usage.tokens_in_context_window()` for context occupancy display
- **AND** the `used` value is not reused as the session total token count

### Requirement: Kodex maps both session total and turn delta from one usage update
`acp-core` SHALL parse the `kodex.ai/usage` metadata and emit a `SessionTotal` usage event from the top-level fields and a `TurnDelta` usage event from the nested `turn_delta` object, both sharing the ACP-reported context occupancy.

#### Scenario: Meta contains both scopes
- **WHEN** an ACP `UsageUpdate` arrives with `kodex.ai/usage` containing top-level token fields and a `turn_delta` sub-object
- **THEN** `acp-core` emits one `ClientEvent::UsageUpdated` with `UsageEventScope::SessionTotal` populated from the top-level fields
- **AND** emits one `ClientEvent::UsageUpdated` with `UsageEventScope::TurnDelta` populated from the `turn_delta` fields
- **AND** both events carry the same `context.used_tokens` and `context.window_tokens` from the ACP `used` and `size`

#### Scenario: Meta contains only top-level fields
- **WHEN** an ACP `UsageUpdate` arrives with `kodex.ai/usage` that has no `turn_delta` sub-object
- **THEN** `acp-core` emits only the `SessionTotal` usage event
- **AND** does not emit a `TurnDelta` event

#### Scenario: Meta contains a zero turn delta
- **WHEN** the `turn_delta` sub-object exists but every token field is zero or null
- **THEN** `acp-core` does not emit a `TurnDelta` event
- **AND** still emits the `SessionTotal` event from the top-level fields

#### Scenario: Meta is absent
- **WHEN** an ACP `UsageUpdate` arrives without `kodex.ai/usage` metadata
- **THEN** `acp-core` emits a single usage event that updates only context occupancy
- **AND** leaves all token breakdown fields as unavailable

### Requirement: Session total reflects cumulative consumption, not context occupancy
The reducer and persistence layer SHALL treat `SessionTotal` as the authoritative cumulative token consumption and SHALL NOT reinterpret `ContextSnapshot` scope as a session total.

#### Scenario: SessionTotal event arrives
- **WHEN** the reducer receives a `UsageEvent` with `UsageEventScope::SessionTotal` and non-empty token fields
- **THEN** the live `session_total` is overwritten with the event's token breakdown
- **AND** the per-model summary for that model/provider/agent is overwritten with the same breakdown

#### Scenario: TurnDelta event arrives
- **WHEN** the reducer receives a `UsageEvent` with `UsageEventScope::TurnDelta`
- **THEN** the live `current_turn` is set to the event's token breakdown
- **AND** the event's tokens are added into the live `session_total`
- **AND** the event's tokens are added into the per-model summary

#### Scenario: ContextSnapshot event arrives
- **WHEN** the reducer receives a `UsageEvent` with `UsageEventScope::ContextSnapshot`
- **THEN** only the context occupancy (`used_tokens`, `window_tokens`) is updated
- **AND** `session_total` and `current_turn` token breakdowns are left unchanged
- **AND** the per-model summary token breakdown is left unchanged

#### Scenario: No SessionTotal has arrived yet
- **WHEN** a session has received only `ContextSnapshot` or `TurnDelta` events but no `SessionTotal`
- **THEN** the live `session_total` reflects accumulated `TurnDelta` values, or is unavailable when no delta has arrived
- **AND** the UI does not display context occupancy as the session total

### Requirement: Historical aggregation uses corrected scope semantics
`session-store` SHALL aggregate usage events so that `SessionTotal` and `TurnDelta` contribute to token totals according to their scope, and legacy `ContextSnapshot` rows from before this change remain readable without inflating totals.

#### Scenario: Aggregating new events
- **WHEN** a usage summary is computed from events that use `SessionTotal` and `TurnDelta` scopes
- **THEN** `SessionTotal` events overwrite the group total when they arrive later
- **AND** `TurnDelta` events accumulate additively into the group total
- **AND** `ContextSnapshot` events contribute only context peak, not token totals

#### Scenario: Aggregating legacy rows
- **WHEN** a usage summary includes rows persisted before this change with `scope = context_snapshot` and only `total_tokens` populated
- **THEN** those rows' `total_tokens` are applied as a best-effort session total for the owning session
- **AND** they do not contribute to per-model token totals to avoid double counting
- **AND** no database migration or backfill is required

#### Scenario: Reloaded session restores correct totals
- **WHEN** a session with persisted `SessionTotal` and `TurnDelta` events is reloaded
- **THEN** the restored `session_total` equals the latest `SessionTotal` event, or the accumulation of `TurnDelta` events when no `SessionTotal` exists
- **AND** the restored `current_turn` equals the most recent `TurnDelta` event
- **AND** context occupancy reflects the most recent context snapshot

### Requirement: Usage breakdown displays real per-field values for codex-acp
The desktop UI SHALL show non-empty input, output, cache read, and reasoning token values for active `codex-acp` sessions once token-count events arrive, without code changes to the components.

#### Scenario: Active codex-acp session receives usage
- **WHEN** an active `codex-acp` session receives a token-count event with full usage info
- **THEN** the workbench dock usage section shows non-zero input, output, cache read, and reasoning totals where the agent reported them
- **AND** the "本轮" figure reflects the most recent turn delta
- **AND** the "会话" figure reflects the session cumulative total
- **AND** the context occupancy bar still reflects `used`/`size` independently

#### Scenario: Historical summary for codex-acp sessions
- **WHEN** the user opens a usage summary grouped by model for sessions that produced full breakdown events
- **THEN** the summary shows non-zero input, output, cache read, and reasoning columns for `codex-acp` models
- **AND** the total column matches the agent-provided `total_tokens` rather than context occupancy

#### Scenario: Reasoning tokens are a subset of output
- **WHEN** a `codex-acp` usage event reports both `output_tokens` and `reasoning_tokens`
- **THEN** the UI displays both values as reported
- **AND** does not add `reasoning_tokens` on top of `output_tokens` when computing a display total, instead preferring the agent-provided `total_tokens`
