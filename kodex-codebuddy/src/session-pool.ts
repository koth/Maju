/**
 * Session pool: keeps CodeBuddy SDK `Session` instances alive across HTTP
 * requests so that multi-turn conversations reuse a warm CLI process instead
 * of spawning a new one per request.
 *
 * Selection: the HTTP client pins a conversation via the `X-Session-Id`
 * header. The pool looks it up; on a hit it reuses the live session, on a
 * miss (or when no id is supplied) it creates a new session and admits it to
 * the pool (subject to the max-sessions cap).
 *
 * Eviction: idle sessions are closed after `idleTimeoutMs` and the pool never
 * holds more than `maxSessions` entries (LRU on admission when full).
 *
 * Concurrency: a single CodeBuddy session does not support concurrent turns,
 * so each entry owns a promise-chain that serializes send/stream operations.
 */
import { unstable_v2_createSession, type Session } from '@tencent-ai/agent-sdk';
import type { PendingQueue } from './codebuddy-adapter.js';
import { logger } from './logger.js';

export interface PoolSessionOptions {
  /** Working directory for the CodeBuddy CLI. */
  cwd: string;
  /** Permission mode (proxy always bypasses). */
  permissionMode: 'bypassPermissions';
  /** Model to initialize the session with. */
  model: string;
  /** Maximum conversation turns (1 = stop after one model call). */
  maxTurns?: number;
  /** SDK MCP servers to register (built-in tools disabled; client tools only). */
  mcpServers?: Record<string, unknown>;
  /** System prompt passed via SDK `SessionOptions.systemPrompt` — the proper
   *  system-prompt channel instead of mixing system text into the user message. */
  systemPrompt?: string;
  /** Environment variables forwarded to the CodeBuddy CLI session
   *  (`CODEBUDDY_API_KEY`, `CODEBUDDY_INTERNET_ENVIRONMENT`, …). */
  env?: Record<string, string | undefined>;
  /** Callback to build the SDK MCP server using the entry's persistent
   *  pending object. Called only at session creation time. */
  buildMcp?: BuildMcpCallback;
}
/** Callback to build the SDK MCP server using the entry's persistent
 *  pending queue. Called only at session creation time. */
export type BuildMcpCallback = (pending: PendingQueue) => unknown | undefined;

interface Entry {
  session: Session;
  sessionId: string;
  /** Monotonic timestamp (ms) of last activity. */
  lastUsed: number;
  /** Serializes turns: a chain of promises so only one send/stream runs. */
  tail: Promise<unknown>;
  /** Snapshot of tool names registered at creation (for change detection). */
  toolSignature: string;
  /** Whether the underlying CLI process is still alive. */
  closed: boolean;
  /** Shared pending handler queue — bound to the SDK MCP server
   *  at session creation time. Reused across requests on the same session
   *  so the handler-captured input is visible to runStreaming. */
  pending: PendingQueue;
  /** True when this entry was created in the current acquire call
   *  (fresh session, no prior CLI history). False when reused. */
  isNew: boolean;
}

export interface SessionPoolConfig {
  /** Maximum number of live sessions the pool will hold. */
  maxSessions: number;
  /** Idle time (ms) after which a session is closed and evicted. */
  idleTimeoutMs: number;
  /** How often (ms) the reaper scans for idle sessions. */
  reapIntervalMs: number;
}

export const DEFAULT_POOL_CONFIG: SessionPoolConfig = {
  maxSessions: 8,
  idleTimeoutMs: 10 * 60 * 1000, // 10 minutes
  reapIntervalMs: 60 * 1000, // 1 minute
};

export class SessionPool {
  private readonly entries = new Map<string, Entry>();
  private readonly cfg: SessionPoolConfig;
  private readonly baseOptions: { cwd: string };
  private reaper: NodeJS.Timeout | null = null;

  constructor(
    cfg: Partial<SessionPoolConfig> = {},
    baseOptions: { cwd: string },
  ) {
    this.cfg = { ...DEFAULT_POOL_CONFIG, ...cfg };
    this.baseOptions = baseOptions;
    this.startReaper();
  }

  /** Current number of live sessions. */
 get size(): number {
    return this.entries.size;
  }

  /**
   * Acquire a session for the given client id, creating one if missing.
   * Returns the entry plus a function that enqueues a serialized operation.
   */
  async acquire(
    sessionId: string,
    opts: PoolSessionOptions,
    toolSignature: string,
  ): Promise<Entry> {
    let entry = this.entries.get(sessionId);
    logger.info(
      'acquire sessionId=%s existing=%s closed=%s toolSig_match=%s',
      sessionId,
      !!entry,
      entry?.closed ?? 'n/a',
      entry ? entry.toolSignature === toolSignature : 'n/a',
    );
    if (entry && !entry.closed) {
      entry.lastUsed = Date.now();
      // Tool schema changed since creation? Caller decides to rebuild.
      if (entry.toolSignature !== toolSignature) {
        logger.info('session %s tool signature changed — rebuilding session', sessionId);
        await this.evict(sessionId);
        entry = undefined;
      }
    }
    let isNew = false;
    if (!entry || entry.closed) {
      entry = await this.create(sessionId, opts, toolSignature);
      isNew = true;
    }
    entry.isNew = isNew;
    return entry;
  }

