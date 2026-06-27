## ADDED Requirements

### Requirement: Steer capability advertisement
The system SHALL expose whether the current session supports active-turn steering through prompt input capabilities.

#### Scenario: Managed steer-capable agent
- **WHEN** a session is backed by a managed Codex ACP or Claude ACP agent
- **THEN** the session prompt input capabilities include session steering support

#### Scenario: Unknown agent
- **WHEN** a session is backed by an agent without known steering support
- **THEN** the session prompt input capabilities do not include session steering support

### Requirement: Active-turn steering submission
The system SHALL allow an additional user prompt to be submitted while a steer-capable session has an active turn.

#### Scenario: Submit steer while streaming
- **WHEN** the session is streaming and the user submits non-empty composer text
- **THEN** the system appends the text as user input in the current session timeline
- **THEN** the system forwards the prompt content to the active ACP session as steering input

#### Scenario: Submit steer while waiting for tool
- **WHEN** the session is waiting for a tool or permission response and the user submits non-empty composer text
- **THEN** the system appends the text as user input in the current session timeline
- **THEN** the system forwards the prompt content to the active ACP session as steering input

#### Scenario: Steering unsupported
- **WHEN** the session has an active turn but does not advertise steering support
- **THEN** the system does not send a steering request to the agent
- **THEN** the system keeps the active turn unchanged

### Requirement: Current turn preservation
The system SHALL treat steering input as part of the current local turn rather than as a new independent prompt task.

#### Scenario: Steering does not replace turn state
- **WHEN** steering input is accepted during an active turn
- **THEN** the system preserves the existing in-flight prompt owner
- **THEN** the system does not clear the active agent plan
- **THEN** the system does not reset current tool, permission, or file-change ownership for the active turn

#### Scenario: Steering does not prematurely finish the turn
- **WHEN** an earlier prompt request completes after a later steering request has been accepted
- **THEN** the system does not mark the session idle from the stale completion
- **THEN** the session remains active until the latest accepted prompt or steering chain completes

### Requirement: Agent steering behavior
The system SHALL route active-turn prompt input to each managed agent using that agent's native steering or handoff behavior.

#### Scenario: Codex ACP regular turn
- **WHEN** Codex ACP receives a prompt request while a regular Codex turn is active
- **THEN** it sends the prompt content to Codex core as active-turn steering input

#### Scenario: Codex ACP non-steerable turn
- **WHEN** Codex ACP receives a prompt request while Codex core reports the active turn is not steerable
- **THEN** it rejects the steering request with a clear non-steerable reason

#### Scenario: Claude ACP pending handoff
- **WHEN** Claude ACP receives a prompt request while its prompt loop is already running
- **THEN** it pushes the user message into the active Claude input stream
- **THEN** prompt completion ownership remains with the handoff prompt instead of the stale original prompt completion

### Requirement: Steering failure feedback
The system SHALL report steering rejection without failing the existing active turn when the original turn can continue.

#### Scenario: Immediate rejection
- **WHEN** a steering request is rejected before being accepted by the agent
- **THEN** the system shows non-blocking feedback explaining that the additional instruction was not accepted
- **THEN** the existing active turn remains active if it was otherwise still running

#### Scenario: Transport failure
- **WHEN** forwarding steering input to the active ACP session fails because the session transport is unavailable
- **THEN** the system reports the failure to the user
- **THEN** the system does not create a new local prompt task for the failed steering input

### Requirement: Composer active-turn controls
The Composer SHALL use one active-turn primary action that sends steering text when text is present and stops the turn when text is empty.

#### Scenario: Active steer-capable session with text
- **WHEN** the session is active, steering is supported, and composer text is non-empty
- **THEN** the primary composer action sends the text as an additional instruction

#### Scenario: Active steer-capable session without text
- **WHEN** the session is active, steering is supported, and composer text is empty
- **THEN** the Composer keeps the text input available for additional instructions
- **THEN** the primary composer action stops the current turn

#### Scenario: Active session control restrictions
- **WHEN** the session has an active turn
- **THEN** provider, model, and mode controls remain disabled
- **THEN** unsupported active-turn attachments remain disabled
