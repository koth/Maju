## 1. Settings And Secrets

- [x] 1.1 Add web tools settings to app settings with enablement, provider selection, and default disabled behavior.
- [x] 1.2 Add provider secret storage helpers for supported web search provider API keys using existing Kodex secrets storage patterns.
- [x] 1.3 Add settings snapshot fields and Tauri commands for reading/updating web tool enablement and provider key configuration.
- [x] 1.4 Add migration/default tests proving existing settings files load with web tools disabled.

## 2. Internal Web Tools Service

- [x] 2.1 Create a shared web tools module/service with request and response DTOs for `web_search` and `web_fetch`.
- [x] 2.2 Implement the Brave Search provider client with bounded result count, provider error mapping, and testable HTTP abstraction.
- [x] 2.3 Implement Kodex-managed public HTTP/HTTPS fetch with redirects, timeout, response-size limits, and readable markdown/text extraction.
- [x] 2.4 Implement URL and network safety checks for schemes, embedded credentials, localhost, private ranges, link-local ranges, cloud metadata, and redirect targets.
- [x] 2.5 Add result truncation, chunk metadata, and a small cache for recent search/fetch calls.
- [x] 2.6 Add unit tests for successful search, provider failure, successful fetch, chunking, blocked destinations, redirects, and truncation.

## 3. Managed MCP Adapter

- [x] 3.1 Implement a Kodex-managed local MCP adapter exposing `web_search` and `web_fetch` tool schemas backed by the internal web tools service.
- [x] 3.2 Add adapter startup, health checking, local access token generation, and log redaction.
- [x] 3.3 Ensure provider keys remain only in Kodex-managed storage and are never sent in agent-visible MCP server configuration.
- [x] 3.4 Add integration tests that call the MCP adapter tools without using real external network providers.

## 4. ACP Session Injection

- [x] 4.1 Extend `acp-core::SessionConfig` to carry managed MCP server definitions prepared by app-core.
- [x] 4.2 Include managed MCP servers in both ACP `session/new` and `session/load` requests.
- [x] 4.3 Update local session startup to inject the web tools adapter only when enabled and fully configured.
- [x] 4.4 Add remote workspace connection support or fail-closed feedback when the remote agent cannot reach the local web tools adapter.
- [x] 4.5 Add acp-core and app-core tests for enabled, disabled, missing-key, resume, and remote-unavailable cases.

## 5. Agent Adapter Behavior

- [x] 5.1 Verify Codex ACP consumes the injected MCP server and maps web tool calls/results to search or read-style Kodex tool events.
- [x] 5.2 Verify Claude Agent ACP consumes the injected MCP server through existing Claude SDK MCP configuration.
- [x] 5.3 Refine Claude tool mapping for `web_search` and `web_fetch` MCP tool calls so titles and content are concise and recognizable.
- [x] 5.4 Add codex-acp and kodex-claude tests covering injected web tool display and failed tool results.

## 6. Desktop UI

- [x] 6.1 Add settings UI for enabling web tools, selecting a provider, and configuring provider API keys.
- [x] 6.2 Show clear unavailable/configuration feedback when web tools are disabled, missing a key, or unavailable for the current remote session.
- [x] 6.3 Refine conversation tool card title extraction for `web_search` query titles and `web_fetch` URL titles.
- [x] 6.4 Add frontend tests for settings state, save behavior, and web tool card title extraction.

## 7. Verification

- [x] 7.1 Run targeted Rust tests for app-core, acp-core, and the web tools service.
- [x] 7.2 Run targeted tests for codex-acp and kodex-claude adapter changes.
- [x] 7.3 Run frontend tests for settings and tool presentation changes.
- [x] 7.4 Run `cargo fmt`, relevant TypeScript formatting/build checks, and `git diff --check`.

## 8. Tavily Provider

- [x] 8.1 Add Tavily as a supported web search provider with its own API key storage.
- [x] 8.2 Normalize Tavily search responses into Kodex managed `web_search` results.
- [x] 8.3 Add settings UI support for switching between Brave Search and Tavily.
- [x] 8.4 Add targeted backend and frontend tests for Tavily provider selection and search mapping.
- [x] 8.5 Move Web tools controls into a dedicated settings pane.
