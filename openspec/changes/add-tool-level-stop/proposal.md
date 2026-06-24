## Why

Kodex can currently cancel the visible turn, but users need to stop a single long-running tool call without necessarily aborting the entire agent response. This matters most for Codex and Claude ACP agents, where Kodex owns both the client runtime and agent implementation and can support precise tool-level cancellation.

## What Changes

- Add a tool-level stop capability for running Codex and Claude ACP tool calls.
- Introduce a backend execution-handle model that links a `tool_call_id` to stoppable resources such as terminal processes, permission waits, and agent-owned tool abort handles.
- Add a session command and Tauri API for stopping a specific tool call.
- Update the frontend tool cards so a running stoppable tool exposes a stop action that targets that tool first.
- Fall back to visible-turn cancellation only when a tool has no precise stop handle or the active agent does not support tool-level stop.
- Keep CodeBuddy behavior at the current level for this change: CodeBuddy tools continue using the existing turn/interruption path instead of the new precise stop contract.

## Capabilities

### New Capabilities

- `tool-call-stop`: Running tool calls can expose stoppable execution handles and be stopped independently from the rest of the turn when supported by the active agent/runtime.

### Modified Capabilities

## Impact

- `crates/acp-core`: add runtime commands, client events, and execution-handle tracking for terminal-backed and agent-owned tools.
- Codex ACP agent and Claude ACP agent wrappers: add cooperative tool abort registration and a private cancellation request/notification path.
- `crates/app-core`: persist/project stop metadata into `ToolInvocation`, route stop requests to the owning session runtime, and mark stopped tools consistently.
- `crates/workspace-model`: add optional tool stop capability fields without exposing raw ACP protocol types.
- `apps/desktop/src-tauri`: add a command to stop a tool call in the visible session.
- `apps/desktop/ui`: show precise tool stop controls and fallback behavior in `ToolCallCard`.
- Tests: cover terminal-backed stop, agent-owned stop, unsupported fallback, and CodeBuddy staying on the existing behavior.
