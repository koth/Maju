## ADDED Requirements

### Requirement: Workbench remote open entry point
The system SHALL provide a Workbench entry point for opening a remote directory from a saved remote machine profile.

#### Scenario: Remote profiles exist
- **WHEN** the user chooses to open a remote directory and at least one remote machine profile exists
- **THEN** the system SHALL allow the user to select a saved remote machine profile, provide a remote project path, and choose the Agent for this open

#### Scenario: No remote profiles exist
- **WHEN** the user chooses to open a remote directory and no remote machine profiles exist
- **THEN** the system SHALL guide the user to add a remote machine in Settings

#### Scenario: Sidebar workspace open menu
- **WHEN** the user opens the new workspace menu from the Workbench sidebar
- **THEN** the system SHALL offer local folder opening and remote directory opening as peer choices
- **AND** choosing remote directory SHALL show the guided remote-open flow using saved remote machine profiles or a link to Remote settings

### Requirement: Remote directory open request
The system SHALL open a remote directory by combining a saved remote machine profile with a remote absolute project path, optional one-time SSH password, and an open-time Agent selection.

#### Scenario: Open remote directory successfully
- **WHEN** the user selects a saved remote machine profile, chooses an Agent, and submits a valid remote absolute project path
- **THEN** the system SHALL start the existing remote Linux ACP TCP workspace flow for that machine, path, and Agent
- **AND** the active workspace SHALL identify the remote host and remote project path

#### Scenario: Open with one-time SSH password
- **WHEN** the user enters an SSH password while opening a remote directory
- **THEN** the system SHALL pass the password only to the SSH process for that open request
- **AND** the system SHALL NOT persist the password in profiles, recents, or workspace snapshots

#### Scenario: Missing remote path
- **WHEN** the user attempts to open a remote directory without a remote project path
- **THEN** the system SHALL reject the request before attempting SSH

#### Scenario: Remote open fails
- **WHEN** the remote workspace launch or ACP TCP connection fails
- **THEN** the system SHALL show an actionable error
- **AND** it SHALL leave the current workspace or welcome state unchanged

### Requirement: Remote open validation
The system SHALL allow users to validate a selected machine and remote path before opening the remote workspace.

#### Scenario: Validate before open succeeds
- **WHEN** the user validates a selected remote machine and project path before opening
- **THEN** the system SHALL show successful validation phases for SSH and remote path checks

#### Scenario: Validate before open fails
- **WHEN** validation fails for the selected remote machine or project path
- **THEN** the system SHALL show the failed phase and SHALL NOT open the remote workspace automatically

### Requirement: Local-first welcome experience
The system SHALL keep local project opening as the primary welcome experience while making remote opening a secondary guided path.

#### Scenario: Welcome screen primary action
- **WHEN** the welcome screen is shown
- **THEN** the primary action SHALL open a local folder
- **AND** remote opening SHALL NOT be displayed as a raw SSH login dialog by default

#### Scenario: Welcome remote action
- **WHEN** the user chooses the remote action from the welcome screen
- **THEN** the system SHALL show a guided remote-open flow using saved remote machine profiles or a link to Remote settings

### Requirement: Remote workspace labels and recents
The system SHALL display remote workspace identity distinctly from local workspace identity in Workbench surfaces.

#### Scenario: Remote workspace is active
- **WHEN** a remote workspace is active
- **THEN** Workbench headers and workspace controls SHALL show the remote machine display name or SSH target and the remote project path

#### Scenario: Remote workspace appears in recents
- **WHEN** a remote directory has been opened successfully
- **THEN** recent workspace UI SHALL show it as a remote workspace with host and path information
- **AND** it SHALL NOT treat the remote path as a local filesystem directory

#### Scenario: Remote workspace appears beside local workspaces
- **GIVEN** a local workspace is already open
- **WHEN** the user opens or activates a remote directory workspace
- **THEN** the open workspace/session lists SHALL retain the local workspace entry beside the remote workspace
- **AND** the active workspace SHALL be the selected remote directory

### Requirement: Remote unavailable actions
The system SHALL keep local-only Workbench actions disabled or explicitly unsupported for remote workspaces unless a remote implementation exists.

#### Scenario: Local-only action in remote workspace
- **WHEN** the user invokes a local-only action while a remote workspace is active
- **THEN** the system SHALL return or display an explicit unsupported-remote-workspace result
- **AND** it SHALL NOT resolve the remote path as a local desktop filesystem path
