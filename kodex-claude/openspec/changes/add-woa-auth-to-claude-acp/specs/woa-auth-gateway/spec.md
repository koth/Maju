## ADDED Requirements

### Requirement: Explicit WOA gateway mode

The system SHALL provide an explicit WOA gateway mode for the TypeScript Claude ACP agent that is disabled by default and enabled only by `--woa` or `CLAUDE_ACP_WOA=1`.

#### Scenario: WOA disabled by default

- **WHEN** the user starts the ACP agent without `--woa` and without `CLAUDE_ACP_WOA=1`
- **THEN** the system SHALL preserve existing non-WOA ACP agent behavior
- **AND** the system SHALL NOT inject WOA gateway URL, WOA access token, or WOA custom headers

#### Scenario: WOA enabled by CLI flag

- **WHEN** the user starts the ACP agent with `--woa`
- **THEN** the system SHALL enable WOA gateway mode for ACP sessions
- **AND** the system SHALL validate that a usable WOA token is available before accepting ACP work

#### Scenario: WOA enabled by environment variable

- **WHEN** the user starts the ACP agent with `CLAUDE_ACP_WOA=1`
- **THEN** the system SHALL enable WOA gateway mode for ACP sessions

### Requirement: WOA CLI command handling

The system SHALL provide process-level WOA commands for login, status, and refresh without starting the ACP server.

#### Scenario: Login command starts device flow

- **WHEN** the user runs the ACP executable with `--woa-login`
- **THEN** the system SHALL request a WOA device code from `https://copilot.code.woa.com/api/v2/auth/device/code`
- **AND** the system SHALL display the verification URL and user code needed for browser authorization

#### Scenario: Status command reports token state

- **WHEN** the user runs the ACP executable with `--woa-status`
- **THEN** the system SHALL print WOA token status and exit
- **AND** the system SHALL NOT start the ACP server

#### Scenario: Refresh command refreshes token

- **WHEN** the user runs the ACP executable with `--woa-refresh`
- **THEN** the system SHALL refresh the stored WOA token and exit
- **AND** the system SHALL NOT start the ACP server

#### Scenario: CLI passthrough remains unchanged

- **WHEN** the user runs the ACP executable with `--cli`
- **THEN** the system SHALL preserve the existing Claude CLI passthrough behavior

### Requirement: WOA OAuth Device Code flow

The system SHALL implement WOA OAuth Device Code authentication natively in TypeScript.

#### Scenario: Login completes successfully

- **WHEN** the user authorizes the device code in the browser
- **THEN** the system SHALL poll the WOA device token endpoint until an access token is returned
- **AND** the system SHALL save the access token, refresh token when present, and expiration time to the configured token path

#### Scenario: Login handles pending authorization

- **WHEN** the WOA device token endpoint returns `authorization_pending`
- **THEN** the system SHALL continue polling until authorization succeeds or the device code expires

#### Scenario: Login handles slow down

- **WHEN** the WOA device token endpoint returns `slow_down`
- **THEN** the system SHALL increase the polling interval up to a maximum of 15 seconds

#### Scenario: Login handles expiry

- **WHEN** the device code expires before authorization completes
- **THEN** the system SHALL fail with an actionable message instructing the user to retry WOA login

### Requirement: WOA token cache compatibility

The system SHALL read and write WOA token files compatible with the existing `claude-woa` JSON format.

#### Scenario: Default token path

- **WHEN** no custom token path is configured
- **THEN** the system SHALL use `~/.claude-woa-token.json` as the token cache path

#### Scenario: Custom token path

- **WHEN** the user provides `--woa-token-path` or `CLAUDE_WOA_TOKEN_PATH`
- **THEN** the system SHALL read and write WOA tokens at the configured token path

#### Scenario: Compatible JSON fields

- **WHEN** the system saves a WOA token
- **THEN** the token file SHALL contain `accessToken`, `refreshToken`, and `expiresAt` fields
- **AND** `expiresAt` SHALL be stored as a millisecond timestamp

#### Scenario: Malformed token file

- **WHEN** the configured token file exists but does not match the expected token shape
- **THEN** the system SHALL fail with a clear malformed-token error

#### Scenario: Secure Unix token permissions

- **WHEN** the system saves a token file on Unix
- **THEN** the system SHALL set the token file permissions to `0600`

### Requirement: Automatic WOA token ensure and refresh

The system SHALL ensure that WOA mode uses a valid access token and refreshes tokens that expire within five minutes.

#### Scenario: Existing token is valid

- **WHEN** WOA mode starts and the cached token expires more than five minutes in the future
- **THEN** the system SHALL use the cached access token without refreshing

#### Scenario: Existing token needs refresh

- **WHEN** WOA mode starts and the cached token expires within five minutes
- **THEN** the system SHALL refresh the token before starting or using a WOA-backed ACP session

#### Scenario: Refresh response omits refresh token

- **WHEN** the WOA refresh endpoint returns a new access token without a new refresh token
- **THEN** the system SHALL preserve the existing refresh token in the saved token file

#### Scenario: Token unavailable

- **WHEN** WOA mode starts and no usable token exists
- **THEN** the system SHALL fail startup with an actionable message instructing the user to run WOA login

#### Scenario: Session creation rechecks token

- **WHEN** a WOA-enabled ACP process creates a session after startup
- **THEN** the system SHALL ensure the WOA token again before constructing Claude Agent SDK session options

