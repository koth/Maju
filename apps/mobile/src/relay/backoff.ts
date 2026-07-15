// Exponential backoff + jitter for reconnect, 2s -> 60s cap (mirrors the PC
// driver's backoff curve). Stateless helpers so the driver owns the attempt
// counter and can reset it on success.
export const BACKOFF_INITIAL_MS = 2_000;
export const BACKOFF_MAX_MS = 60_000;

/** Next delay after `attempt` failed reconnects (0-based). */
export function nextBackoffDelay(
  attempt: number,
  initialMs: number = BACKOFF_INITIAL_MS,
  maxMs: number = BACKOFF_MAX_MS,
  now: () => number = Date.now,
): number {
  const exp = initialMs * 2 ** attempt;
  const base = Math.min(exp, maxMs);
  const jitter = (now() % 1000) / 1000; // [0,1)
  const factor = 0.75 + jitter * 0.5;
  return Math.min(Math.round(base * factor), maxMs);
}
// end of file
