## ADDED Requirements

### Requirement: Managed web tool availability
The system SHALL provide managed web tools to supported Kodex agent sessions when web tools are enabled and configured.

#### Scenario: Managed Codex ACP session receives web tools
- **WHEN** web tools are enabled with valid provider configuration
- **AND** a new or resumed session is backed by managed Codex ACP
- **THEN** the session receives the managed `web_search` and `web_fetch` tools

#### Scenario: Managed Claude Agent ACP session receives web tools
- **WHEN** web tools are enabled with valid provider configuration
- **AND** a new or resumed session is backed by managed Claude Agent ACP
- **THEN** the session receives the managed `web_search` and `web_fetch` tools

#### Scenario: Web tools disabled
- **WHEN** web tools are disabled or provider configuration is missing
- **THEN** the system does not expose `web_search` or `web_fetch` to new or resumed sessions

### Requirement: Web search execution
The system SHALL allow managed agents to search the public web through Kodex-controlled search execution.

#### Scenario: Successful search
- **WHEN** an agent calls `web_search` with a valid query
- **THEN** the system sends the query through the configured search provider
- **THEN** the tool result includes bounded search results with titles, URLs, and snippets

#### Scenario: Tavily search provider
- **WHEN** web tools are configured to use Tavily with a valid Tavily API key
- **AND** an agent calls `web_search` with a valid query
- **THEN** the system sends the query through Tavily's search API
- **THEN** the tool result includes bounded Tavily results normalized into the managed web search result shape

#### Scenario: Search result limit
- **WHEN** an agent requests more search results than Kodex allows
- **THEN** the system caps the result count to the configured maximum
- **THEN** the tool result indicates that the output was limited

#### Scenario: Search provider failure
- **WHEN** the configured search provider returns an error or quota failure
- **THEN** the tool result reports the failure without crashing or ending the agent session

### Requirement: Web fetch execution
The system SHALL allow managed agents to fetch public HTTP or HTTPS page content through Kodex-controlled fetch execution.

#### Scenario: Successful fetch
- **WHEN** an agent calls `web_fetch` with a public HTTP or HTTPS URL
- **THEN** the system retrieves the page content
- **THEN** the tool result includes the final URL, title when available, content format, and bounded extracted content

#### Scenario: Chunked fetch
- **WHEN** fetched content exceeds the allowed response size
- **THEN** the system returns a bounded chunk of content
- **THEN** the tool result includes enough metadata for the agent to request a later chunk

#### Scenario: Fetch failure
- **WHEN** the URL cannot be retrieved or content cannot be extracted
- **THEN** the tool result reports the failure without crashing or ending the agent session

### Requirement: Network safety enforcement
The system SHALL prevent web tools from accessing local, private, credential-bearing, or unsupported network targets.

#### Scenario: Blocked local target
- **WHEN** an agent calls `web_fetch` with a localhost, private IP, link-local, cloud metadata, or otherwise blocked network destination
- **THEN** the system rejects the fetch before returning page content
- **THEN** the tool result explains that the destination is blocked by network safety policy

#### Scenario: Blocked redirect target
- **WHEN** a public URL redirects to a blocked network destination
- **THEN** the system rejects the fetch before returning redirected page content
- **THEN** the tool result explains that the redirect target is blocked by network safety policy

#### Scenario: Unsupported scheme
- **WHEN** an agent calls `web_fetch` with a non-HTTP scheme
- **THEN** the system rejects the fetch
- **THEN** the tool result explains that only public HTTP and HTTPS URLs are supported

#### Scenario: Credential-bearing URL
- **WHEN** an agent calls `web_fetch` with a URL that includes embedded credentials
- **THEN** the system rejects the fetch
- **THEN** the tool result explains that credential-bearing URLs are not supported

### Requirement: Credential and configuration control
The system SHALL keep web provider credentials and web tool settings under Kodex control.

#### Scenario: Provider key stored outside session payload
- **WHEN** a user configures a web search provider key
- **THEN** the key is stored in Kodex-managed secrets storage
- **THEN** the key is not written into conversation messages, tool inputs, or agent-visible session configuration

#### Scenario: Provider keys remain separate
- **WHEN** a user configures keys for multiple supported web search providers
- **THEN** each provider key is stored under that provider's own secret entry
- **THEN** switching providers does not reuse another provider's key

#### Scenario: Missing provider key
- **WHEN** web tools are enabled but the required provider key is missing
- **THEN** the system reports that web tools are not configured
- **THEN** the system does not expose partially configured web tools to the agent session

#### Scenario: Dedicated settings pane
- **WHEN** a user opens Kodex settings
- **THEN** web tool enablement, provider selection, and provider key entry are available from a dedicated Web Tools pane
- **THEN** those controls are not embedded in the general settings pane

### Requirement: Web tool timeline presentation
The system SHALL present web search and web fetch activity in the conversation timeline using existing tool UI patterns.

#### Scenario: Search tool display
- **WHEN** an agent runs `web_search`
- **THEN** the conversation timeline shows a tool card with a search-oriented title based on the query
- **THEN** the expanded content includes compact source results

#### Scenario: Fetch tool display
- **WHEN** an agent runs `web_fetch`
- **THEN** the conversation timeline shows a tool card with a fetch-oriented title based on the URL
- **THEN** the expanded content includes compact fetched content or failure details

### Requirement: Remote workspace support
The system SHALL support web tools for remote workspace sessions without requiring web provider credentials to be copied to the remote machine.

#### Scenario: Remote managed session
- **WHEN** web tools are enabled with valid provider configuration
- **AND** a managed agent session runs in a remote workspace
- **THEN** the session can access the managed web tools through a Kodex-controlled connection
- **THEN** provider credentials remain stored on the local Kodex host

#### Scenario: Remote web tool connection unavailable
- **WHEN** a remote managed session cannot reach the Kodex-controlled web tool connection
- **THEN** the system does not expose broken web tools to the session
- **THEN** the user receives visible feedback that web tools are unavailable for that session