### Requirement: WOA channel selection

The system SHALL support `default` and `offline` WOA gateway channels through `--woa-channel` and `CLAUDE_WOA_CHANNEL`.

#### Scenario: Default channel

- **WHEN** no WOA channel is specified
- **THEN** the system SHALL use `default`
- **AND** the gateway URL SHALL be `https://copilot.code.woa.com/server/chat/codebuddy-gateway/codebuddy-code`

#### Scenario: Offline channel

- **WHEN** the user sets the WOA channel to `offline`
- **THEN** the system SHALL use `https://copilot.code.woa.com/server/chat/codebuddy-gateway-offline/codebuddy-code`
- **AND** the custom headers SHALL include `x-channel: offline`

#### Scenario: Invalid channel

- **WHEN** the user sets an unsupported WOA channel
- **THEN** the system SHALL fail with a clear invalid-channel error

### Requirement: WOA custom header construction

The system SHALL build WOA custom headers for each WOA-backed Claude Agent SDK session.

#### Scenario: Header set is built

- **WHEN** a WOA-backed ACP session is created
- **THEN** the system SHALL build newline-separated custom headers containing `x-api-key`, `x-conversation-id`, `x-app-version`, `x-app-name`, `x-request-platform`, `x-scene-name`, `User-Agent`, `x-request-platform-v2`, `x-app-name-v2`, `x-claude-code-internal`, and `x-channel`

#### Scenario: Access token is used as x-api-key

- **WHEN** WOA custom headers are built
- **THEN** `x-api-key` SHALL use the current WOA access token

#### Scenario: Conversation id is session scoped

- **WHEN** a WOA-backed ACP session is created
- **THEN** the custom headers SHALL include an `x-conversation-id` derived from that ACP session id

#### Scenario: Forked session gets distinct conversation id

- **WHEN** a WOA-backed ACP session is forked
- **THEN** the forked session SHALL receive a distinct `x-conversation-id`

### Requirement: WOA environment injection

The system SHALL inject WOA gateway environment variables into Claude Agent SDK options for WOA-backed sessions.

#### Scenario: WOA environment is applied

- **WHEN** a WOA-backed ACP session is created
- **THEN** the system SHALL set `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `AUTH_TOKEN`, and `ANTHROPIC_CUSTOM_HEADERS` in the Claude Agent SDK session environment

#### Scenario: Nonessential traffic is disabled

- **WHEN** a WOA-backed ACP session is created
- **THEN** the system SHALL set `DISABLE_ERROR_REPORTING`, `DISABLE_TELEMETRY`, `DISABLE_AUTOUPDATER`, `DISABLE_COST_WARNINGS`, and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` to `1`

#### Scenario: WOA overrides generic gateway values

- **WHEN** WOA mode is enabled and generic gateway auth values are also present
- **THEN** the WOA gateway URL, WOA access token, and WOA custom headers SHALL take precedence for the Claude Agent SDK session environment

#### Scenario: ACP session state events remain enabled

- **WHEN** WOA environment is applied
- **THEN** the system SHALL preserve `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`

#### Scenario: Model configuration remains user-controlled

- **WHEN** WOA mode is enabled and the user configures model or thinking settings
- **THEN** the system SHALL preserve existing model and thinking configuration behavior while applying WOA gateway authentication

### Requirement: WOA terminal auth method

The system SHALL expose a WOA login terminal auth method when WOA mode is enabled and the ACP client supports terminal auth.

#### Scenario: Terminal auth supported

- **WHEN** WOA mode is enabled and the client advertises terminal auth support
- **THEN** the agent SHALL include a WOA login auth method that runs the ACP executable with `--woa-login`

#### Scenario: Terminal auth unsupported

- **WHEN** WOA mode is enabled and the client does not advertise terminal auth support
- **THEN** the agent SHALL NOT require terminal auth support to run WOA-backed sessions when a valid token already exists

### Requirement: Safe WOA output and logging

The system SHALL redact WOA secrets from CLI output, errors, and logs.

#### Scenario: Login and refresh redact secrets

- **WHEN** WOA login or refresh succeeds
- **THEN** the system SHALL NOT print full access tokens or refresh tokens

#### Scenario: Status uses masked tokens

- **WHEN** token status is displayed
- **THEN** token values SHALL be masked so the complete secret cannot be reconstructed from output

#### Scenario: Session errors redact headers

- **WHEN** WOA environment creation or session creation fails
- **THEN** error output SHALL NOT include raw `ANTHROPIC_CUSTOM_HEADERS`, full access tokens, or full refresh tokens

### Requirement: WOA documentation

The system SHALL document native WOA gateway mode for users and ACP client configuration.

#### Scenario: README documents commands

- **WHEN** a user reads the project README
- **THEN** the README SHALL explain `--woa-login`, `--woa-status`, `--woa-refresh`, `--woa`, `--woa-channel`, and `--woa-token-path`

#### Scenario: README documents environment variables

- **WHEN** a user reads the project README
- **THEN** the README SHALL document `CLAUDE_ACP_WOA`, `CLAUDE_WOA_CHANNEL`, and `CLAUDE_WOA_TOKEN_PATH`

#### Scenario: README documents security and compliance

- **WHEN** a user reads the WOA documentation
- **THEN** the README SHALL state that the token file contains sensitive credentials and that WOA gateway use may be subject to internal compliance rules