  /** Create and admit a new session, honoring the max-sessions cap (LRU). */
  private async create(
    sessionId: string,
    opts: PoolSessionOptions,
    toolSignature: string,
  ): Promise<Entry> {
    if (this.entries.size >= this.cfg.maxSessions) {
      // Evict the least-recently-used live entry.
      const lru = [...this.entries.values()]
        .filter((e) => !e.closed)
        .sort((a, b) => a.lastUsed - b.lastUsed)[0];
      if (lru) {
        logger.info('pool full (%d) — evicting LRU session %s', this.entries.size, lru.sessionId);
        await this.evict(lru.sessionId);
      }
    }
    // Create the persistent pending queue first so buildMcp can bind
    // SDK MCP handlers to it. This same queue is stored on the entry
    // and reused across requests on the same session.
    const pending: PendingQueue = { handlers: [] };
    const mcp = opts.buildMcp ? opts.buildMcp(pending) : undefined;
    const mcpServers = mcp ? { proxy_tools: mcp } : opts.mcpServers;
    const session = unstable_v2_createSession({
      cwd: opts.cwd,
      permissionMode: opts.permissionMode,
      model: opts.model,
      tools: [],
      includePartialMessages: true,
      mcpServers: mcpServers as never,
      sessionId,
      systemPrompt: opts.systemPrompt,
      maxTurns: opts.maxTurns,
      env: opts.env,
    });
    const entry: Entry = {
      session,
      sessionId,
      lastUsed: Date.now(),
      tail: Promise.resolve(),
      toolSignature,
      closed: false,
      pending,
      isNew: true,
    };
    this.entries.set(sessionId, entry);
    logger.info('created session %s (pool size %d)', sessionId, this.entries.size);
    return entry;
  }

  /**
   * Run an operation serialized against the session's turn queue.
   * The session's agentic loop is single-threaded; this prevents two
   * concurrent HTTP requests on the same id from interleaving send/stream.
   */
  serialize<T>(sessionId: string, fn: (session: Session) => Promise<T>): Promise<T> {
    const entry = this.entries.get(sessionId);
    if (!entry || entry.closed) {
      return Promise.reject(new Error(`session ${sessionId} not found`));
    }
    entry.lastUsed = Date.now();
    const run = entry.tail.then(
      () => fn(entry.session),
      () => fn(entry.session),
    );
    // Keep the chain alive even if the op rejects.
    entry.tail = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  /** Explicitly close and remove a session. */
  async evict(sessionId: string): Promise<void> {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    this.entries.delete(sessionId);
    if (!entry.closed) {
      entry.closed = true;
      try {
        entry.session.close();
      } catch (err) {
        logger.debug('session %s close error: %s', sessionId, err instanceof Error ? err.message : String(err));
      }
    }
    logger.info('evicted session %s (pool size %d)', sessionId, this.entries.size);
  }

  /** Close all sessions (used on shutdown). */
  async drain(): Promise<void> {
    this.stopReaper();
    const ids = [...this.entries.keys()];
    await Promise.all(ids.map((id) => this.evict(id)));
  }

  private startReaper(): void {
    if (this.reaper) return;
    this.reaper = setInterval(() => this.reap(), this.cfg.reapIntervalMs);
    if (this.reaper && typeof this.reaper.unref === 'function') {
      this.reaper.unref();
    }
  }

  private stopReaper(): void {
    if (this.reaper) {
      clearInterval(this.reaper);
      this.reaper = null;
    }
  }

  private reap(): void {
    const now = Date.now();
    for (const [id, entry] of this.entries) {
      if (entry.closed) {
        this.entries.delete(id);
        continue;
      }
      if (now - entry.lastUsed > this.cfg.idleTimeoutMs) {
        logger.info('reaping idle session %s (idle %dms)', id, now - entry.lastUsed);
        void this.evict(id);
      }
    }
  }
}

/** Build a stable signature string from an OpenAI tool list (for change detection). */
export function toolSignatureOf(tools: unknown): string {
  if (!Array.isArray(tools) || tools.length === 0) return '';
  try {
    return JSON.stringify(tools.map((t) => (t as { function?: { name?: string } })?.function?.name).sort());
  } catch {
    return '';
  }
}
