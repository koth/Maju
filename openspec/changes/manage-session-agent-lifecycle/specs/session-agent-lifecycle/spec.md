## ADDED Requirements

### Requirement: Visible session has a live agent runtime
Kodex SHALL ensure the currently opened local session is associated with a live ACP agent runtime.

#### Scenario: Opening a session with an existing runtime
- **WHEN** the user opens a local session whose ACP agent runtime is still alive
- **THEN** Kodex reuses that runtime without sending ACP `session/load`

#### Scenario: Opening a session without an existing runtime
- **WHEN** the user opens a local session whose runtime is not alive and the session has a persisted `acp_session_id`
- **THEN** Kodex starts the session's configured agent and restores through ACP `session/load`

#### Scenario: Opening a local session with no persisted ACP id
- **WHEN** the user opens a local session with no persisted `acp_session_id`
- **THEN** Kodex starts the session's configured agent through ACP `session/new`

### Requirement: Switching sessions backgrounds active agent work
Kodex SHALL keep a switched-away session's ACP runtime alive while its prompt is still in flight.

#### Scenario: Switch away during an in-flight prompt
- **WHEN** the user switches from session A to session B while session A has an in-flight prompt
- **THEN** session A's ACP runtime remains alive and continues processing
- **AND** session B becomes the visible session with its own live runtime

#### Scenario: Background prompt completes
- **WHEN** a background session's in-flight prompt finishes
- **THEN** Kodex persists its messages, tool updates, file changes, status, and `acp_session_id` under that background local session

### Requirement: Session list reports background work state
Kodex SHALL expose lightweight per-session runtime and attention state for session list rows without exposing raw ACP protocol types to the frontend.

#### Scenario: Background session is still working
- **WHEN** the user switches from session A to session B while session A has an in-flight prompt
- **THEN** the session list row for session A shows an animated circular progress indicator while session A continues running in the background

#### Scenario: Background session finishes before being viewed
- **WHEN** session A is running in the background and its in-flight prompt finishes while another session is visible
- **THEN** the session list row for session A stops showing the progress indicator
- **AND** shows a small completed/unviewed dot until session A is opened

#### Scenario: Completed background session is opened
- **WHEN** the user opens a background session that has a completed/unviewed dot
- **THEN** Kodex drains pending events for that session and rebuilds the visible snapshot
- **AND** clears the completed/unviewed dot for that session

#### Scenario: Runtime retirement preserves unviewed completion
- **WHEN** a background session finishes, shows a completed/unviewed dot, and its idle runtime is later reclaimed
- **THEN** the completed/unviewed dot remains visible in the session list until the user opens that session

#### Scenario: Background session needs user attention
- **WHEN** a background runtime reaches a permission request or another state that requires user input
- **THEN** Kodex marks that session as needing attention in the session list
- **AND** does not display the background session's permission prompt as if it belonged to the currently visible session

### Requirement: Background idle runtimes are reclaimed
Kodex SHALL shut down a background session's ACP runtime after it has been idle and unviewed for the configured idle grace period.

#### Scenario: Background idle runtime exceeds grace period
- **WHEN** a background session has no in-flight prompt and has not been viewed for 10 minutes
- **THEN** Kodex shuts down that session's ACP runtime
- **AND** retains the local SQLite history and persisted `acp_session_id`

#### Scenario: Background session is viewed before grace period
- **WHEN** a background idle session is opened before the 10-minute grace period expires
- **THEN** Kodex cancels the pending retirement and reuses the existing live runtime

### Requirement: Session commands target the visible runtime
Kodex SHALL route user prompt, cancel, permission, config, model, mode, and reconnect commands to the runtime owned by the currently visible session.

#### Scenario: Cancel visible session only
- **WHEN** session A is visible and session B has a background in-flight prompt
- **THEN** invoking cancel affects only session A's runtime

#### Scenario: Reconnect visible session only
- **WHEN** the user reconnects the visible session while other background runtimes exist
- **THEN** Kodex restarts or restores only the visible session runtime
- **AND** background runtimes are left unchanged

### Requirement: Workspace shutdown stops all live runtimes
Kodex SHALL shut down every live session runtime for a workspace when that workspace or application closes.

#### Scenario: Workspace closes with multiple live runtimes
- **WHEN** a workspace closes while multiple local sessions have live ACP runtimes
- **THEN** Kodex requests shutdown for each live runtime and clears in-memory runtime ownership
