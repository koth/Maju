## ADDED Requirements

### Requirement: Default Agent List Excludes Goose

The system SHALL remove Goose from the built-in default ACP agent list exposed by settings, install actions, and new-session agent selection.

#### Scenario: Settings snapshot omits Goose

- **WHEN** the settings page loads its agent snapshot
- **THEN** the returned `agents` list SHALL include CodeBuddy, Codex, and Claude when detectable
- **AND** the returned `agents` list SHALL NOT include Goose

#### Scenario: New workspace agent selector omits Goose

- **WHEN** the user opens the new workspace or new session agent selector
- **THEN** Goose SHALL NOT be offered as a selectable agent

#### Scenario: Existing Goose setting migrates

- **WHEN** persisted settings contain Goose as the selected agent
- **THEN** settings load SHALL migrate the selected agent to Codex if Codex ACP is available
- **AND** settings load SHALL migrate the selected agent to CodeBuddy if Codex ACP is unavailable

### Requirement: Provider Profiles Are Exposed Per Agent Family

The system SHALL expose provider profiles separately for Codex and Claude agent families.

#### Scenario: Codex provider profiles are listed

- **WHEN** the settings page loads agent settings
- **THEN** the snapshot SHALL include Codex provider profiles for Default, Venus, DeepSeek, Kimi Code, and Xiaomi Token Plan
- **AND** each Codex profile SHALL include a stable id, display label, proxy kind, configured status, optional selected model, and help text

#### Scenario: Claude provider profiles are listed

- **WHEN** the settings page loads agent settings
- **THEN** the snapshot SHALL include Claude provider profiles for WOA, Venus, DeepSeek, Kimi Code, and Xiaomi Token Plan
- **AND** each Claude profile SHALL include a stable id, display label, proxy kind, configured status, optional selected model, and help text

#### Scenario: Unsupported profile is rejected

- **WHEN** the frontend submits a provider profile id that is not present for the requested agent family
- **THEN** the backend SHALL reject the update with a clear validation error

### Requirement: Settings UI Uses A Shared BYOK Model Pool

The settings page SHALL present Codex and Claude as agent channels, SHALL present Default as a first-class Codex channel, SHALL present Venus as a first-class Codex and Claude channel, SHALL present WOA as a first-class Claude channel, and SHALL present DeepSeek, Kimi Code, Xiaomi Token Plan, and future custom-key services as sources in one shared BYOK model pool.

#### Scenario: Agent settings use tabs

- **WHEN** the user views agent settings
- **THEN** the UI SHALL show CodeBuddy, Codex, and Claude as peer tabs
- **AND** the UI SHALL only render the active tab's agent-specific controls
- **AND** each tab SHALL show that agent's CLI command, detected path, install status, and default-agent action

#### Scenario: Codex channel offers Default, Venus, and BYOK

- **WHEN** the user views Codex connection settings
- **THEN** the UI SHALL offer Default, Venus, and BYOK as Codex channel choices
- **AND** the UI SHALL explain that BYOK uses the shared model pool through the local proxy
- **AND** the UI SHALL NOT render a grid of provider cards for Codex
- **AND** the UI SHALL NOT present DeepSeek, Kimi Code, or Xiaomi Token Plan as Codex channel switches

#### Scenario: Claude channel offers WOA, Venus, and BYOK

- **WHEN** the user views Claude connection settings
- **THEN** the UI SHALL offer WOA, Venus, and BYOK as Claude channel choices
- **AND** the UI SHALL keep WOA login/channel controls for the WOA channel
- **AND** the UI SHALL explain that configured BYOK source models can appear in the Claude BYOK channel
- **AND** the UI SHALL NOT present DeepSeek, Kimi Code, or Xiaomi Token Plan as Claude channel switches

#### Scenario: BYOK source details update

- **WHEN** the user chooses a different BYOK source in the shared model pool editor
- **THEN** the settings page SHALL show that source's status, model defaults, endpoint, help text, and credential replacement input

#### Scenario: Xiaomi Token Plan details

- **WHEN** the user chooses Xiaomi Token Plan in the shared BYOK model pool editor
- **THEN** the settings page SHALL show `MiMo-V2.5-Pro` and `MiMo-V2.5` as available models
- **AND** the Codex profile metadata SHALL use `https://token-plan-cn.xiaomimimo.com/v1` as its OpenAI-compatible base URL
- **AND** the Claude profile metadata SHALL use `https://token-plan-cn.xiaomimimo.com/anthropic` as its Anthropic-compatible base URL
- **AND** Codex model catalog entries SHALL use API slugs `mimo-v2.5-pro` and `mimo-v2.5` while preserving the display names

### Requirement: Codex Profiles Generate Correct Proxy Configuration

Codex provider profiles SHALL generate Codex ACP configuration according to the profile proxy kind.

#### Scenario: Codex default profile preserves unmanaged config

- **WHEN** the user selects the Codex Default profile
- **THEN** Kodex SHALL stop managing custom Codex provider configuration for that profile
- **AND** Codex ACP SHALL use the user's normal Codex configuration

#### Scenario: Codex responses-compatible profile

