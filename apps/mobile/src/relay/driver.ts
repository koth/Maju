import type { RelayConnection } from "./connection";
import type { ControlClient } from "../session/control-client";
import { nextBackoffDelay } from "./backoff";
import type { Envelope, EventFrame, ControlResponse } from "../types/relay-protocol";

export type EventSink = (frame: EventFrame) => void;
export type OtherSink = (env: Envelope) => void;

/**
 * Phone receive loop: dispatch ControlResponse to the control client and route
 * EventFrame to the event sink. Other message types (subscription_status,
 * pairing_confirm, bind_device_response) go to `onOther`. Fail-open: a
 * connection error or clean close ends the loop without throwing — the
 * ConnectionManager decides whether to reconnect.
 */
export async function runReceiveLoop(
  conn: RelayConnection,
  controlClient: ControlClient,
  onEvent: EventSink,
  onOther?: OtherSink,
  shouldStop?: () => boolean,
): Promise<"closed" | "stopped"> {
  while (!shouldStop?.()) {
  let env: Envelope | null;
  try {
  env = await conn.recvEnvelope();
  } catch {
  return "closed";
  }
  if (env === null) return "closed";
  if (env.type === "control_response") {
  controlClient.dispatchResponse(env.payload as ControlResponse);
  } else if (env.type === "event") {
  onEvent(env.payload as EventFrame);
  } else if (onOther) {
  onOther(env);
  }
  }
  return "stopped";
}

export interface ReconnectLoopOptions {
  /** Attempt one connection + run the receive loop. Resolves on close/error. */
  connect: () => Promise<void>;
  shouldStop?: () => boolean;
  onBackoff?: (attempt: number, delayMs: number) => void;
  /** Test hook: override the sleep function (defaults to real setTimeout). */
  sleep?: (ms: number) => Promise<void>;
  maxAttempts?: number;
}

/**
 * Reconnect loop with exponential backoff + jitter (2s -> 60s cap). Calls
 * `connect` repeatedly until `shouldStop` or `maxAttempts` is reached. Never
 * throws — fail-open at the app level (broken relay never crashes the app).
 */
export async function reconnectLoop(opts: ReconnectLoopOptions): Promise<void> {
  const sleep = opts.sleep ?? ((ms: number) => new Promise((r) => setTimeout(r, ms)));
  let attempt = 0;
  while (!opts.shouldStop?.()) {
  try {
  await opts.connect();
  attempt = 0; // reset on a (cleanly ended) successful connection
  } catch {
  // connection failed; backoff and retry
  }
  if (opts.shouldStop?.()) return;
  if (opts.maxAttempts !== undefined && attempt >= opts.maxAttempts) return;
  const delay = nextBackoffDelay(attempt);
  opts.onBackoff?.(attempt, delay);
  attempt += 1;
  await sleep(delay);
  }
}
// end of file
