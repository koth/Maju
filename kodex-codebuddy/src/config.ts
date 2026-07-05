/**
 * Proxy configuration resolved from environment variables.
 */
export interface ProxyConfig {
  /** Host/port the HTTP server binds to. */
  host: string;
  port: number;
  /** Shared API key clients must present. Empty => auth disabled. */
  apiKey: string;
  /** Default model when the client omits `model` in the request. */
  defaultModel: string;
  /** Working directory passed to the CodeBuddy CLI. */
  cwd: string;
  /** Max turns for the CodeBuddy query (1 = single assistant response). */
  maxTurns: number;
  /** Max live pooled sessions (LRU eviction when full). */
  maxSessions: number;
  /** Idle time (ms) before a pooled session is reaped. */
  idleTimeoutMs: number;
  /** Per-request timeout in ms for non-streaming responses. */
  requestTimeoutMs: number;
  /** Log level: 'debug' | 'info' | 'warn' | 'error' | 'silent'. */
  logLevel: string;
}

function env(name: string, fallback = ''): string {
  const v = process.env[name];
  return v === undefined || v === '' ? fallback : v;
}

function envInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const n = Number(raw);
  return Number.isFinite(n) ? n : fallback;
}

export function loadConfig(): ProxyConfig {
  return {
    host: env('CODEBUDDY_PROXY_HOST', '127.0.0.1'),
    port: envInt('CODEBUDDY_PROXY_PORT', 17856),
    apiKey: env('CODEBUDDY_PROXY_API_KEY'),
    defaultModel: env('CODEBUDDY_PROXY_DEFAULT_MODEL', 'claude-sonnet-5'),
    cwd: env('CODEBUDDY_PROXY_CWD', process.cwd()),
    maxTurns: envInt('CODEBUDDY_PROXY_MAX_TURNS', 1),
    maxSessions: envInt('CODEBUDDY_PROXY_MAX_SESSIONS', 8),
    idleTimeoutMs: envInt('CODEBUDDY_PROXY_IDLE_TIMEOUT_MS', 600_000),
    requestTimeoutMs: envInt('CODEBUDDY_PROXY_REQUEST_TIMEOUT_MS', 120_000),
    logLevel: env('CODEBUDDY_PROXY_LOG_LEVEL', 'info'),
  };
}
