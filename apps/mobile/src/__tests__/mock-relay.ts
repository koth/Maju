import type { RelayTransport } from "../relay/transport";

// In-memory transport pair for offline tests (mirrors relay-client's
// `ChannelTransport` + `linked_pair`). `phone.sendText` lands in `pc.recvText`
// and vice versa, so a cross-linked pair behaves as a passthrough relay with
// the two endpoints sharing the session key.
export class ChannelTransport implements RelayTransport {
  private buf: string[] = [];
  private waiters: Array<(v: string | null) => void> = [];
  private peer: ChannelTransport | null = null;
  private closed = false;

  link(peer: ChannelTransport): void {
  this.peer = peer;
  }

  async sendText(frame: string): Promise<void> {
  if (!this.peer) throw new Error("channel not linked");
  const waiter = this.peer.waiters.shift();
  if (waiter) waiter(frame);
  else this.peer.buf.push(frame);
  }

  async recvText(): Promise<string | null> {
  const queued = this.buf.shift();
  if (queued !== undefined) return queued;
  if (this.closed) return null;
  return new Promise<string | null>((resolve) => this.waiters.push(resolve));
  }

  /** Simulate a relay/peer drop: unblocks all pending recvText with null. */
  forceClose(): void {
  this.closed = true;
  while (this.waiters.length) this.waiters.shift()!(null);
  }

  async close(): Promise<void> {
  this.forceClose();
  }
}

/** Cross-link two in-memory transports: a.send -> b.recv, b.send -> a.recv. */
export function linkedPair(): [ChannelTransport, ChannelTransport] {
  const a = new ChannelTransport();
  const b = new ChannelTransport();
  a.link(b);
  b.link(a);
  return [a, b];
}
// end of file
