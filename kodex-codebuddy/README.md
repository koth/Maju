# kodex-codebuddy-proxy

An OpenAI-compatible HTTP proxy that exposes CodeBuddy's model capabilities via
[`@tencent-ai/agent-sdk`](https://www.npmjs.com/package/@tencent-ai/agent-sdk).

It is a Node/TypeScript sibling of the Rust `crates/acp-core/src/codex_api_proxy`.
Where `codex_api_proxy` forwards raw HTTP to external providers (timiai,
commandcode, deepseek, kimi, xiaomi_mimo), this proxy wraps the CodeBuddy CLI
through its official SDK and presents an **OpenAI Chat Completions** surface so
any OpenAI-compatible client can drive CodeBuddy models.

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET`  | `/healthz` | Health check (no auth). |
| `GET`  | `/v1/models` | List available CodeBuddy models (OpenAI `list` shape). |
| `POST` | `/v1/chat/completions` | Chat completion. Supports `stream: true` (SSE), `tools` (passthrough), and `X-Session-Id` reuse. |
| `DELETE` | `/v1/sessions/:id` | Explicitly close and evict a pooled session. |

## How it works

1. **Messages → prompt.** The OpenAI `messages` array is serialized into a
   single text prompt (`<system>` / `<user>` / `<assistant>` / `<tool_result>`
   blocks) and sent to the CodeBuddy SDK `query()`.
2. **Models.** `GET /v1/models` calls the SDK session's `getAvailableModels()`.
3. **Streaming.** With `stream: true`, the SDK's `RawMessageStreamEvent`
   (`content_block_delta` / `text_delta`) is converted to OpenAI
   `chat.completion.chunk` SSE frames.
4. **Tool calling (passthrough).** Client-declared `tools` are registered as an
   in-process **SDK MCP server** (`createSdkMcpServer`). The CodeBuddy model
   therefore sees real tool schemas and emits native `tool_use` content blocks.
   Each tool handler is a pending promise that **never resolves**; the proxy
   interrupts the session the instant the first assistant `tool_use` block is
   observed, so CodeBuddy's agentic loop stops at the tool boundary and the
   `tool_use` is handed back to the HTTP client. The client executes the tool
   and sends `role: "tool"` results in the next request (on the same session).
   Tool names are demangled from `mcp__proxy_tools__<name>` back to `<name>`.

## Session reuse (`X-Session-Id`)

To avoid the per-request cold start of spawning a CodeBuddy CLI process, the
proxy maintains a **session pool** (default cap **8** live sessions, idle
timeout 10 min). A conversation is pinned via the `X-Session-Id` request
header (OpenAI clients can also pass `extra_body.session_id`):

- **With `X-Session-Id`** — the proxy reuses a warm `Session` whose CLI process
  stays alive across turns. Multi-turn conversations reuse the live process and
  its conversation state (no re-stating of history needed), cutting latency to
  the model round-trip.
- **Without `X-Session-Id`** — the proxy mints a new id and still admits the
  session to the pool, so the response echoes `X-Session-Id` for the client to
  pin subsequent turns.
- **Eviction** — idle sessions are closed after `CODEBUDDY_PROXY_IDLE_TIMEOUT_MS`
  (default 600000). The pool evicts least-recently-used sessions when full.
  `DELETE /v1/sessions/:id` closes a session explicitly.
- **Concurrency** — a single session serializes turns (the CodeBuddy CLI does
  not support concurrent prompts on one session); different sessions run in
  parallel.

> Tool-schema changes across turns on the same session trigger a session
> rebuild (the CLI binds MCP tools at initialize time), so changing `tools`
> mid-conversation is supported at the cost of a re-warm.

> The proxy does **not** execute CodeBuddy's built-in tools (Bash/Read/Write).
> Built-in tools are disabled (`tools: []`) so the model only uses the
> client-declared tools.

## Setup

```bash
cd kodex-codebuddy
npm install
npm run build
```

Requires the CodeBuddy CLI to be authenticated (run `codebuddy` once to log in,
or set `CODEBUDDY_CODE_PATH` to a CLI executable).

## Run

```bash
npm start
# or with options:
CODEBUDDY_PROXY_API_KEY=secret \
CODEBUDDY_PROXY_DEFAULT_MODEL=claude-sonnet-5 \
CODEBUDDY_PROXY_PORT=17856 \
node dist/index.js
```

### Configuration (environment variables)

| Variable | Default | Description |
|---|---|---|
| `CODEBUDDY_PROXY_HOST` | `127.0.0.1` | Bind host. |
| `CODEBUDDY_PROXY_PORT` | `17856` | Bind port. |
| `CODEBUDDY_PROXY_API_KEY` | _(empty = auth disabled)_ | Shared key clients must present via `Authorization: Bearer <key>` or `x-api-key`. |
| `CODEBUDDY_PROXY_DEFAULT_MODEL` | `claude-sonnet-5` | Model used when the request omits `model`. |
| `CODEBUDDY_PROXY_CWD` | `process.cwd()` | Working directory passed to the CodeBuddy CLI. |
| `CODEBUDDY_PROXY_MAX_TURNS` | `1` | Max CodeBuddy turns per request. |
| `CODEBUDDY_PROXY_MAX_SESSIONS` | `8` | Max live pooled sessions (LRU eviction when full). |
| `CODEBUDDY_PROXY_IDLE_TIMEOUT_MS` | `600000` | Idle time (ms) before a pooled session is reaped. |
| `CODEBUDDY_PROXY_REQUEST_TIMEOUT_MS` | `120000` | Non-streaming request timeout. |
| `CODEBUDDY_PROXY_LOG_LEVEL` | `info` | `debug`/`info`/`warn`/`error`/`silent`. |

## Usage example

```bash
curl http://127.0.0.1:17856/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer secret" \
  -d '{
    "model": "claude-sonnet-5",
    "messages": [{"role": "user", "content": "Say hello"}],
    "stream": false
  }'
