## 1. WOA Module Skeleton

- [x] 1.1 Create `src/woa/config.ts`, `token.ts`, `auth.ts`, `headers.ts`, `error.ts`, `cli.ts`, and `index.ts`.
- [x] 1.2 Export WOA public types and helpers needed by `src/index.ts`, `src/acp-agent.ts`, and tests.
- [x] 1.3 Add typed WOA errors for invalid channel, missing token, malformed token, missing refresh token, OAuth request failure, token polling failure, refresh failure, and file I/O failure.

## 2. Configuration And CLI Parsing

- [x] 2.1 Implement `WoaChannel` and `WoaConfig` parsing with defaults for disabled mode, `default` channel, and `~/.claude-woa-token.json`.
- [x] 2.2 Support `--woa`, `--woa-channel`, `--woa-token-path`, `CLAUDE_ACP_WOA`, `CLAUDE_WOA_CHANNEL`, and `CLAUDE_WOA_TOKEN_PATH`.
- [x] 2.3 Implement process command detection for `--woa-login`, `--woa-status`, and `--woa-refresh`.
- [x] 2.4 Add tests for CLI/env precedence, invalid channel, custom token path, and command detection.

## 3. Token Storage

- [x] 3.1 Implement `WoaToken` validation for `accessToken`, optional `refreshToken`, and millisecond `expiresAt`.
- [x] 3.2 Implement async token load, save, parent directory creation, and atomic rewrite behavior.
- [x] 3.3 Apply Unix `0600` permissions when saving token files.
- [x] 3.4 Implement expiration threshold and masked secret helpers.
- [x] 3.5 Add tests for token roundtrip, missing file, malformed file, expiring-soon behavior, and secret masking.

## 4. OAuth Flow

- [x] 4.1 Implement WOA device code request using Node global `fetch`.
- [x] 4.2 Implement device token polling with `authorization_pending`, `slow_down`, success, failure, and expiry handling.
- [x] 4.3 Implement refresh token request with existing-refresh-token fallback when the response omits a new refresh token.
- [x] 4.4 Implement `ensureToken` to return a valid token or refresh when it expires within five minutes.
- [x] 4.5 Add mock-fetch tests for device code, polling, refresh, and ensure-token branches.

## 5. Header And Environment Injection

- [x] 5.1 Implement WOA gateway URL selection for `default` and `offline`.
- [x] 5.2 Implement `buildWoaCustomHeaders` with the full WOA-required header set.
- [x] 5.3 Implement `buildWoaEnv` for gateway URL, auth token, custom headers, and nonessential traffic disable flags.
- [x] 5.4 Use ACP `sessionId` as the WOA `x-conversation-id`.
- [x] 5.5 Add tests for required headers, channel header, conversation id, gateway URL, and environment variables.

## 6. Entry Point Integration

- [x] 6.1 Modify `src/index.ts` to execute `--woa-login`, `--woa-status`, and `--woa-refresh` before ACP startup.
- [x] 6.2 Modify `src/index.ts` to parse WOA config and call `ensureToken` before `runAcp()` when WOA mode is enabled.
- [x] 6.3 Keep existing `--cli` passthrough behavior unchanged.
- [x] 6.4 Add tests around the extracted CLI parsing and command dispatch helpers.

## 7. ACP Agent Integration

- [x] 7.1 Extend `ClaudeAcpAgent` construction with backward-compatible optional WOA configuration.
- [x] 7.2 Extend `runAcp()` to accept optional WOA configuration and pass it to the agent.
- [x] 7.3 Inject WOA env into Claude Agent SDK `Options.env` during `createSession()` after process env, user env, and generic gateway env.
- [x] 7.4 Ensure session creation calls `ensureToken` again when WOA mode is enabled.
- [x] 7.5 Add WOA terminal auth method when WOA mode is enabled and the client supports terminal auth.
- [x] 7.6 Preserve model, small-fast-model, thinking-token, permission, tool, terminal, plan, load, resume, fork, and cancel behavior.

## 8. Safety And Documentation

- [x] 8.1 Audit WOA command output and errors to ensure full tokens and raw custom headers are never printed.
- [x] 8.2 Update `README.md` with WOA login, status, refresh, startup, channel, token path, environment variable, security, and compliance notes.
- [x] 8.3 Export any WOA library types from `src/lib.ts` only if needed for public API usage.

## 9. Verification

- [x] 9.1 Run `npm run build`.
- [x] 9.2 Run `npm run lint`.
- [ ] 9.3 Run `npm run format:check`.
- [x] 9.4 Run `npm run test:run`.
- [ ] 9.5 Manually verify `node dist/index.js --woa-login`, `--woa-status`, and `--woa-refresh` in a WOA-capable environment.
- [ ] 9.6 Manually verify an ACP client can use WOA mode for prompt, Read, Edit, Write, Bash, permission, cancel, Plan mode, session list, load, resume, and fork workflows.
