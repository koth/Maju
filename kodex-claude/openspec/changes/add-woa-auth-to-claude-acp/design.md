## Context

The repository is a TypeScript ACP adapter for the Claude Agent SDK. The executable entry point is `src/index.ts`, and the main ACP implementation is `src/acp-agent.ts`.

The existing `claude-woa` folder contains a working wrapper for Tencent WOA gateway access. It performs OAuth Device Code authentication, caches tokens in `~/.claude-woa-token.json`, refreshes tokens near expiry, selects a WOA channel, and injects WOA-specific Claude environment variables before launching the official Claude binary.

The current ACP agent already has a generic gateway auth path: clients that support `auth._meta.gateway` can call `authenticate()` and provide a base URL plus custom headers. That path is client-driven and does not manage local WOA login, token refresh, token persistence, WOA channel selection, or WOA-specific traffic flags. WOA support should build on the existing session environment injection point without replacing the current ACP session, tool, permission, diff, or terminal architecture.

## Goals / Non-Goals

**Goals:**

- Provide native TypeScript WOA OAuth login, status, refresh, token ensure, and gateway mode.
- Reuse the existing `~/.claude-woa-token.json` token shape.
- Keep WOA mode explicit through `--woa` or `CLAUDE_ACP_WOA=1`.
- Inject WOA gateway environment into each Claude Agent SDK session.
- Generate a WOA conversation id per ACP session.
- Preserve default non-WOA behavior.
- Avoid leaking access tokens, refresh tokens, and custom headers in output or logs.

**Non-Goals:**

- Do not invoke `claude-woa.sh` or `woa-auth.mjs` at runtime.
- Do not change the generic gateway auth protocol for non-WOA gateways.
- Do not make WOA the default authentication path.
- Do not implement unrelated gateway providers.
- Do not persist WOA configuration into project settings.
- Do not add `--cli --woa` behavior in the first implementation.

## Decisions

### Decision: Implement WOA as TypeScript modules under `src/woa`

Create:

```text
src/woa/auth.ts
src/woa/cli.ts
src/woa/config.ts
src/woa/error.ts
src/woa/headers.ts
src/woa/token.ts
```

Rationale: WOA support needs to participate in CLI handling, token lifecycle, safe output, and ACP session creation. Keeping the implementation in TypeScript allows it to be tested with the current Vitest setup and integrated directly into `runAcp()` and `ClaudeAcpAgent`.

Alternative considered: invoke the existing shell wrapper or Node script. Rejected because ACP startup and session creation need direct access to token ensure results, environment construction, and error handling.

### Decision: Keep WOA mode explicit

Enable WOA only through `--woa` or `CLAUDE_ACP_WOA=1`. Add process-level commands for `--woa-login`, `--woa-status`, and `--woa-refresh`.

Rationale: Existing users must not see gateway or auth behavior change unless they opt in.

Alternative considered: auto-enable WOA when `~/.claude-woa-token.json` exists. Rejected because token presence is not a reliable user intent signal.

### Decision: Preserve the existing token file format

Use `~/.claude-woa-token.json` by default and serialize fields as:

```json
{
  "accessToken": "...",
  "refreshToken": "...",
  "expiresAt": 1234567890000
}
```

Rationale: Users can reuse credentials created by `claude-woa`, and the migration path stays simple.

Alternative considered: store tokens under `~/.claude` or a package-specific directory. Rejected for the first implementation because it would require migration without improving ACP behavior.

### Decision: Use Node 18+ global `fetch`

Use global `fetch` for Device Code, token polling, and refresh calls.

Rationale: The package already targets modern Node as an ESM CLI and does not need a new HTTP dependency for form-encoded OAuth requests.

Alternative considered: add an HTTP client dependency. Rejected to keep the dependency surface small unless fetch compatibility becomes a blocker.

### Decision: Ensure tokens at startup and session creation

When WOA mode is enabled, `src/index.ts` validates token availability before starting ACP. `ClaudeAcpAgent.createSession()` also calls `ensureToken()` before constructing the Claude SDK `Options`.

Rationale: Startup validation gives an immediate actionable error, while session-time ensure keeps long-running editor processes healthy after tokens near expiry.

Alternative considered: only ensure at startup. Rejected because ACP agents may stay alive long enough for a later session to require refresh.

### Decision: Apply WOA env after process, user, and generic gateway env

Build `options.env` in this order:

```ts
{
  ...process.env,
  ...userProvidedOptions?.env,
  ...createEnvForGateway(this.gatewayAuthRequest),
  ...woaEnv,
  CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1",
}
```

Rationale: `--woa` is an explicit process-level choice, so WOA gateway URL, token, and custom headers must win over generic auth values. The ACP session state flag remains last because ACP depends on it.

Alternative considered: make generic gateway auth override WOA. Rejected because it would let a client override a locally requested WOA process mode.

### Decision: Use ACP session id as WOA conversation id

Use the ACP `sessionId` as `x-conversation-id`.

Rationale: It is already unique for new and forked sessions, stable for resumed sessions, and easy to test.

Alternative considered: generate a separate random UUID per SDK process. This remains possible later if the WOA gateway requires per-process uniqueness rather than per-ACP-session stability.

### Decision: Keep `--cli` passthrough unchanged

WOA flags are handled by the ACP executable before normal ACP startup. The existing `--cli` mode continues to pass arguments through to Claude CLI and does not implicitly apply WOA env.

Rationale: `--cli` currently means "run the underlying Claude CLI." Adding WOA behavior there changes a separate contract and creates ambiguity around argument stripping.

Alternative considered: support `--cli --woa`. Deferred to a separate change.

## Risks / Trade-offs

- WOA OAuth response shape or endpoint behavior changes -> centralize endpoint constants and response parsing, and return actionable error messages.
- Token or custom header leakage -> keep all status and error output masked, and never log raw `ANTHROPIC_CUSTOM_HEADERS`.
- `src/acp-agent.ts` is large and heavily tested -> isolate WOA modules first, then make a narrow session env injection change.
- WOA mode and generic gateway auth can both be configured -> WOA explicit mode takes precedence while generic gateway auth remains available for non-WOA use.
- Windows file permissions do not map to Unix `0600` -> apply `0600` on Unix and document that the token file is sensitive on all platforms.
- `src/index.ts` has top-level side effects -> put CLI parsing in `src/woa/cli.ts` so most behavior can be tested as pure functions.

## Migration Plan

1. Add isolated WOA modules and tests without wiring them into ACP startup.
2. Add CLI command parsing and process-level login/status/refresh commands.
3. Add optional WOA configuration to `runAcp()` and `ClaudeAcpAgent`.
4. Inject WOA env during session creation.
5. Document WOA usage in `README.md`.
6. If WOA mode causes issues, users can start the agent without `--woa` to return to existing behavior.

## Open Questions

- Should a future change add `--cli --woa` support for direct Claude CLI passthrough?
- Should WOA mode hide built-in Claude login methods by default, or only when `--hide-claude-auth` is supplied?
- Should the app version headers stay pinned to `1.1.7`, or be configurable if the WOA gateway expectations change?
