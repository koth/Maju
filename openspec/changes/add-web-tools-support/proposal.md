## Why

Kodex sessions currently lack reliable web search and web fetch tools, so both Codex ACP and Claude Agent ACP are forced to answer time-sensitive or URL-based questions without live context. Users expect the coding agent to verify current docs, releases, issues, and web pages without manually pasting content into the conversation.

## What Changes

- Add a Kodex-owned Web Tools capability that provides `web_search` and `web_fetch` to managed agent sessions.
- Implement the tool execution as an internal Kodex web service with shared provider configuration, key management, caching, result limits, and network safety checks.
- Expose the internal web service to Codex ACP and Claude Agent ACP through an automatically injected local MCP adapter.
- Use Brave Search API as the default search provider.
- Use Kodex-managed HTTP fetch and readable-content extraction for page fetches, with room for optional hosted extraction fallback later.
- Render web tool activity in the existing conversation tool timeline with useful titles and compact output.
- Keep the first implementation additive; no existing agent, prompt, or model behavior is removed.

## Capabilities

### New Capabilities

- `web-tools`: Managed Kodex sessions can search the public web and fetch public web page content through Kodex-controlled tools exposed to agents.

### Modified Capabilities

- None.

## Impact

- `crates/app-core`: store web tool settings, resolve provider secrets, construct the managed web tool configuration, and add the injected MCP server to session startup.
- `crates/acp-core`: carry managed MCP server definitions through `session/new` and `session/load` for local and remote sessions.
- `codex-acp`: consume the injected MCP server through its existing MCP support and ensure web tool calls map cleanly to Kodex tool cards.
- `kodex-claude`: consume the injected MCP server through existing Claude SDK MCP support and improve web tool display mapping where needed.
- `apps/desktop/src-tauri`: host or supervise the local web tool adapter and expose settings commands for provider/key configuration.
- `apps/desktop/ui`: add settings affordances for enabling web tools and configuring the search provider key; refine tool card titles for web search/fetch.
- New internal web tooling code for provider clients, fetch extraction, cache, rate limits, and SSRF/private-network protection.
- Tests across app-core, acp-core, agent adapters, UI settings/tool cards, and the web service.
