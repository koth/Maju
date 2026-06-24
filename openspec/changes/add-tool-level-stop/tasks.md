## 1. Runtime Stop Model

- [x] 1.1 Add a runtime-local `ToolExecutionRegistry` in `acp-core` keyed by ACP `tool_call_id`.
- [x] 1.2 Define execution handle variants for terminal, permission wait, and agent-owned cooperative cancellation.
- [x] 1.3 Add `RuntimeCommand::StopTool { tool_call_id, reply_tx }` and `SessionHandle::stop_tool`.
- [x] 1.4 Process `StopTool` inside the active prompt loop without waiting for the prompt result.
- [x] 1.5 Remove registered handles when tools complete, fail, are interrupted, terminals are released, or the runtime shuts down.

## 2. Terminal And Permission Handles

- [x] 2.1 Register terminal handles when Codex/Claude tool metadata links a `tool_call_id` to a `terminal_id`.
- [x] 2.2 Stop terminal-backed tools through `TerminalManager::kill_terminal` and make the operation idempotent for already-exited terminals.
- [x] 2.3 Register permission wait handles when a tool permission request is created.
- [x] 2.4 Add a single-request cancel/reject path to `PermissionBroker` so stopping one permission wait does not cancel all waiters.
- [x] 2.5 Emit client events that project stop availability changes for affected tool calls.

## 3. Codex And Claude Agent Support

- [x] 3.1 Add a private stop metadata contract for Codex/Claude tool updates, including stop kind and terminal id when applicable.
- [x] 3.2 Update Codex/Claude Bash and terminal-display tools to emit cooperative stop metadata, using `agent_owned` unless a real client-managed `terminal/create` id is linked.
- [x] 3.3 Add an agent-side `tool_call_id -> AbortHandle` registry for long-running Codex-owned tools.
- [x] 3.4 Add the matching Claude agent-side abort registry for long-running Claude-owned tools.
- [x] 3.5 Implement a private tool-level cancel request or notification and wire it to the Codex/Claude abort registries.
- [x] 3.6 Ensure aborted agent-owned tools emit normal interrupted/failed tool updates.

## 4. App-Core And Tauri API

- [x] 4.1 Extend `workspace-model::ToolInvocation` with stop availability/status fields that do not expose raw runtime handles.
- [x] 4.2 Update the reducer to apply stop availability events and clear stop state on terminal/tool completion.
- [x] 4.3 Add `Application::stop_tool(tool_call_id)` routed to the visible session runtime.
- [x] 4.4 Add a Tauri command such as `session_stop_tool` that calls the app-core stop path.
- [x] 4.5 Preserve existing `session_cancel` behavior as fallback and avoid changing CodeBuddy-specific interruption handling.

## 5. Frontend UX

- [x] 5.1 Add a frontend API wrapper for `session_stop_tool`.
- [x] 5.2 Update `ToolCallCard` to show a precise stop action only when `tool.can_stop` is true and the tool is running or pending.
- [x] 5.3 Make the stop action call `session_stop_tool(tool.call_id)` and show a transient stopping state.
- [x] 5.4 Keep broad turn cancellation available from the composer, visually separate from precise tool stop.
- [x] 5.5 Ensure CodeBuddy running tools continue to use the current fallback behavior.

## 6. Tests

- [x] 6.1 Add `acp-core` tests for `StopTool` reaching the prompt loop during an in-flight prompt.
- [x] 6.2 Add `acp-core` tests for terminal handle registration, terminal kill, and stale terminal cleanup.
- [x] 6.3 Add `acp-core` tests for stopping one permission request without cancelling unrelated permission waiters.
- [x] 6.4 Add agent tests for Codex/Claude cooperative cancellation metadata and abort behavior.
- [x] 6.5 Add `app-core` reducer tests for stop availability projection and cleanup.
- [x] 6.6 Add frontend tests for `ToolCallCard` precise stop visibility and action dispatch.
- [x] 6.7 Add regression tests proving CodeBuddy still uses the existing fallback path.

## 7. Verification

- [x] 7.1 Run `cargo test -p acp-core`.
- [x] 7.2 Run relevant Codex/Claude agent crate tests after adding cooperative cancellation.
- [x] 7.3 Run `cargo test -p app-core`.
- [x] 7.4 Run frontend tests covering `ToolCallCard`.
- [x] 7.5 Run `npm run build` for `apps/desktop/ui`.
