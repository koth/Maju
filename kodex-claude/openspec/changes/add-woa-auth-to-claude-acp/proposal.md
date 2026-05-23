## Why

`claude-woa` proves that Tencent WOA gateway access works by wrapping the official Claude binary with OAuth Device Code authentication, token refresh, and gateway headers. Current `@agentclientprotocol/claude-agent-acp` users cannot use that flow natively from ACP clients, and the existing gateway auth path depends on a client-provided `authenticate` request rather than local WOA token lifecycle management.

This change makes WOA access a first-class, explicit mode in the TypeScript ACP agent while preserving existing non-WOA behavior by default.

## What Changes

- Add explicit WOA gateway mode to the TypeScript ACP agent, enabled by `--woa` or `CLAUDE_ACP_WOA=1`.
- Add native WOA OAuth Device Code commands:
  - `--woa-login`
  - `--woa-status`
  - `--woa-refresh`
- Add WOA channel selection for `default` and `offline`.
- Add WOA token cache support compatible with the existing `~/.claude-woa-token.json` format.
- Automatically ensure and refresh WOA tokens before ACP startup and before WOA-backed session creation.
- Inject WOA gateway environment into Claude Agent SDK sessions, including base URL, auth token, custom headers, and nonessential traffic disable flags.
- Generate a WOA conversation id per ACP session.
- Keep the existing generic gateway auth mechanism available for non-WOA custom gateways.
- Add tests and README documentation for WOA gateway mode.

## Capabilities

### New Capabilities

- `woa-auth-gateway`: Native WOA authentication, token management, channel selection, safe status output, and per-session gateway environment injection for the TypeScript Claude ACP agent.

### Modified Capabilities

None.

## Impact

- Affected code:
  - `src/index.ts`
  - `src/acp-agent.ts`
  - `src/lib.ts`
  - new `src/woa/*` module files
  - `src/tests/*`
  - `README.md`
- API surface:
  - New CLI flags and environment variables.
  - `runAcp()` and `ClaudeAcpAgent` gain optional WOA configuration while remaining backward compatible.
- Dependencies:
  - No new runtime dependency is required; use Node 18+ global `fetch`.
- Operational impact:
  - WOA mode requires access to `copilot.code.woa.com`.
  - WOA mode stores sensitive credentials in the token file and must redact secrets in output and logs.
  - Default non-WOA ACP behavior remains unchanged.
