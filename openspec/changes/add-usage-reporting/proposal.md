## Why

Kodex already receives usage information from supported ACP agents, but the desktop app drops `usage_update` events before they reach the UI. Users therefore cannot see current context pressure, per-session token use, or historical usage patterns by model.

## What Changes

- Add a usage reporting capability that captures ACP `usage_update` notifications from `codex-acp`, `claude-acp`, and compatible agents.
- Surface real-time context usage in the existing environment/progress dock without adding pricing.
- Add a compact composer usage indicator that links back to the dock details.
- Persist usage events so sessions retain usage history after reload.
- Provide historical summaries grouped by model, agent, workspace, session, and date range.
- Support richer agent-provided usage metadata for input, output, cache, and reasoning token breakdowns when available, while still handling agents that only report `used` and `size`.
- Ignore or discard cost fields from upstream agents; this change is token/count reporting only.

## Capabilities

### New Capabilities
- `usage-reporting`: Capture, persist, and display agent token usage and context window occupancy for live sessions and historical summaries.

### Modified Capabilities

## Impact

- `crates/acp-core/src/mapping.rs`: map ACP `SessionUpdate::UsageUpdate` into a Kodex `ClientEvent`.
- `crates/acp-core/src/events.rs`: add usage event DTOs at the protocol-to-core boundary.
- `crates/workspace-model`: add usage DTOs to `UiSnapshot` and TypeScript bindings consumed by the UI.
- `crates/app-core/src/reducer.rs`: update live snapshot usage state when usage events arrive.
- `crates/app-core/src/application/events.rs`: persist usage events and keep usage state associated with the correct local session.
- `crates/session-store`: add SQLite storage and query helpers for usage events and aggregate summaries.
- `apps/desktop/src-tauri`: expose commands for usage summary queries.
- `apps/desktop/ui/src/features/workbench`: show real-time usage in the environment/progress dock and composer indicator.
- `apps/desktop/ui/src/features/settings` or a related summary surface: show historical usage summaries grouped by model and other dimensions.
- `codex-acp` and `kodex-claude`: optionally enrich `usage_update` `_meta` with detailed token breakdown fields where the underlying agent SDK exposes them.
