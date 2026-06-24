## Context

Kodex currently exposes cancellation as a visible-turn operation. `session_cancel` sends ACP `session/cancel`, cancels pending permission waiters, and marks every pending/running tool in the current turn as interrupted. This is useful as a last resort, but it is too broad for long-running tool calls: stopping one shell command or one agent-owned search should not necessarily discard the whole assistant response.

The runtime already has some pieces needed for narrower cancellation:

- `acp-core` owns client-side terminal processes through `TerminalManager`, including `kill_terminal`.
- The prompt loop polls `RuntimeCommand` during an in-flight prompt, so a stop command can be delivered while the agent is still working.
- Codex and Claude ACP agents are controlled by Kodex, so they can emit Kodex-specific stop metadata and support cooperative aborts for agent-owned tools.
- CodeBuddy has separate interruption/background-task semantics, so this change should not require CodeBuddy to adopt the new contract.

## Goals / Non-Goals

**Goals:**

- Add a single user-facing "stop tool" action that targets one running tool call.
- Keep the frontend API centered on `tool_call_id`, not terminal ids or raw ACP protocol details.
- Support terminal-backed tools by terminating the associated client-managed terminal process.
- Support Codex/Claude agent-owned tools through a cooperative private stop request/notification.
- Support pending permission tools by resolving or cancelling only that permission request when the stopped item is a permission wait.
- Fall back to the existing turn cancel only when a precise stop handle is unavailable.
- Keep CodeBuddy on the existing behavior in this change.

**Non-Goals:**

- Do not add a generic process manager UI.
- Do not expose raw terminal ids or process ids to React as the control surface.
- Do not require standard ACP schema changes.
- Do not guarantee precise stop for third-party agents that do not emit Kodex stop metadata.
- Do not solve cancellation of CodeBuddy background tasks in this change.

## Decisions

### Decision: Model cancellation as tool-level execution handles

Introduce a runtime-local `ToolExecutionRegistry` keyed by ACP `tool_call_id`. Each entry records one or more stoppable handles:

- `Terminal { terminal_id }`
- `Permission { request_id }`
- `AgentOwned { tool_call_id, cancel_method }`

The registry lives inside the ACP runtime, not in the UI. It is updated by client request handlers, ACP session update mapping, and permission broker registration. `workspace-model::ToolInvocation` only needs user-facing state such as `can_stop`, `stop_kind`, and optional stop status.

Rationale: terminal is only one resource type. A registry lets the same UI action stop terminal processes, pending permission waits, and agent-owned long-running work without adding one frontend API per resource type.

Alternative considered: expose `terminal_id` on `ToolInvocation` and wire the stop button directly to terminal termination. Rejected because it only handles Bash and leaks runtime implementation details into the frontend.

### Decision: UI sends `stop tool_call_id`, backend resolves the handle

Add a Tauri command such as `session_stop_tool(tool_call_id)` that routes to the visible session runtime. `app-core` calls `SessionHandle::stop_tool(tool_call_id)`, which sends `RuntimeCommand::StopTool { tool_call_id, reply_tx }`. The prompt loop already checks command messages during active prompts, so this command can be handled without waiting for the turn to finish.

The runtime stops the best available handle in this order:

1. Permission wait for the same tool, if present.
2. Terminal/process handle, if present.
3. Agent-owned cooperative cancel, if supported.
4. Fallback to existing `session/cancel` only when requested by app-core/frontend policy.

Rationale: a single backend command gives the UI predictable behavior and lets backend policy evolve without UI churn.

### Decision: Codex and Claude agents emit Kodex stop metadata

Codex and Claude ACP agents should mark stoppable tools using private metadata in normal tool updates. The exact wire shape can be implemented with whichever metadata surface the current ACP crate supports, but the semantic payload must include:

- `tool_call_id`
- stop kind: `terminal` or `agent_owned`
- for terminal tools, the `terminal_id` returned by `terminal/create`
- for agent-owned tools, enough information for the agent runtime to find its abort handle

The concrete Kodex-private metadata key is `_meta["kodex.ai/toolStop"]`:

```json
{
  "toolCallId": "call-id",
  "stopKind": "agent_owned"
}
```

Terminal-backed tools may additionally include:

```json
{
  "toolCallId": "call-id",
  "stopKind": "terminal",
  "terminalId": "terminal-id"
}
```

When a client asks to stop an `agent_owned` handle, it sends the private ACP notification `kodex.ai/tool_stop` with `sessionId` and `toolCallId`. The notification is intentionally private to Kodex-owned agents and must not be assumed by third-party agents.

