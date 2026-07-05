/** HTTP server: OpenAI-compatible surface backed by the CodeBuddy SDK. */
import express, { type Request, type Response } from 'express';
import { createAuthMiddleware } from './auth.js';
import { runNonStreaming, runStreaming, buildMcpServer, type PendingQueue } from './codebuddy-adapter.js';
import { buildPrompt } from './prompt-builder.js';
import { listModels } from './codebuddy-models.js';
import { SessionPool, toolSignatureOf } from './session-pool.js';
import { type ProxyConfig } from './config.js';
import { logger } from './logger.js';
import type { OAIChatRequest } from './openai-types.js';

export interface ServerContext {
  cfg: ProxyConfig;
  pool: SessionPool;
}

export function createApp(cfg: ProxyConfig): { app: express.Express; pool: SessionPool } {
  const pool = new SessionPool(
    { maxSessions: cfg.maxSessions, idleTimeoutMs: cfg.idleTimeoutMs },
    { cwd: cfg.cwd },
  );
  const app = express();
  app.use(express.json({ limit: '16mb' }));

  // Health check (must be registered BEFORE the auth middleware so callers
  // can probe the proxy without a key — used by the Tauri lifecycle
  // manager and by external liveness checks).
  app.get('/healthz', (_req: Request, res: Response) => {
    res.json({ status: 'ok', version: VERSION, poolSize: pool.size });
  });

  app.use(createAuthMiddleware({ apiKey: cfg.apiKey }));

  // GET /v1/models
  app.get('/v1/models', async (_req: Request, res: Response) => {
    try {
      const models = await listModels();
      res.json(models);
    } catch (err) {
      logger.error('listModels failed: %s', err instanceof Error ? err.stack ?? err.message : String(err));
      res.status(500).json({ error: { message: 'Failed to enumerate models', type: 'server_error' } });
    }
  });

  /** Resolve the session id from X-Session-Id (or OpenAI extra_body fallback). */
  function resolveSessionId(req: Request): string | undefined {
    const h = req.headers['x-session-id'];
    if (typeof h === 'string' && h.trim()) return h.trim();
    const extra = (req.body as { extra_body?: { session_id?: string } } | undefined)?.extra_body;
    if (extra && typeof extra.session_id === 'string' && extra.session_id.trim()) {
      return extra.session_id.trim();
    }
    return undefined;
  }

  // POST /v1/chat/completions
  app.post('/v1/chat/completions', async (req: Request, res: Response) => {
    const body = req.body as OAIChatRequest;
    if (!body || !Array.isArray(body.messages) || body.messages.length === 0) {
      res.status(400).json({ error: { message: 'messages must be a non-empty array', type: 'invalid_request_error' } });
      return;
    }
    const stream = body.stream === true;
    const adapterOpts = {
      defaultModel: cfg.defaultModel,
      maxTurns: cfg.maxTurns,
      cwd: cfg.cwd,
    };

    // Extract system prompt so it can be passed via SDK SessionOptions.systemPrompt
    // instead of being mixed into the user message text.
    const { systemPrompt } = buildPrompt(body, {
      defaultModel: cfg.defaultModel,
      maxTurns: cfg.maxTurns,
    });

    // Resolve (or mint) a session id and acquire a live Session from the pool.
    const requestedId = resolveSessionId(req);
    const sessionId = requestedId || randomSessionId();
    const model = (body.model && body.model.trim()) || cfg.defaultModel;
    const toolSig = toolSignatureOf(body.tools);
    const pending: PendingQueue = { handlers: [] };
    const mcp = buildMcpServer(body, pending);

    let entry: Awaited<ReturnType<typeof pool.acquire>>;
    try {
      entry = await pool.acquire(sessionId, {
        cwd: cfg.cwd,
        permissionMode: 'bypassPermissions',
        model,
        maxTurns: cfg.maxTurns,
        mcpServers: mcp ? { [PROXY_TOOL_SERVER_NAME_KEY]: mcp } : undefined,
        buildMcp: (p) => buildMcpServer(body, p),
        systemPrompt,
        env: {
          CODEBUDDY_API_KEY: process.env.CODEBUDDY_API_KEY,
          CODEBUDDY_INTERNET_ENVIRONMENT: process.env.CODEBUDDY_INTERNET_ENVIRONMENT,
        },
      }, toolSig);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      logger.error('session acquire failed: %s', detail);
      res.status(500).json({ error: { message: 'session setup failed: ' + detail, type: 'server_error' } });
      return;
    }

    // Use the entry's persistent pending object (bound to the session's
    // MCP server at creation time) instead of the per-request one.
    const sessionPending = entry.pending;
    const sessionIsNew = entry.isNew;

    // Echo the session id so stateless clients can pin subsequent turns.
    res.setHeader('X-Session-Id', sessionId);

    if (stream) {
      res.setHeader('Content-Type', 'text/event-stream');
      res.setHeader('Cache-Control', 'no-cache');
      res.setHeader('Connection', 'keep-alive');
      res.setHeader('X-Accel-Buffering', 'no');
      res.flushHeaders?.();
      try {
        await pool.serialize(sessionId, async (session) => {
          for await (const frame of runStreaming(body, session, adapterOpts, sessionPending, sessionIsNew)) {
            res.write(frame);
          }
        });
      } catch (err) {
        logger.error('stream write failed: %s', err instanceof Error ? err.message : String(err));
        res.write(`data: ${JSON.stringify({ error: { message: 'stream failed', type: 'server_error' } })}\n\n`);
        res.write('data: [DONE]\n\n');
      } finally {
        res.end();
      }
      return;
    }

    // Non-streaming.
    try {
      const resp = await pool.serialize(sessionId, (session) =>
        runNonStreaming(body, session, adapterOpts, sessionPending, sessionIsNew),
      );
      res.json(resp);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      logger.error('completion failed: %s', err instanceof Error ? err.stack ?? detail : detail);
      res.status(500).json({ error: { message: 'completion failed: ' + detail, type: 'server_error' } });
    }
  });

  // DELETE /v1/sessions/:id — explicitly close and evict a session.
  app.delete('/v1/sessions/:id', async (req: Request, res: Response) => {
    const id = req.params.id;
    await pool.evict(id);
    res.json({ ok: true, sessionId: id });
  });

  return { app, pool };
}

const VERSION = '0.1.0';
const PROXY_TOOL_SERVER_NAME_KEY = 'proxy_tools';

function randomSessionId(): string {
  return 'ps-' + Math.random().toString(36).slice(2) + Date.now().toString(36);
}
