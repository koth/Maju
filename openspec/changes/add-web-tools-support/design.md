## Context

Kodex currently starts ACP sessions as a client and delegates model/tool execution to the selected managed agent. Codex ACP and Claude Agent ACP both already accept ACP-provided `mcpServers` during session creation, but `acp-core` currently starts and loads sessions without supplying any MCP server definitions. Codex ACP also already maps native Codex web search events into search tool cards, and Claude Agent ACP already understands Claude web/search/fetch and MCP tool result shapes.

The missing product capability is therefore not just a search API client. Kodex needs one managed web tool surface that is available to both managed agents, shares credentials and network safety policy, and produces consistent UI telemetry. Users should experience the tools as built-in Kodex capabilities; MCP is the narrow adapter used to expose those capabilities to agent runtimes that already understand MCP.

## Goals / Non-Goals

**Goals:**

- Provide `web_search` and `web_fetch` to managed Codex ACP and Claude Agent ACP sessions.
- Keep provider configuration, API keys, safety policy, caching, and telemetry under Kodex control.
- Use one implementation for both agents rather than duplicating web logic in `codex-acp` and `kodex-claude`.
- Inject the web tools automatically when enabled, without requiring users to hand-edit MCP configuration.
- Default search to Brave Search API while allowing users to choose Tavily as another managed provider.
- Fetch public web pages through Kodex-managed HTTP fetch and readable-content extraction.
- Protect users from accidental local, private-network, credential-bearing, or oversized fetches.
- Surface web tool calls in the existing conversation timeline with useful, compact titles and outputs.
- Support local and remote workspaces without requiring remote machines to hold web provider credentials.

**Non-Goals:**

- Do not add a general user-configurable MCP marketplace in this change.
- Do not require users to install or maintain an external MCP server.
- Do not depend on Claude-only or OpenAI-only hosted web tools as the primary implementation.
- Do not support browser automation, authenticated website crawling, form submission, or JavaScript interaction in the first release.
- Do not allow agents to fetch local files, localhost services, cloud metadata endpoints, or private network resources through web tools.
- Do not redesign conversation tool cards beyond the title/output refinements needed for web tools.

## Decisions

### Implement Kodex Web Tools as an internal service with an MCP adapter

Kodex will own a local web tools service that implements provider clients, fetch extraction, cache, rate limiting, and safety checks. ACP sessions will receive this service through an automatically injected MCP server definition. For users this is a built-in Kodex feature; MCP is only the agent-facing protocol adapter.

Alternatives considered:

- Add bespoke web tools inside `codex-acp` and `kodex-claude`. This would duplicate provider clients, credentials, safety policy, caching, tool schemas, and tests, and it would need another implementation for every future managed agent.
- Extend ACP with arbitrary client-hosted tools first. That is a cleaner long-term protocol direction, but it requires changes across protocol types, runtime dispatch, streaming updates, cancellation, permissions, and both managed agents before users get working search.
- Ask users to configure their own MCP search/fetch servers. This gives quick power users an escape hatch but fails the product goal of reliable built-in web support.

### Support Brave Search and Tavily as managed search providers

Kodex will ship with Brave Search API as the default provider and Tavily as an alternate managed provider. Brave offers a general web index, fresh search results, predictable request pricing, and a simple HTTP API surface. Tavily is attractive for agent-oriented search responses and gives users a more AI-workflow-focused search option. Provider selection stays abstract so Firecrawl, Exa, or provider-hosted search can be added later without changing the agent-facing tool surface.

Alternatives considered:

- Tavily as the only provider. It is attractive because search and extraction are both available from one vendor, but it makes the default implementation more dependent on one AI-agent search vendor and less transparent as a general web index.
- Firecrawl as the only provider. It is strong for scraping/extraction, but default search should prioritize search-index quality and predictable search behavior.
- Claude/OpenAI hosted web tools. These are useful provider-specific accelerators, but they would make Codex and Claude channels behave differently and tie availability to selected model/provider.

### Keep `web_fetch` Kodex-managed by default

`web_fetch` will perform direct HTTP fetches from the Kodex-managed service, extract readable content into markdown/text, and return bounded chunks. The service will support `max_length` and `start_index` so agents can request additional chunks without loading full pages into context. Tavily Extract and other hosted extraction providers may be added as opt-in fallback when direct extraction fails, but the default fetch path should stay under Kodex's safety policy.