- **WHEN** the user selects a Codex profile with proxy kind `responses`
- **THEN** Kodex SHALL write Codex configuration using that profile's Responses-compatible base URL, model defaults, and credential environment variable

#### Scenario: Codex completion-to-responses source

#### Scenario: Codex Venus channel

- **WHEN** the user selects or configures the Codex Venus channel
- **THEN** Kodex SHALL write Codex configuration with `model_provider = "venus"`
- **AND** the generated Venus model catalog SHALL include Venus models

#### Scenario: Codex completion-to-responses BYOK source

- **WHEN** the user configures a Codex BYOK source with proxy kind `completion_to_responses`
- **THEN** Kodex SHALL write Codex configuration that points Codex ACP at the shared BYOK proxy endpoint
- **AND** the generated configuration SHALL NOT claim that the upstream provider is natively Responses-compatible
- **AND** the Kimi Code Codex source SHALL use model `kimi-for-coding` and route through Kimi's Anthropic Messages API shape
- **AND** the shared BYOK proxy SHALL route requests to the upstream source that owns the selected model

### Requirement: Claude Channel Exposes Configured BYOK Models

Claude launch configuration SHALL preserve WOA and Venus channel behavior while exposing configured BYOK source models under BYOK.

#### Scenario: Claude WOA profile preserves WOA behavior

- **WHEN** the user selects the Claude WOA profile
- **THEN** Kodex SHALL preserve WOA token path, channel selection, login, refresh, and token status behavior
- **AND** Claude Agent ACP SHALL launch with WOA gateway configuration

#### Scenario: Claude Venus channel

- **WHEN** the user selects or configures the Claude Venus channel
- **THEN** Claude Agent ACP SHALL launch with the Venus profile configuration
- **AND** Venus SHALL NOT be listed as a BYOK source

#### Scenario: Claude native source

- **WHEN** the user configures a Claude BYOK source with proxy kind `claude_native`
- **THEN** Claude Agent ACP SHALL include that source's models in its available model configuration
- **AND** the Kimi Code Claude profile SHALL use the Anthropic-compatible base URL `https://api.kimi.com/coding/`

#### Scenario: Claude completion-to-claude source

- **WHEN** the user configures a Claude BYOK source with proxy kind `completion_to_claude`
- **THEN** Claude Agent ACP SHALL include that source's models in its available model configuration
- **AND** the WOA channel launch SHALL remain selected unless the user explicitly changes the channel

### Requirement: Provider Credentials Are Write-Only And Redacted

Provider API keys and tokens SHALL be write-only through the settings UI and redacted in all snapshots, logs, and errors.

#### Scenario: Configured credential is shown as status only

- **WHEN** a provider profile has a stored credential
- **THEN** the settings snapshot SHALL report that the credential is configured
- **AND** the snapshot SHALL NOT include the raw credential value

#### Scenario: Replacing credential

- **WHEN** the user enters a replacement credential for the selected provider profile
- **THEN** the backend SHALL store the credential for that profile
- **AND** the settings UI SHALL clear the input after successful save

#### Scenario: Error messages redact secrets

- **WHEN** credential save, config generation, or provider selection fails
- **THEN** returned errors and logs SHALL NOT contain raw API keys, access tokens, refresh tokens, or custom auth headers

### Requirement: Existing Provider Settings Migrate To Profiles

The system SHALL migrate existing Codex and Claude provider settings into provider profile selections without losing configured credentials.

#### Scenario: Existing Codex Venus selection migrates

- **WHEN** existing settings select Codex provider `venus`
- **THEN** settings load SHALL select the Codex Venus provider profile
- **AND** any stored Venus credential status SHALL remain configured

#### Scenario: Existing Codex DeepSeek selection migrates

- **WHEN** existing settings select Codex provider `deepseek`
- **THEN** settings load SHALL select the Codex DeepSeek provider profile
- **AND** any stored DeepSeek credential status SHALL remain configured

#### Scenario: Existing Claude WOA settings migrate

- **WHEN** existing settings include Claude WOA channel, token path, or model list
- **THEN** settings load SHALL select the Claude WOA provider profile
- **AND** the WOA channel, token path, token status, and model list SHALL remain available

### Requirement: BYOK Model Sources Apply To New Agent Sessions

New ACP sessions SHALL launch with channel configuration plus all configured BYOK model-source choices.

#### Scenario: New Codex session uses BYOK proxy and model catalog

- **WHEN** the selected agent is Codex and a new session starts
- **THEN** the session launch SHALL use either the Venus channel config or the BYOK Codex provider and model catalog generated from configured sources

#### Scenario: New Claude session includes BYOK models

- **WHEN** the selected agent is Claude and a new session starts
- **THEN** the session launch SHALL keep WOA channel configuration
- **AND** the session launch SHALL include configured BYOK source models in model selection

#### Scenario: Updating BYOK source affects subsequent sessions

- **WHEN** the user adds or replaces a BYOK source credential in settings
- **THEN** existing running sessions SHALL remain unchanged
- **AND** the currently selected Codex or Claude channel SHALL remain unchanged unless the user explicitly changes it
- **AND** subsequently created sessions SHALL include the updated model-source configuration
