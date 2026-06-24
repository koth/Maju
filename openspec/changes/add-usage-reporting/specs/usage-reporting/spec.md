## ADDED Requirements

### Requirement: Usage updates are captured from ACP agents
Kodex SHALL map ACP usage notifications into application usage state without exposing raw ACP types to frontend components.

#### Scenario: Agent sends context usage update
- **WHEN** an ACP agent sends a `usage_update` with `used` and `size`
- **THEN** Kodex records `used` as the current context used tokens
- **AND** records `size` as the current context window tokens for the owning session

#### Scenario: Agent sends cost information
- **WHEN** an ACP usage update includes a `cost` field
- **THEN** Kodex ignores the cost field for persistence and UI display

#### Scenario: Agent sends usage metadata
- **WHEN** an ACP usage update includes `kodex.ai/usage` metadata with token breakdown fields
- **THEN** Kodex captures the provided input, output, cache read, cache write, reasoning, total, model, provider, agent, and scope fields that are present

#### Scenario: Agent sends partial usage data
- **WHEN** an ACP usage update only contains context `used` and `size`
- **THEN** Kodex updates context usage
- **AND** does not invent unavailable input, output, cache, or reasoning token values

### Requirement: Live usage is available in the UI snapshot
Kodex SHALL expose render-ready usage state in `UiSnapshot` for the currently visible session.

#### Scenario: Visible session receives usage update
- **WHEN** the visible session receives a usage update
- **THEN** the next UI snapshot includes updated context usage for that session

#### Scenario: Session has no usage data
- **WHEN** a session has not received any usage updates
- **THEN** the UI snapshot represents usage as unavailable rather than zero context pressure

#### Scenario: Session changes model
- **WHEN** usage metadata includes a model id
- **THEN** Kodex attributes that usage to the metadata model id
- **AND** falls back to the session model only when the usage update does not include a model id

### Requirement: Usage events are persisted per session
Kodex SHALL persist usage events under the local session that owns the agent runtime.

#### Scenario: Usage arrives for active session
- **WHEN** the active session receives a usage update
- **THEN** Kodex appends a usage event for that session in SQLite

#### Scenario: Usage arrives for background session
- **WHEN** a background session receives a usage update while another session is visible
- **THEN** Kodex appends the usage event under the background session
- **AND** does not apply it to the visible session's live usage state

#### Scenario: Session is reloaded
- **WHEN** a session with persisted usage events is reloaded
- **THEN** Kodex restores the latest live usage snapshot from persisted usage data

#### Scenario: Session is deleted
- **WHEN** a session is deleted
- **THEN** Kodex deletes usage events owned by that session

### Requirement: Live context usage is shown in the workbench
Kodex SHALL show real-time usage in the existing workbench environment/progress dock.

#### Scenario: Context usage is available
- **WHEN** the current session has context used tokens and context window tokens
- **THEN** the dock shows a usage section with used/window token text
- **AND** shows a progress bar representing context occupancy

#### Scenario: Token breakdown is available
- **WHEN** the current session has input, output, cache, or reasoning token breakdown data
- **THEN** the dock shows current turn and session token totals using the available fields

#### Scenario: Usage is unavailable
- **WHEN** the current session has no usage data
- **THEN** the dock either hides detailed usage values or labels usage as unavailable
- **AND** does not show misleading zero totals

#### Scenario: User clicks composer usage pill
- **WHEN** the composer usage pill is clicked
- **THEN** Kodex opens the environment/progress dock if space allows
- **AND** makes the usage section visible

### Requirement: Historical usage summaries are available
Kodex SHALL provide historical usage summaries without calculating pricing.

#### Scenario: Summary grouped by model
- **WHEN** the user opens usage summaries grouped by model
- **THEN** Kodex shows token totals for each model with available input, output, cache, reasoning, and overall token fields

#### Scenario: Summary grouped by agent
- **WHEN** the user opens usage summaries grouped by agent
- **THEN** Kodex groups usage by agent identifier such as `codex-acp` or `claude-acp`

#### Scenario: Summary filtered by date range
- **WHEN** the user selects a date range such as today, 7 days, or 30 days
- **THEN** Kodex limits the summary to usage events created within that range

#### Scenario: Summary filtered by workspace
- **WHEN** the user views usage for the current workspace
- **THEN** Kodex summarizes only sessions associated with that workspace unless a broader scope is explicitly selected

#### Scenario: Pricing is not shown
- **WHEN** usage summaries are displayed
- **THEN** Kodex does not display cost, price, or billing estimates

### Requirement: Usage reporting degrades gracefully
Kodex SHALL keep normal session operation working when usage data is missing, malformed, or unsupported.

#### Scenario: Agent does not support usage updates
- **WHEN** an agent never sends usage updates
- **THEN** Kodex continues normal message, tool, permission, and file-change behavior
- **AND** marks usage as unavailable

#### Scenario: Usage metadata contains malformed numbers
- **WHEN** a usage update contains non-numeric token fields in metadata
- **THEN** Kodex ignores the malformed fields
- **AND** preserves any valid fields from the same update

#### Scenario: Usage persistence fails
- **WHEN** Kodex fails to persist a usage event
- **THEN** the active session continues processing
- **AND** live usage display may still update from in-memory state