Alternatives considered:

- Always use a hosted extractor. This simplifies parsing but sends every URL to a third party, adds cost, and reduces control over security policy.
- Reuse the public MCP Fetch server unmodified. It demonstrates the right behavior but exposes local/internal network risk unless wrapped; Kodex needs first-class deny rules and product telemetry.

### Inject managed tools at session creation and resume

`app-core` will resolve whether web tools are enabled and whether a usable provider credential exists. `acp-core` will include the managed web MCP definition in both `session/new` and `session/load`. The injected server should be stable for a session, and sessions should reconnect with equivalent tool availability when settings remain valid.

For remote workspaces, the web service should run on the local Kodex host and be exposed to the remote agent through an HTTP endpoint reachable via existing port forwarding or a dedicated loopback tunnel. Provider keys stay local unless an explicit future remote execution mode is designed.

Alternatives considered:

- Run the web service on the remote workspace machine. This would require copying credentials and binaries to remote machines and would make network behavior dependent on the remote host.
- Inject only for new sessions. Resume behavior would be confusing because restored sessions could lose web capability until recreated.

### Treat web tools as read/search operations with network-specific policy

`web_search` and public `web_fetch` are read-only operations, but they can leak queries and URLs to third parties. Kodex will have a feature toggle and provider key setup. The service will block localhost, private IP ranges, link-local addresses, cloud metadata addresses, non-HTTP schemes, credential-bearing URLs, and redirects into blocked destinations. The tool output must be truncated and structured.

Alternatives considered:

- Reuse file/shell permission policy unchanged. Web tools need different checks because the risk is network exfiltration and SSRF rather than filesystem mutation.
- Prompt for every web search/fetch. This is safer but would make current-doc lookup noisy; a global enable toggle plus strict blocked destinations gives a better default balance.

### Present web activity through existing tool timeline

The initial UI will reuse existing tool cards. Tool mapping should prefer titles like `Search: <query>` and `Fetch: <host/path>`, and outputs should include source title, URL, snippets, page age when available, and concise markdown/text chunks. Raw JSON should remain available for debugging but not dominate the card.

Alternatives considered:

- Build a dedicated browser/search panel in the first release. This is larger than needed for the core capability and can come after tool execution semantics stabilize.
- Hide tool calls and only show final citations. Users need to trust and debug live web access, so the tool timeline should remain visible.

## Risks / Trade-offs

- [Risk] MCP adapter availability can fail independently of the agent. -> Mitigation: make startup health explicit, fail closed, and surface a clear tool-unavailable message instead of silently removing tools.
- [Risk] Web fetch can be used for SSRF or local network probing. -> Mitigation: normalize and resolve URLs, block private/local destinations before and after redirects, restrict schemes, and add tests for blocked address classes.
- [Risk] Provider keys may leak through logs or session payloads. -> Mitigation: keep keys in Kodex secrets storage, inject only local service tokens to agents, and redact web tool configuration from logs.
- [Risk] Search/fetch output can overwhelm context. -> Mitigation: hard result limits, max content length, chunking, cache, and compact default output.
- [Risk] Remote sessions may not be able to reach a local MCP endpoint. -> Mitigation: implement and test a local HTTP adapter path with port forwarding; if unavailable, disable web tools with visible setup feedback.
- [Risk] Provider cost or quota exhaustion can degrade experience. -> Mitigation: cache recent query/fetch results, expose quota errors clearly, and leave provider abstraction for alternatives.
- [Risk] Agents may overuse web tools for stable local questions. -> Mitigation: describe intended use in tool schemas and keep tool names specific; later add per-turn rate limits or model instructions if needed.

## Migration Plan

This change is additive and disabled unless web tools are enabled and a provider credential is configured. Existing sessions continue without web tools until the feature is enabled. When enabled, new and resumed managed sessions receive the injected web tool MCP server.

Rollback can disable the web tools setting or remove the managed MCP injection path. Since the tool schemas are additive and session state remains compatible, rollback should not require database migration.

## Open Questions

- Should `web_fetch` obey `robots.txt` by default for model-initiated fetches, and should user-explicit URL fetches have a different policy?
- Should hosted extraction fallback be included in the first implementation or deferred until direct fetch limitations are measured?
- How should remote workspaces expose the local MCP adapter when the remote agent cannot reach local loopback directly?