If standard `terminal/create` cannot carry `tool_call_id`, the agent should register the link after `terminal/create` returns by emitting a tool update with `kodex.ai/terminalId`. The client maps that metadata into the runtime registry and updates the UI stop availability.

Rationale: the current `CreateTerminalRequest` path in Kodex does not assume a tool id. Emitting explicit metadata from our own agents avoids brittle "current running Bash" matching.

### Decision: Agent-owned cancellation is cooperative

Codex and Claude agents should maintain their own `tool_call_id -> AbortHandle` registry for long-running internal tools. When Kodex sends the private cancel request/notification, the agent aborts the matching tool and emits a normal tool failed/interrupted update for that tool.

Rationale: client-side process killing only works for tools hosted by the client, such as terminal commands. Search, indexing, network calls, or internal MCP-style work must be stopped at the agent.

Alternative considered: always send `session/cancel` for non-terminal tools. Rejected because it preserves the current broad behavior and does not meet the single-tool stop requirement.

### Decision: Stop state is projected, not persisted as a durable runtime contract

`ToolInvocation` may persist lightweight fields such as `can_stop: false` after completion or `stop_status: interrupted`, but runtime handles themselves are in-memory only. On session reload, no historical tool should remain stoppable unless a live runtime reattaches and re-emits availability.

Rationale: terminal/process handles are not valid across app restarts or runtime retirement. Persisting raw handles would create stale controls.

### Decision: CodeBuddy stays on existing cancellation behavior

For CodeBuddy tools, the stop button should keep using the current turn/interruption cancellation path unless CodeBuddy later exposes reliable per-tool/background-task cancellation metadata.

Rationale: CodeBuddy background tasks can outlive the visible tool card and may not be represented by Kodex-owned processes. Treating them as terminal-backed would create false precision.

## Risks / Trade-offs

- Agent metadata may arrive after the tool appears running. -> Show no precise stop action until `can_stop` is true; keep the existing turn cancel available at the composer level.
- Stopping a terminal may not terminate child process groups on every platform. -> Implement process-group termination where supported and keep a kill escalation fallback in `TerminalManager`.
- Agent-owned aborts may leave partial tool output. -> Require the agent to emit a normal interrupted/failed tool update after accepting the stop request.
- Permission cancellation can race with user approval. -> Make permission resolution idempotent and treat late responses as no-ops after a stop.
- Fallback to turn cancel can still be broad. -> Make fallback visually distinct and only use it when no precise handle exists.
- Runtime handles can become stale after completion/release. -> Remove registry entries on tool completion, terminal release, terminal exit, session finish, and runtime shutdown.
- Different agents may support different stop capabilities. -> Gate `can_stop` by actual runtime metadata and agent support, not by tool name alone.

## Migration Plan

1. Add the runtime `ToolExecutionRegistry` and stop command plumbing with no frontend behavior change.
2. Register permission wait handles and terminal handles, then emit stop availability events to the reducer.
3. Add `ToolInvocation` stop metadata and Tauri/frontend stop action behind capability checks.
4. Add Codex/Claude agent metadata and cooperative cancel support for agent-owned tools.
5. Switch the running tool stop button to call `session_stop_tool` first, with explicit fallback to `session_cancel` only when no precise handle exists.
6. Add tests for terminal stop, permission stop, agent-owned stop, stale-handle cleanup, and CodeBuddy fallback.

## Open Questions

- Should fallback-to-turn-cancel be a separate secondary action when no precise handle exists, or should the same button ask for confirmation before broad cancellation?
- Which private ACP method name should Codex/Claude use for cooperative stop: a request that waits for acknowledgement, or a notification that only asks the agent to abort?
- Should background session tool stop be exposed from the session list later, or remain limited to the visible session for this change?

## Implementation Notes

- `codex-acp` currently receives a session-level `session/cancel` and forwards it as `Op::Interrupt`. The inspected Codex thread API does not currently expose a `tool_call_id -> AbortHandle` or `interrupt tool` primitive, so wiring 3.3-3.6 requires a lower-level Codex core extension rather than reusing `Op::Interrupt`.
- `kodex-claude` currently delegates cancellation to `session.query.interrupt()`. The inspected Claude Agent SDK type only exposes turn-level `interrupt()`, so true Claude agent-owned tool stop requires SDK/runtime support for aborting a single `toolUseID`; otherwise it would be a broad turn cancel and should not be advertised as precise stop.
- Existing Codex/Claude `terminal_info` metadata is display metadata. It must not be treated as a client-managed `TerminalManager` handle unless the agent explicitly links a `terminal/create` result or emits Kodex private stop metadata.
