## ADDED Requirements

### Requirement: Running tools expose stop availability
Kodex SHALL expose whether a running tool call can be stopped precisely without exposing raw ACP terminal ids, process ids, or agent-internal abort handles to the frontend.

#### Scenario: Precise handle is registered
- **WHEN** the active runtime registers a stoppable execution handle for a running tool call
- **THEN** the corresponding `ToolInvocation` is projected as stoppable
- **AND** the frontend can request stop by `tool_call_id`

#### Scenario: No precise handle exists
- **WHEN** a running tool call has no registered execution handle
- **THEN** the corresponding `ToolInvocation` is not projected as precisely stoppable
- **AND** the frontend does not receive raw runtime handle details for that tool

#### Scenario: Tool is no longer running
- **WHEN** a tool call completes, fails, is interrupted, or its runtime handle is released
- **THEN** Kodex removes its stop availability from the visible snapshot

### Requirement: Stop requests target a single tool call
Kodex SHALL provide a backend command that attempts to stop one tool call identified by `tool_call_id`.

#### Scenario: User stops a stoppable tool
- **WHEN** the user invokes stop for a running tool with a registered execution handle
- **THEN** Kodex stops the resource associated with that tool call
- **AND** Kodex marks that tool call as interrupted or stopped
- **AND** other running tool calls in the same turn are not marked stopped solely because this tool was stopped

#### Scenario: Stop command is sent during an active prompt
- **WHEN** a prompt is in flight and the user invokes stop for a running tool
- **THEN** the ACP runtime processes the stop command without waiting for the prompt to finish

#### Scenario: Stop command is repeated
- **WHEN** the user invokes stop more than once for the same tool call
- **THEN** Kodex treats the request idempotently
- **AND** does not surface duplicate stopped logs for the same stop operation

### Requirement: Terminal-backed tools can be stopped
Kodex SHALL stop terminal-backed tool calls by terminating the client-managed terminal process associated with the tool.

#### Scenario: Terminal handle is linked to a tool
- **WHEN** a Codex or Claude ACP agent links a returned `terminal_id` to a running tool call
- **THEN** Kodex registers a terminal execution handle for that tool call
- **AND** projects that tool call as stoppable

#### Scenario: User stops a terminal-backed tool
- **WHEN** the user stops a terminal-backed running tool
- **THEN** Kodex invokes the ACP runtime terminal kill path for the linked terminal
- **AND** the tool is marked interrupted or stopped after the kill is accepted

#### Scenario: Terminal already exited
- **WHEN** the user stops a terminal-backed tool whose terminal has already exited
- **THEN** Kodex removes the stale handle
- **AND** does not cancel the entire turn solely because that terminal handle is stale

### Requirement: Permission waits can be stopped independently
Kodex SHALL allow a pending permission wait to be stopped without cancelling unrelated running tools.

#### Scenario: User stops a permission request
- **WHEN** the user stops a tool card representing a pending permission request
- **THEN** Kodex resolves or cancels that permission waiter as rejected/cancelled
- **AND** marks only that permission tool as interrupted or cancelled

#### Scenario: Permission is approved after stop
- **WHEN** a permission waiter has already been stopped
- **THEN** a later approval response for the same request is ignored or treated as a no-op

### Requirement: Codex and Claude support cooperative agent-owned stop
Codex and Claude ACP agents SHALL support a Kodex private tool-level stop path for long-running tools that are owned by the agent rather than by the client terminal manager.

#### Scenario: Agent-owned tool registers stop support
- **WHEN** a Codex or Claude ACP agent starts a long-running agent-owned tool
- **THEN** the agent registers an abort handle keyed by that tool call id
- **AND** emits stop metadata that lets Kodex project the tool as stoppable

#### Scenario: User stops an agent-owned tool
- **WHEN** the user stops a running agent-owned Codex or Claude tool
- **THEN** Kodex sends the private tool-level stop request or notification to the agent
- **AND** the agent aborts the matching tool call
- **AND** the agent emits a normal interrupted or failed update for that tool call

#### Scenario: Agent reports unsupported stop
- **WHEN** the active agent cannot stop the requested agent-owned tool precisely
- **THEN** Kodex leaves the tool running or offers the existing turn-level cancel fallback
- **AND** does not claim that the specific tool was stopped

### Requirement: Unsupported agents fall back safely
Kodex SHALL preserve existing broad cancellation behavior as a fallback for agents or tools that do not support precise stop.

#### Scenario: CodeBuddy running tool is stopped from current UI
- **WHEN** the active agent is CodeBuddy and the user invokes a stop control for a running tool
- **THEN** Kodex uses the existing CodeBuddy turn/interruption cancellation behavior
- **AND** does not attempt to terminate a terminal or background task unless CodeBuddy has provided a precise Kodex-owned handle

#### Scenario: Third-party agent has no stop metadata
- **WHEN** a third-party ACP agent does not provide stop metadata for a running tool
- **THEN** Kodex does not expose precise stop for that tool
- **AND** any cancellation uses the existing visible-turn cancel path

### Requirement: Stop handles are runtime-scoped
Kodex SHALL treat tool execution handles as live runtime state rather than durable session history.

#### Scenario: Session is reloaded from persistence
- **WHEN** a session is loaded from SQLite without a live runtime handle for a historical running tool
- **THEN** Kodex does not project that historical tool as stoppable

#### Scenario: Runtime shuts down
- **WHEN** an ACP runtime shuts down, reconnects, or is retired
- **THEN** Kodex clears all in-memory stop handles owned by that runtime