```

## Test

```bash
npm start         # in one terminal
node e2e-test.mjs # in another — exercises models, non-streaming, streaming, tool-call passthrough
```

## Architecture

```
HTTP client (OpenAI shape)
        │  POST /v1/chat/completions  { messages, tools, stream, X-Session-Id }
        ▼
src/server.ts            — Express routes + auth
        │
        ├─ src/codebuddy-models.ts    — GET /v1/models  (SDK getAvailableModels)
        └─ src/session-pool.ts        — acquire/reuse Session by X-Session-Id (cap 8)
                │
                └─ src/codebuddy-adapter.ts   — session.send/stream → OpenAI frames
                        ├─ src/prompt-builder.ts — messages→prompt, OpenAI tools→SDK MCP tools
                        └─ @tencent-ai/agent-sdk — Session / createSdkMcpServer()
                                │
                                ▼
                        CodeBuddy CLI (headless)  ← one process per session, reused across turns
```

### Files

- `src/config.ts` — env-based configuration.
- `src/auth.ts` — API-key middleware.
- `src/openai-types.ts` — minimal OpenAI request/response types.
- `src/prompt-builder.ts` — OpenAI→CodeBuddy prompt + SDK MCP tool mapping.
- `src/session-pool.ts` — session pool (cap 8, idle reaping, per-session serialization).
- `src/codebuddy-adapter.ts` — runs a turn on a `Session` and maps the message
  stream to OpenAI `chat.completion` / `chat.completion.chunk`.
- `src/codebuddy-models.ts` — model enumeration.
- `src/server.ts` — Express app + `X-Session-Id` routing + session endpoints.
- `src/index.ts` — entry point + teardown-race guard + pool drain on shutdown.
- `e2e-test.mjs` — end-to-end test suite.

## Notes / limitations

- Each distinct `X-Session-Id` keeps a CodeBuddy CLI process alive (one process
  per session, reused across turns). The pool caps live sessions at
  `CODEBUDDY_PROXY_MAX_SESSIONS` (default 8, LRU eviction).
- `thinking` content blocks are dropped in the OpenAI output (chat.completions
  has no standard slot for reasoning).
- Tool-call passthrough surfaces a single tool call per turn (the loop stops at
  the first `tool_use`), matching the common OpenAI client pattern.
- Changing `tools` across turns on the same session triggers a session rebuild.
